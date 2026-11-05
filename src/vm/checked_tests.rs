//! An internal error reports a VM invariant that failed. The tests corrupt a
//! compiled program's bytecode so that an instruction indexes a table out of
//! range, and check that the host sees the internal error whatever handlers
//! the script wraps around the failing instruction and whatever a host
//! callback does with it.

use super::*;
use crate::{CallOptions, Engine, HostMethod, Script};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Lets `edit` change the code of `function` in a compiled script. The code
/// is sealed in an `Arc` with a weak reference to itself, so it is unwrapped,
/// edited and sealed again.
fn corrupt(script: Script, function: &str, edit: impl FnOnce(&mut [Op])) -> Script {
    let crate::ScriptInner {
        code,
        loader,
        strict_effects,
        random_source,
        output_writer,
        error_writer,
    } = Arc::try_unwrap(script.inner).ok().unwrap();
    let mut code = Arc::try_unwrap(code).ok().unwrap();
    let function = code
        .program
        .functions
        .iter_mut()
        .find(|candidate| candidate.name == function)
        .unwrap();
    edit(function.code.as_mut_slice());
    let code = Arc::new(code);
    Script {
        inner: Arc::new(crate::ScriptInner {
            code,
            loader,
            strict_effects,
            random_source,
            output_writer,
            error_writer,
        }),
    }
}

/// Points the first constant load past the constant table.
fn constant_out_of_range(code: &mut [Op]) {
    let constant = code
        .iter_mut()
        .find(|op| matches!(op, Op::Constant(_)))
        .unwrap();
    *constant = Op::Constant(u32::MAX);
}

/// Points the first attached block past the function table.
fn block_out_of_range(code: &mut [Op]) {
    let attach = code
        .iter_mut()
        .find(|op| matches!(op, Op::Attach(_)))
        .unwrap();
    *attach = Op::Attach(u32::MAX);
}

fn is_internal(error: &Error) -> bool {
    error.kind == ErrorKind::Internal
        && error.class().is_none()
        && error.message.contains("index 4294967295 is out of range")
}

fn internal_error(source: &str) {
    let script = corrupt(
        Engine::new().compile(source).unwrap(),
        "run",
        constant_out_of_range,
    );
    let error = script
        .call("run", &[], CallOptions::default())
        .expect_err("a corrupted constant index must fail the call");
    assert!(is_internal(&error), "{error}");
}

#[test]
fn a_corrupted_constant_index_is_an_internal_error() {
    internal_error("def run -> int\n123456\nend");
}

#[test]
fn rescue_does_not_catch_an_internal_error() {
    internal_error("def run -> int\nbegin\n123456\nrescue\n42\nend\nend");
}

#[test]
fn ensure_cannot_return_past_an_internal_error() {
    internal_error("def run -> int\nbegin\n123456\nensure\nreturn 42\nend\nend");
}

#[test]
fn ensure_cannot_replace_an_internal_error() {
    internal_error(
        "def run -> int\nbegin\nbegin\n123456\nensure\nraise \"replacement\"\nend\nrescue\n42\nend\nend",
    );
}

/// A block call whose target is corrupted fails while the block is prepared,
/// before the run loop, and hands the error to the host callback.
const GUARDED_BLOCK: &str =
    "def run -> any\nbegin\nvisit { 123456 }\nrescue\nmark()\n42\nensure\nmark()\nend\nend";

/// What the callback does with the error its block call returned.
#[derive(Clone, Copy, Debug)]
enum Reaction {
    Ignore,
    Replace,
}

impl Reaction {
    fn result(self) -> Result<Value> {
        match self {
            Self::Ignore => Ok(Value::int(99)),
            Self::Replace => Err(Error::new(ErrorKind::Runtime, "replacement")),
        }
    }
}

/// What the host saw: the block call's error, the error of later work on the
/// context, and how often the script's rescue or ensure ran.
#[derive(Default)]
struct Observed {
    block: AtomicBool,
    later: AtomicBool,
    handlers: AtomicUsize,
}

impl Observed {
    fn check(&self, result: Result<()>, reaction: Reaction) {
        self.check_terminal(result, reaction);
        self.check_callback(reaction);
    }

    fn check_terminal(&self, result: Result<()>, reaction: Reaction) {
        let error = result.expect_err("the corrupted block target must fail the call");
        assert!(is_internal(&error), "{reaction:?}: {error}");
        assert_eq!(self.handlers.load(Ordering::Relaxed), 0, "{reaction:?}");
    }

    fn check_callback(&self, reaction: Reaction) {
        assert!(self.block.load(Ordering::Relaxed), "{reaction:?}");
        assert!(self.later.load(Ordering::Relaxed), "{reaction:?}");
    }
}

fn marking(observed: &Arc<Observed>) -> Engine {
    let mut engine = Engine::new();
    let marks = observed.clone();
    engine.register("mark", move |_, _| {
        marks.handlers.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    engine
}

fn guarded_block(reaction: Reaction) -> (Script, Arc<Observed>) {
    let observed = Arc::new(Observed::default());
    let mut engine = marking(&observed);
    let seen = observed.clone();
    engine.register_method(
        "visit",
        HostMethod::new_with_block("visit", move |call, _, _| {
            let block = call.call_block(&[]);
            seen.block
                .store(block.is_err_and(|e| is_internal(&e)), Ordering::Relaxed);
            let later = call.context().checkpoint();
            seen.later
                .store(later.is_err_and(|e| is_internal(&e)), Ordering::Relaxed);
            reaction.result()
        }),
    );
    let script = corrupt(
        engine.compile(GUARDED_BLOCK).unwrap(),
        "run",
        block_out_of_range,
    );
    (script, observed)
}

#[test]
fn a_callback_cannot_ignore_or_replace_an_internal_block_error() {
    for reaction in [Reaction::Ignore, Reaction::Replace] {
        let (script, observed) = guarded_block(reaction);
        let result = script.call("run", &[], CallOptions::default());
        observed.check(result.map(drop), reaction);
    }
}

#[test]
fn an_unresolved_invocation_is_terminal_even_in_a_handler() {
    for body in [
        "target(*[1])",
        "begin\ntarget(*[1])\nrescue\n2\nend",
        "begin\ntarget(*[1])\nensure\nreturn 2\nend",
    ] {
        let source = format!("def target(n: int) -> int\nn\nend\ndef run -> int\n{body}\nend");
        let script = corrupt(Engine::new().compile(&source).unwrap(), "run", |code| {
            let root = code
                .iter_mut()
                .find(|op| matches!(op, Op::RootCall(..)))
                .unwrap();
            *root = Op::Arguments;
            let invoke = code
                .iter_mut()
                .find(|op| matches!(op, Op::InvokeRoot(_)))
                .unwrap();
            *invoke = Op::Invoke(Invocation::Resolved);
        });
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal, "{body}: {error}");
        assert!(error.class().is_none());
        assert!(
            error.message.starts_with("internal error at src/vm.rs:"),
            "{error}"
        );
    }
}

#[test]
fn a_callback_cannot_swallow_an_internal_import_error() {
    for reaction in [Reaction::Ignore, Reaction::Replace] {
        let observed = Arc::new(Observed::default());
        let mut engine = marking(&observed);
        let seen = observed.clone();
        engine.register("visit", move |ctx, _| {
            // The second record enters the shared-key path with a malformed
            // container key, which scalar import must reject as an invariant error.
            let record = || Value::hash(vec![(b"key".to_vec(), Value::nil())]);
            let mut malformed = record();
            if let Kind::Hash(hash) = &mut malformed.0 {
                Arc::get_mut(hash).unwrap().buffer.data[0].0 = Value::array(vec![]);
            }
            let imported = ctx.import(&Value::array(vec![record(), malformed]));
            seen.block.store(
                imported.is_err_and(|e| e.kind == ErrorKind::Internal),
                Ordering::Relaxed,
            );
            seen.later.store(
                ctx.checkpoint()
                    .is_err_and(|e| e.kind == ErrorKind::Internal),
                Ordering::Relaxed,
            );
            reaction.result()
        });
        let script = engine
            .compile("def run -> any\nbegin\nvisit()\nrescue\nmark()\nensure\nmark()\nend\nend")
            .unwrap();
        let result = script.call("run", &[], CallOptions::default());
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal, "{reaction:?}: {error}");
        assert!(observed.block.load(Ordering::Relaxed));
        assert!(observed.later.load(Ordering::Relaxed));
        assert_eq!(observed.handlers.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn missing_vm_operands_are_terminal_in_host_blocks() {
    for op in [
        Op::Pop,
        Op::Dup,
        Op::LoopBody,
        Op::EnsureEnd,
        Op::Return,
        Op::TextPart,
        Op::TextEnd(false),
        Op::Range(true, false, false),
        Op::Range(false, true, false),
        Op::RaiseValue,
        Op::Yield(0),
        Op::Bind(0, 0),
        Op::Attach(0),
        Op::LoopEnd,
        Op::AddressStore,
        Op::AddressDrop,
        Op::NamespaceConstant(0, 0),
    ] {
        for reaction in [Reaction::Ignore, Reaction::Replace] {
            let observed = Arc::new(Observed::default());
            let mut engine = marking(&observed);
            let seen = observed.clone();
            engine.register_method(
                "visit",
                HostMethod::new_with_block("visit", move |call, _, _| {
                    let error = call.call_block(&[]).unwrap_err();
                    assert_eq!(error.kind, ErrorKind::Internal, "{op:?}: {error}");
                    seen.block.store(true, Ordering::Relaxed);
                    let later = call.context().checkpoint().unwrap_err();
                    assert_eq!(later.kind, ErrorKind::Internal);
                    assert_eq!(later.message, error.message);
                    seen.later.store(true, Ordering::Relaxed);
                    reaction.result()
                }),
            );
            let script = corrupt(engine.compile(GUARDED_BLOCK).unwrap(), "<block>", |code| {
                code[0] = op;
            });
            let error = script.call("run", &[], CallOptions::default()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Internal, "{op:?}: {error}");
            assert!(observed.block.load(Ordering::Relaxed));
            assert!(observed.later.load(Ordering::Relaxed));
            assert_eq!(observed.handlers.load(Ordering::Relaxed), 0);
        }
    }
}

#[test]
fn a_native_frame_has_no_required_function() {
    let script = Engine::new().compile("0").unwrap();
    let mut execution =
        Execution::new(&script, "__main__", &[], &[], CallOptions::default()).unwrap();
    let run = execution.run.as_mut().unwrap();
    let mut frame = Frame::new(run.root.clone(), &run.storage, Some(0), 0, 0);
    assert_eq!(frame.required_function().unwrap(), 0);
    frame.function = FrameCode::Iteration;
    let error = Error::from(frame.required_function().unwrap_err());
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.ends_with("frame must have a function"));
}

#[test]
fn missing_run_state_latches_before_returning_to_the_host() {
    for missing in ["entry", "result", "driver"] {
        let script = Engine::new().compile("0").unwrap();
        let mut execution =
            Execution::new(&script, "__main__", &[], &[], CallOptions::default()).unwrap();
        let run = execution.run.as_mut().unwrap();
        let ctx = &mut execution.context;
        let boundary = match missing {
            "entry" => {
                run.pending_entry = None;
                None
            }
            "result" => Some(0),
            "driver" => {
                let mut frame = Frame::new(run.root.clone(), &run.storage, None, 0, 0);
                frame.function = FrameCode::PooledIteration;
                run.frames.push(ctx, frame).unwrap();
                None
            }
            _ => panic!("unknown case"),
        };
        let error = run.resume(ctx, boundary, None).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Internal, "{missing}: {error}");
        assert!(error.class().is_none());
        let later = ctx.checkpoint().unwrap_err();
        assert_eq!(later.kind, ErrorKind::Internal);
        assert_eq!(later.message, error.message);
    }
}

#[test]
fn malformed_integer_literals_bypass_script_handlers() {
    for body in [
        "123456",
        "begin;123456;rescue;42;end",
        "begin;123456;ensure;return 42;end",
    ] {
        let source = format!("def run -> int;{body};end");
        let script = corrupt(Engine::new().compile(&source).unwrap(), "run", |code| {
            let constant = code
                .iter_mut()
                .find(|op| matches!(op, Op::Constant(_)))
                .unwrap();
            let Op::Constant(index) = *constant else {
                panic!("expected a constant");
            };
            *constant = Op::Integer(index, 10);
        });
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal, "{body}: {error}");
        assert!(error.message.ends_with("integer literal must be bytes"));
    }
}

#[test]
fn an_open_range_in_an_active_loop_is_an_internal_error() {
    for (start, end) in [(None, Some(3)), (Some(1), None)] {
        let script = corrupt(
            Engine::new().compile("123456").unwrap(),
            "__main__",
            |code| {
                code[0] = Op::IterNext;
            },
        );
        let mut execution =
            Execution::new(&script, "__main__", &[], &[], CallOptions::default()).unwrap();
        let run = execution.run.as_mut().unwrap();
        let ctx = &mut execution.context;
        run.storage
            .loops
            .push(
                ctx,
                LoopState {
                    base: 0,
                    address_base: 0,
                    bypass_base: 0,
                    argument_base: 0,
                    text_base: 0,
                    next: 0,
                    end: 0,
                    expression: true,
                    source: Value(Kind::Range(Range::untracked(start, end, false))),
                    position: 0,
                    length: 3,
                    last: Value::nil(),
                    broken: false,
                    break_value: None,
                },
            )
            .unwrap();
        let error = run.run(ctx).unwrap_err();
        assert_eq!(
            error.kind,
            ErrorKind::Internal,
            "{start:?}..{end:?}: {error}"
        );
        assert!(error.message.contains("iterated range must have"));
        assert_eq!(ctx.checkpoint().unwrap_err().message, error.message);
    }
}

#[test]
fn a_callback_cannot_swallow_a_missing_code_roots_error() {
    for reaction in [Reaction::Ignore, Reaction::Replace] {
        let observed = Arc::new(Observed::default());
        let mut engine = marking(&observed);
        let seen = observed.clone();
        let method = HostMethod::new_with_block("cap.store", move |call, _, _| {
            call.context().code_roots = None;
            let error = call
                .set_receiver_field(b"value", &Value::int(1))
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Internal);
            assert!(
                error
                    .message
                    .ends_with("invocation must retain its code roots")
            );
            seen.block.store(true, Ordering::Relaxed);
            let later = call.context().checkpoint().unwrap_err();
            assert_eq!(later.message, error.message);
            seen.later.store(true, Ordering::Relaxed);
            reaction.result()
        });
        let capability = crate::Capability::from_value(
            "cap",
            Value::object(vec![(b"store".to_vec(), method.value())]),
        );
        engine.declare_capability(&capability).unwrap();
        let script = engine
            .compile("def run -> any;begin;cap.store();rescue;mark();ensure;mark();end;end")
            .unwrap();
        let error = script
            .call(
                "run",
                &[],
                CallOptions {
                    capabilities: vec![capability],
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(observed.block.load(Ordering::Relaxed));
        assert!(observed.later.load(Ordering::Relaxed));
        assert_eq!(observed.handlers.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn non_utf8_namespace_fields_keep_their_type_error() {
    let script = Engine::new().compile("module Box;end").unwrap();
    let mut execution =
        Execution::new(&script, "__main__", &[], &[], CallOptions::default()).unwrap();
    let run = execution.run.as_mut().unwrap();
    let ctx = &mut execution.context;
    let program = run.root.clone();
    let state = namespaces::state(&program, ctx, &mut run.storage, 0).unwrap();
    let key = ctx.bytes(&[0xff]).unwrap();
    run.storage
        .namespaces
        .at_mut(state)
        .unwrap()
        .fields
        .insert_field(ctx, key, Value::int(1))
        .unwrap();
    let value = Value(Kind::Namespace(
        run.storage.namespaces.at(state).unwrap().namespace.clone(),
    ));
    let error = programs::snapshot(ctx, &run.storage, &value).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(error.message, "namespace field name is not UTF-8");
}

#[test]
fn an_import_with_a_slot_past_the_class_layout_is_terminal() {
    let script = corrupt(
        Engine::new()
            .compile(
                "class Box\n@value: int\ndef initialize(value: int)\n@value = value\nend\nend\nBox.new(7)",
            )
            .unwrap(),
        "Box#initialize",
        |code| {
            let store = code
                .iter_mut()
                .find(|op| matches!(op, Op::InstanceStore(_)))
                .unwrap();
            assert!(matches!(store, Op::InstanceStore(0)));
            *store = Op::InstanceStore(5);
        },
    );
    // The bad write completes; importing its result must fail at the name lookup.
    let value = script.run(CallOptions::default()).unwrap().value;
    let Kind::Instance(instance) = &value.0 else {
        panic!("expected an instance");
    };
    assert_eq!(instance.class().field_layout().unwrap(), &["value"]);

    for reaction in [Reaction::Ignore, Reaction::Replace] {
        let observed = Arc::new(Observed::default());
        let mut engine = marking(&observed);
        let seen = observed.clone();
        let value = value.clone();
        engine.register("visit", move |ctx, _| {
            let error = ctx.import(&value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Internal);
            assert!(error.class().is_none());
            assert!(
                error
                    .message
                    .starts_with("internal error at src/objects/fields.rs:"),
                "{error}"
            );
            assert!(
                error
                    .message
                    .contains("index 5 is out of range for length 1"),
                "{error}"
            );
            seen.block.store(true, Ordering::Relaxed);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
            seen.later.store(true, Ordering::Relaxed);
            reaction.result()
        });
        let script = engine
            .compile(
                "def run -> any\nbegin\nvisit()\nrescue\nmark()\nensure\nmark()\nend\nmark()\nend",
            )
            .unwrap();
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal, "{reaction:?}: {error}");
        assert!(
            error
                .message
                .contains("index 5 is out of range for length 1"),
            "{reaction:?}: {error}"
        );
        assert!(observed.block.load(Ordering::Relaxed));
        assert!(observed.later.load(Ordering::Relaxed));
        assert_eq!(observed.handlers.load(Ordering::Relaxed), 0);
    }
}

#[cfg(feature = "tokio")]
mod asynchronous {
    use super::*;
    use crate::asynchronous::Runner;

    #[tokio::test]
    async fn a_synchronous_callback_on_a_runner_cannot_swallow_an_internal_block_error() {
        for reaction in [Reaction::Ignore, Reaction::Replace] {
            let (script, observed) = guarded_block(reaction);
            let runner = Runner::new(1).unwrap();
            let result = runner
                .call(script, "run".into(), vec![], CallOptions::default())
                .await;
            observed.check(result.map(drop), reaction);
        }
    }

    #[tokio::test]
    async fn an_asynchronous_callback_cannot_swallow_an_internal_block_error() {
        for reaction in [Reaction::Ignore, Reaction::Replace] {
            let observed = Arc::new(Observed::default());
            let mut engine = marking(&observed);
            let seen = observed.clone();
            let finished = Arc::new(AtomicBool::new(false));
            let returned = finished.clone();
            engine.register_method(
                "visit",
                HostMethod::new_async("visit", move |call, _, _| {
                    let seen = seen.clone();
                    let returned = returned.clone();
                    Box::pin(async move {
                        let block = call.call_block(vec![]).await;
                        seen.block
                            .store(block.is_err_and(|e| is_internal(&e)), Ordering::Relaxed);
                        let later = call.context().and_then(|ctx| ctx.checkpoint());
                        seen.later
                            .store(later.is_err_and(|e| is_internal(&e)), Ordering::Relaxed);
                        returned.store(true, Ordering::Relaxed);
                        reaction.result()
                    })
                }),
            );
            let script = corrupt(
                engine.compile(GUARDED_BLOCK).unwrap(),
                "run",
                block_out_of_range,
            );
            let runner = Runner::new(1).unwrap();
            let result = runner
                .call(script, "run".into(), vec![], CallOptions::default())
                .await;
            // The watchdog can retire the callback at its await as soon as
            // the block latches Internal. Both outcomes must be terminal;
            // only a callback that finished can have consumed the error.
            observed.check_terminal(result.map(drop), reaction);
            if finished.load(Ordering::Relaxed) {
                observed.check_callback(reaction);
            }
        }
    }

    #[tokio::test]
    async fn a_callback_retired_after_an_internal_block_error_stays_terminal() {
        struct MarkDropped(Arc<AtomicBool>);
        impl Drop for MarkDropped {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
            }
        }

        let observed = Arc::new(Observed::default());
        let dropped = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let mut engine = marking(&observed);
        let on_drop = dropped.clone();
        let on_complete = completed.clone();
        engine.register_method(
            "visit",
            HostMethod::new_async("visit", move |call, _, _| {
                let on_drop = on_drop.clone();
                let on_complete = on_complete.clone();
                Box::pin(async move {
                    let _dropped = MarkDropped(on_drop);
                    let _result = call.call_block(vec![]).await;
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    on_complete.store(true, Ordering::Relaxed);
                    Ok(Value::int(99))
                })
            }),
        );
        let script = corrupt(
            engine.compile(GUARDED_BLOCK).unwrap(),
            "run",
            block_out_of_range,
        );
        let runner = Runner::new(1).unwrap();
        let error = runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
            .unwrap_err();
        assert!(is_internal(&error), "{error}");
        assert!(dropped.load(Ordering::Relaxed));
        assert!(!completed.load(Ordering::Relaxed));
        assert_eq!(observed.handlers.load(Ordering::Relaxed), 0);
    }
}
