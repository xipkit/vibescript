use super::{
    arguments::Input,
    environment::Environment,
    facts::{Atom, Facts},
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Capability, Engine, ErrorKind, HostMethod, Limits, Result, Script,
    Signature, SignatureParam, Stats, Value,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn value(source: &str) -> Value {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}

fn options(name: &str, input: Value) -> CallOptions {
    CallOptions {
        globals: [(name.into(), input)].into(),
        ..CallOptions::default()
    }
}

fn signature(parameter: Option<(&str, bool)>, result: &str, block: bool) -> Signature {
    Signature {
        params: parameter
            .map(|(ty, optional)| SignatureParam {
                name: "value".into(),
                ty: ty.into(),
                optional,
            })
            .into_iter()
            .collect(),
        result: result.into(),
        accepts_block: block,
    }
}

fn witness(
    script: &Script,
    options: &CallOptions,
    args: &[Value],
    broad: bool,
    expected: &str,
    issues: bool,
) -> Stats {
    let mut ctx = CallContext::new(options.clone());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, script, options).unwrap();
    let program = &script.inner.code.program;
    let inputs = args
        .iter()
        .map(|arg| {
            Input::Supplied(if broad {
                Atom::Bool.fact()
            } else {
                observed(&mut ctx, &mut facts, program, arg)
            })
        })
        .collect::<Vec<_>>();
    let report = environment
        .analyze(&mut ctx, &mut facts, program.names["run"], &inputs)
        .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(!report.issues.data.is_empty(), issues, "{report:?}");
    let actual = script.call("run", args, options.clone()).unwrap().value;
    assert_eq!(actual.to_string(), expected);
    let actual = observed(&mut ctx, &mut facts, program, &actual);
    assert_ne!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Rejected,
        "{report:?}"
    );
    drop((report, environment, facts));
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    stats
}

fn deep() -> Value {
    let mut input = Value::int(1);
    for _ in 0..129 {
        input = Value::array(vec![input]);
    }
    input
}

#[test]
fn unused_and_overwritten_large_globals_keep_lazy_accounting() {
    let huge = Value::array(vec![Value::bytes(vec![b'x'; 1024]); 512]);
    for strict in [false, true] {
        let mut engine = Engine::new();
        engine.set_strict_effects(strict);
        for body in [
            "7",
            "big=7;big",
            "def ignore(big);big;end;def run;ignore(7);end",
        ] {
            let source = if body.starts_with("def ") {
                body.into()
            } else {
                format!("def run;{body};end")
            };
            let script = engine.compile(&source).unwrap();
            let mut supplied = options("big", huge.clone());
            supplied.limits.memory_bytes = Some(48 << 10);
            let stats = witness(&script, &supplied, &[], false, "7", false);
            assert!(stats.peak_memory_bytes < 48 << 10);
            if !strict {
                let small = witness(
                    &script,
                    &options("big", Value::int(0)),
                    &[],
                    false,
                    "7",
                    false,
                );
                assert_eq!(stats.steps, small.steps);
                assert_eq!(stats.peak_memory_bytes, small.peak_memory_bytes);
            }
        }
    }
}

#[test]
fn a_reachable_large_global_read_still_exhausts_memory() {
    let script = Engine::new().compile("def run;big;end").unwrap();
    let mut options = options("big", Value::bytes(vec![b'x'; 128 << 10]));
    options.limits.memory_bytes = Some(48 << 10);
    let mut ctx = CallContext::new(options.clone());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    let error = environment
        .analyze(
            &mut ctx,
            &mut facts,
            script.inner.code.program.names["run"],
            &[],
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
    drop((environment, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert_eq!(
        script.call("run", &[], options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn strict_validation_rejects_unused_effectful_values_before_any_effect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let count = calls.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(1))
    });
    let script = engine
        .compile("class C;effect();end;def run(x=effect());effect();end")
        .unwrap();
    let mut poisons = vec![HostMethod::new("send", |_, _, _| panic!("host callback ran")).value()];
    for source in [
        "JSON",
        "JSON[:parse]",
        "{x:int}",
        "class C;end;C",
        "class C;end;C.new",
    ] {
        poisons.push(value(source));
    }
    for poison in poisons {
        let mut supplied = options(
            "unused",
            Value::array(vec![Value::object(vec![(b"hidden".to_vec(), poison)])]),
        );
        let count = calls.clone();
        supplied
            .capabilities
            .push(Capability::new("factory", move |_| {
                count.fetch_add(1, Ordering::Relaxed);
                Ok(Value::int(1))
            }));
        let mut ctx = CallContext::new(supplied.clone());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let error = match Environment::new(&mut ctx, &mut facts, &script, &supplied) {
            Err(error) => error,
            Ok(_) => panic!("strict input accepted"),
        };
        let actual = script.call("run", &[], supplied).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Runtime);
        assert_eq!(error.message, actual.message);
        assert!(
            error
                .message
                .starts_with("strict effects: global unused must be data-only")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(ctx.checkpoint().is_ok());
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn strict_data_values_and_maximum_depth_shared_graphs_are_accepted() {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("def run;7;end").unwrap();
    let mut shared = Value::int(1);
    for _ in 0..128 {
        shared = Value::array(vec![shared.clone(), shared]);
    }
    for data in [
        shared,
        value("enum State;Ready;end;State"),
        value("1..3"),
        value("/a/"),
        Value::object(vec![(
            b"a".to_vec(),
            Value::array(vec![Value::int(1), Value::nil()]),
        )]),
    ] {
        witness(&script, &options("unused", data), &[], false, "7", false);
    }
    let supplied = options("unused", deep());
    let mut ctx = CallContext::new(supplied.clone());
    let mut facts = Facts::new(&mut ctx).unwrap();
    assert!(
        matches!(Environment::new(&mut ctx, &mut facts, &script, &supplied), Err(error) if error.kind == ErrorKind::Recursion)
    );
    assert_eq!(
        script.call("run", &[], supplied).unwrap_err().kind,
        ErrorKind::Recursion
    );
    assert!(ctx.checkpoint().is_ok());
}

#[test]
fn unused_invalid_inputs_stay_unread_and_import_errors_are_catchable() {
    let namespace = value("class C;end;C");
    for input in [deep(), namespace] {
        for body in ["7", "unused=7;unused", "if false;unused;end;7"] {
            let script = Engine::new()
                .compile(&format!("def run;{body};end"))
                .unwrap();
            witness(
                &script,
                &options("unused", input.clone()),
                &[],
                false,
                "7",
                false,
            );
        }
    }
    for body in [
        "begin;unused;rescue LimitError;9;end",
        "begin;unused.length;rescue LimitError;9;end",
    ] {
        let script = Engine::new()
            .compile(&format!("def run;{body};end"))
            .unwrap();
        witness(&script, &options("unused", deep()), &[], false, "9", false);
    }
}

#[test]
fn root_import_keeps_attached_method_guards_on_the_read_path() {
    let method = HostMethod::new("send", |_, _, _| panic!("detached method called"));
    let poison = Value::array(vec![method.value()]);
    for (body, issues) in [
        ("7", false),
        ("data=7;data", false),
        ("begin;data;rescue RuntimeError;7;end", true),
    ] {
        let script = Engine::new()
            .compile(&format!("def run;{body};end"))
            .unwrap();
        witness(
            &script,
            &options("data", poison.clone()),
            &[],
            false,
            "7",
            issues,
        );
    }
}

#[test]
fn conditional_writes_preserve_unread_roots_across_calls() {
    for body in [
        "def run(flag);if flag;items=[2,3];end;items.length;end",
        "def change(flag);if flag;items=[2,3];end;end;def run(flag);change(flag);items.length;end",
        "def run(flag);[1].each { if flag;items=[2,3];end };items.length;end",
        "def change(flag);if flag;items=[2,3];end;raise(\"stop\");end;def run(flag);begin;change(flag);rescue;nil;end;items.length;end",
    ] {
        let script = Engine::new().compile(body).unwrap();
        let supplied = options("items", Value::array(vec![Value::int(1)]));
        for (flag, expected) in [(false, "1"), (true, "2")] {
            witness(
                &script,
                &supplied,
                &[Value::boolean(flag)],
                true,
                expected,
                false,
            );
        }
    }
}

#[test]
fn blocks_and_pending_addresses_keep_loaded_roots_and_original_elements() {
    for (body, expected) in [
        ("[1].each { items.push(2) };items", "[1, 2]"),
        ("[1].each { items=[3,4] };items", "[3, 4]"),
        ("items[-1] += (begin;items.push(3);2;end);items", "[3, 3]"),
        ("copy=items;items.push(2);[copy,items]", "[[1], [1, 2]]"),
    ] {
        let script = Engine::new()
            .compile(&format!("def run;{body};end"))
            .unwrap();
        witness(
            &script,
            &options("items", Value::array(vec![Value::int(1)])),
            &[],
            false,
            expected,
            false,
        );
    }
}

#[test]
fn lazy_host_targets_are_selected_before_argument_rebinding() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let method = HostMethod::new("send", move |_, args, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(args[0].clone())
    })
    .with_signature(signature(Some(("int", false)), "int", false))
    .unwrap();
    let script = Engine::new()
        .compile("def run;send(begin;send=nil;7;end);end")
        .unwrap();
    witness(
        &script,
        &options("send", method.value()),
        &[],
        false,
        "7",
        false,
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn script_contracts_load_only_the_selected_type_roots() {
    let enumeration = value("enum State;Ready;end;State");
    for (declarations, ty, root, binding) in [
        ("", "Alias", "Alias", enumeration.clone()),
        (
            "",
            "catalog.State",
            "catalog",
            Value::object(vec![(b"State".to_vec(), enumeration.clone())]),
        ),
        ("enum State;Ready;end;", "State", "state", deep()),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "{declarations}def pick(x:{ty})->{ty};x;end;def run;pick(:ready).name;end"
            ))
            .unwrap();
        let mut supplied = options(root, binding);
        supplied.globals.insert("unrelated".into(), deep());
        witness(&script, &supplied, &[], false, "Ready", false);
    }
}

#[test]
fn host_contracts_resolve_lazy_aliases_and_skip_unused_optional_types() {
    let enumeration = value("enum State;Ready;end;State");
    let mut engine = Engine::new();
    engine.register_method(
        "echo",
        HostMethod::new("echo", |_, args, _| Ok(args[0].clone()))
            .with_signature(signature(Some(("Alias", false)), "Alias", false))
            .unwrap(),
    );
    let script = engine.compile("def run;echo(:ready).name;end").unwrap();
    let mut supplied = options("Alias", enumeration);
    supplied.globals.insert("unrelated".into(), deep());
    witness(&script, &supplied, &[], false, "Ready", false);
    engine.register_method(
        "optional",
        HostMethod::new("optional", |_, _, _| Ok(Value::int(7)))
            .with_signature(signature(Some(("Alias", true)), "int", false))
            .unwrap(),
    );
    let script = engine.compile("def run;optional();end").unwrap();
    witness(&script, &options("Alias", deep()), &[], false, "7", false);
}

#[test]
fn type_root_import_failures_reach_the_correct_rescue() {
    let mut engine = Engine::new();
    engine.register_method(
        "echo",
        HostMethod::new("echo", |_, _, _| {
            panic!("invalid type root reached callback")
        })
        .with_signature(signature(Some(("Alias", false)), "Alias", false))
        .unwrap(),
    );
    for source in [
        "def pick(x:Alias);x;end;def run;begin;pick(1);rescue LimitError;9;end;end",
        "def run;begin;echo(1);rescue LimitError;9;end;end",
    ] {
        let script = engine.compile(source).unwrap();
        witness(&script, &options("Alias", deep()), &[], false, "9", false);
    }
    let script = engine
        .compile(
            "def pick(x:alias);x;end;def run;begin;pick(1);rescue LimitError;9;rescue;11;end;end",
        )
        .unwrap();
    let mut supplied = options("ALIAS", value("enum A;Ready;end;A"));
    supplied
        .globals
        .insert("Alias".into(), value("enum B;Ready;end;B"));
    supplied.globals.insert("aLIAS".into(), deep());
    witness(&script, &supplied, &[], false, "9", false);
}

#[test]
fn the_first_type_binding_error_precedes_later_input_imports() {
    let mut engine = Engine::new();
    engine.register_method(
        "echo",
        HostMethod::new("echo", |_, _, _| panic!("invalid type reached callback"))
            .with_signature(signature(Some(("Missing|Alias", false)), "int", false))
            .unwrap(),
    );
    for source in [
        "def pick(x:Missing|Alias);x;end;def run;begin;pick(1);rescue LimitError;11;rescue;9;end;end",
        "def run;begin;echo(1);rescue LimitError;11;rescue;9;end;end",
    ] {
        let script = engine.compile(source).unwrap();
        witness(&script, &options("Alias", deep()), &[], false, "9", true);
    }
}

#[test]
fn stale_host_grants_fail_before_lazy_named_type_loading() {
    let method = HostMethod::new("send", |_, _, _| panic!("stale grant called"))
        .with_signature(signature(Some(("Alias", false)), "Alias", false))
        .unwrap();
    let mut old = CallContext::new(CallOptions::default());
    let bound = old.import(&method.value()).unwrap();
    drop(old);
    let mut supplied = options("send", bound);
    supplied.globals.insert("Alias".into(), deep());
    let script = Engine::new()
        .compile("def run;begin;send(1);rescue RuntimeError;9;end;end")
        .unwrap();
    witness(&script, &supplied, &[], false, "9", true);
}

fn accounting_work(ctx: &mut CallContext, script: &Script, options: &CallOptions) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let environment = Environment::new(ctx, &mut facts, script, options)?;
    let report =
        environment.analyze(ctx, &mut facts, script.inner.code.program.names["run"], &[])?;
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn deferred_type_and_value_loading_obey_exact_and_sampled_quotas() {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine.register_method(
        "echo",
        HostMethod::new("echo", |_, _, _| panic!("analysis invoked callback"))
            .with_signature(signature(Some(("array<Alias>", false)), "Alias", false))
            .unwrap(),
    );
    let script = engine
        .compile("def run;items.push(:ready);echo(items).name;end")
        .unwrap();
    let mut supplied = options("items", Value::array(vec![]));
    supplied
        .globals
        .insert("Alias".into(), value("enum State;Ready;end;State"));
    let mut ctx = CallContext::new(CallOptions::default());
    accounting_work(&mut ctx, &script, &supplied).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, expected) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            accounting_work(&mut ctx, &script, &supplied)
                .err()
                .map(|error| error.kind),
            expected
        );
        if let Some(expected) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..24 {
        for memory in [false, true] {
            let mut limits = Limits::default();
            let expected = if memory {
                limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 24);
                ErrorKind::Memory
            } else {
                limits.steps = Some(stats.steps * sample as u64 / 24);
                ErrorKind::Steps
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(
                accounting_work(&mut ctx, &script, &supplied)
                    .unwrap_err()
                    .kind,
                expected
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn strict_preparation_and_lazy_reads_preserve_cancellation_and_deadlines() {
    for strict in [false, true] {
        let mut engine = Engine::new();
        engine.set_strict_effects(strict);
        let script = engine.compile("def run;items.length;end").unwrap();
        let supplied = options("items", Value::array(vec![Value::int(1)]));
        for deadline in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let environment = Environment::new(&mut ctx, &mut facts, &script, &supplied).unwrap();
            if deadline {
                ctx.options.deadline = Some(std::time::Instant::now());
            } else {
                ctx.options.cancellation.cancel();
            }
            let expected = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            assert!(
                matches!(Environment::new(&mut ctx, &mut facts, &script, &supplied), Err(error) if error.kind == expected)
            );
            assert_eq!(
                environment
                    .analyze(
                        &mut ctx,
                        &mut facts,
                        script.inner.code.program.names["run"],
                        &[]
                    )
                    .unwrap_err()
                    .kind,
                expected
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            drop((environment, facts));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
