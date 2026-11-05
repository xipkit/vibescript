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
        let error = result.expect_err("the corrupted block target must fail the call");
        assert!(is_internal(&error), "{reaction:?}: {error}");
        assert!(self.block.load(Ordering::Relaxed), "{reaction:?}");
        assert!(self.later.load(Ordering::Relaxed), "{reaction:?}");
        assert_eq!(self.handlers.load(Ordering::Relaxed), 0, "{reaction:?}");
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
            engine.register_method(
                "visit",
                HostMethod::new_async("visit", move |call, _, _| {
                    let seen = seen.clone();
                    Box::pin(async move {
                        let block = call.call_block(vec![]).await;
                        seen.block
                            .store(block.is_err_and(|e| is_internal(&e)), Ordering::Relaxed);
                        let later = call.context().and_then(|ctx| ctx.checkpoint());
                        seen.later
                            .store(later.is_err_and(|e| is_internal(&e)), Ordering::Relaxed);
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
            observed.check(result.map(drop), reaction);
        }
    }
}
