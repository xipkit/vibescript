use super::{
    arguments::Failure,
    environment::{Environment, Incomplete},
    facts::{Callable, Facts, Node},
    flow::IssueKind,
    inputs::Values,
    normalization_tests::observed,
    relation::Relation,
    type_bindings::{Bindings, Resolution},
};
use crate::{
    CallContext, CallOptions, Capability, Engine, ErrorKind, HostMethod, Limits, Result, Script,
    Signature, SignatureParam, Value, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn source_identity_hashing_has_repeatable_work_with_multiple_live_arenas() {
    let script = Engine::new().compile("class C;end;def run;7;end").unwrap();
    let mut arenas = Vec::new();
    let mut baseline = None;
    for _ in 0..32 {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = super::facts::Facts::new(&mut ctx).unwrap();
        let owner = facts
            .source_owner(&mut ctx, &script.inner.code, None)
            .unwrap();
        for index in 0..32 {
            facts.integer(&mut ctx, index as i64).unwrap();
            facts.nominal(&mut ctx, owner, index, b"C", None).unwrap();
            facts
                .callable(&mut ctx, owner, super::facts::Callable::Function(index))
                .unwrap();
        }
        let counters = (ctx.stats().steps, ctx.stats().peak_memory_bytes);
        assert_eq!(counters, *baseline.get_or_insert(counters));
        arenas.push((ctx, facts));
    }
    for (ctx, facts) in arenas {
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn signature(parameter: Option<&str>, result: &str, block: bool) -> Signature {
    Signature {
        params: parameter
            .map(|ty| SignatureParam {
                name: "value".into(),
                ty: ty.into(),
                optional: false,
            })
            .into_iter()
            .collect(),
        result: result.into(),
        accepts_block: block,
    }
}

fn witness(script: &Script, options: &CallOptions, expected: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, script, options).unwrap();
    assert!(
        environment.incomplete.data.is_empty(),
        "{:?}",
        environment.incomplete
    );
    let program = &script.inner.code.program;
    let report = environment
        .analyze(&mut ctx, &mut facts, program.names["run"], &[])
        .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(!report.issues.data.is_empty(), rejected, "{report:?}");
    let output = script.call("run", &[], options.clone()).unwrap();
    assert_eq!(output.value.to_string(), expected);
    let actual = observed(&mut ctx, &mut facts, program, &output.value);
    assert_ne!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Rejected,
        "{report:?}"
    );
    drop((report, environment, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn compiled_legacy_callbacks_ignore_attached_blocks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("echo", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    let script = engine
        .compile("def run; count=0; echo { count=1; missing }; count; end")
        .unwrap();
    let options = CallOptions::default();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            script.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert_eq!(report.contexts, 1);
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(report.returns, facts.integer(&mut ctx, 0).unwrap());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        script.call("run", &[], options).unwrap().value.as_int(),
        Some(0)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn signed_plain_methods_keep_signature_guards_and_ignore_allowed_blocks() {
    for allowed in [false, true] {
        let mut engine = Engine::new();
        engine.register_method(
            "echo",
            HostMethod::new("echo", |_, _, _| Ok(Value::int(7)))
                .with_signature(signature(None, "int", allowed))
                .unwrap(),
        );
        let source = if allowed {
            "def run; count=0; echo { count=1; missing }; count; end"
        } else {
            "def run; begin; echo { missing }; rescue; 9; end; end"
        };
        witness(
            &engine.compile(source).unwrap(),
            &CallOptions::default(),
            if allowed { "0" } else { "9" },
            !allowed,
        );
    }
}

#[test]
fn unsigned_plain_methods_reject_blocks_before_validators() {
    let calls = Arc::new(AtomicUsize::new(0));
    let callback = calls.clone();
    let validator = calls.clone();
    let method = HostMethod::new("echo", move |_, _, _| {
        callback.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    })
    .with_contract(
        move |_, _, _| {
            validator.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
        |_, _| Ok(()),
    );
    let mut engine = Engine::new();
    engine.register_method("echo", method);
    let script = engine
        .compile("def run; begin; echo { missing }; rescue ArgumentError; 9; end; end")
        .unwrap();
    witness(&script, &CallOptions::default(), "9", true);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn registered_block_drivers_keep_sticky_transfers() {
    for transfer in ["break 7", "return 9"] {
        let mut engine = Engine::new();
        engine.register_method(
            "visit",
            HostMethod::new_with_block("visit", |call, _, _| {
                for _ in 0..3 {
                    let _ = call.call_block(&[]);
                }
                Ok(Value::int(99))
            })
            .with_signature(signature(None, "int", true))
            .unwrap(),
        );
        let source = format!(
            "def run; count=0; result=visit {{ count+=1; {transfer} }}; [result,count]; end"
        );
        witness(
            &engine.compile(&source).unwrap(),
            &CallOptions::default(),
            if transfer.starts_with("break") {
                "[7, 1]"
            } else {
                "9"
            },
            false,
        );
    }
}

#[test]
fn registered_named_contracts_resolve_live_source_and_root_types() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register_method(
        "echo",
        HostMethod::new("echo", move |_, args, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(args[0].clone())
        })
        .with_signature(signature(Some("Status"), "Status", false))
        .unwrap(),
    );
    let script = engine
        .compile(
            "enum Status; Draft; Sent; end; def run; begin; echo(:draft).name; rescue; 9; end; end",
        )
        .unwrap();
    witness(&script, &CallOptions::default(), "Draft", false);
    let options = CallOptions {
        globals: [("Status".into(), Value::nil())].into(),
        ..CallOptions::default()
    };
    witness(&script, &options, "9", true);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn supplied_roots_preserve_precedence_and_live_mutations() {
    let mut engine = Engine::new();
    engine.register("answer", |_, _| Ok(Value::int(22)));
    let script = engine
        .compile("def answer; 11; end; def run; answer; end")
        .unwrap();
    witness(&script, &CallOptions::default(), "11", false);
    let options = CallOptions {
        globals: [("answer".into(), Value::int(7))].into(),
        ..CallOptions::default()
    };
    witness(&script, &options, "7", false);
    let script = engine
        .compile("def bump; items.push(9); end; def run; saved=items; bump; [saved,items]; end")
        .unwrap();
    let options = CallOptions {
        globals: [("items".into(), Value::array(vec![Value::int(1)]))].into(),
        ..CallOptions::default()
    };
    witness(&script, &options, "[[1], [1, 9]]", false);
    assert_eq!(options.globals["items"].to_string(), "[1]");
}

#[test]
fn repeated_method_descriptors_share_metadata_but_keep_distinct_grants() {
    let method = HostMethod::new("deliver", |_, _, _| Ok(Value::int(7)))
        .with_signature(signature(None, "int", false))
        .unwrap();
    let fresh = Value::object(vec![
        (b"a".to_vec(), method.value()),
        (b"b".to_vec(), method.value()),
    ]);
    let producer = Engine::new().compile("def run; object; end").unwrap();
    let stale = producer
        .call(
            "run",
            &[],
            CallOptions {
                globals: [("object".into(), fresh.clone())].into(),
                ..CallOptions::default()
            },
        )
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("def run; [fresh.a(), (begin; stale.a(); rescue; 9; end)]; end")
        .unwrap();
    let options = CallOptions {
        globals: [("fresh".into(), fresh), ("stale".into(), stale)].into(),
        ..CallOptions::default()
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    let world = environment.world();
    let mut values = Values::new();
    let fresh = values.read(&mut ctx, &mut facts, &world, 0).unwrap().value;
    let stale = values.read(&mut ctx, &mut facts, &world, 1).unwrap().value;
    let a = facts
        .selected_field(&mut ctx, fresh, b"a")
        .unwrap()
        .unwrap()
        .0;
    let b = facts
        .selected_field(&mut ctx, fresh, b"b")
        .unwrap()
        .unwrap()
        .0;
    let old = facts
        .selected_field(&mut ctx, stale, b"a")
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(a, b);
    assert_ne!(a, old);
    assert!(values.host(&mut ctx, &world, 0).unwrap().is_some());
    assert!(values.host(&mut ctx, &world, 1).unwrap().is_some());
    assert!(values.host(&mut ctx, &world, 2).unwrap().is_none());
    witness(&script, &options, "[7, 9]", true);
}

#[test]
fn stale_grants_fail_before_signature_resolution_and_validators() {
    let calls = Arc::new(AtomicUsize::new(0));
    let callback = calls.clone();
    let validator = calls.clone();
    let method = HostMethod::new("deliver", move |_, _, _| {
        callback.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    })
    .with_contract(
        move |_, _, _| {
            validator.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
        |_, _| Ok(()),
    )
    .with_signature(signature(Some("Missing"), "Missing", false))
    .unwrap();
    let producer = Engine::new().compile("def run; object; end").unwrap();
    let stale = producer
        .call(
            "run",
            &[],
            CallOptions {
                globals: [(
                    "object".into(),
                    Value::object(vec![(b"deliver".to_vec(), method.value())]),
                )]
                .into(),
                ..CallOptions::default()
            },
        )
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("def run; begin; object.deliver(7); rescue; 9; end; end")
        .unwrap();
    let options = CallOptions {
        globals: [("object".into(), stale)].into(),
        ..CallOptions::default()
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            script.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert!(report.issues.data.iter().any(|issue| matches!(
        issue.issue.kind,
        IssueKind::Call {
            failure: Failure::HostGrant,
            ..
        }
    )));
    assert!(!report.issues.data.iter().any(|issue| matches!(
        issue.issue.kind,
        IssueKind::Call {
            failure: Failure::HostTypeBinding { .. },
            ..
        }
    )));
    witness(&script, &options, "9", true);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn opaque_factories_remain_pending_even_when_an_explicit_global_overrides_them() {
    let calls = Arc::new(AtomicUsize::new(0));
    let capabilities = (0..2)
        .map(|_| {
            let count = calls.clone();
            Capability::new("value", move |_| {
                count.fetch_add(1, Ordering::Relaxed);
                Ok(Value::int(1))
            })
        })
        .collect();
    let options = CallOptions {
        globals: [("value".into(), Value::int(7))].into(),
        capabilities,
        ..CallOptions::default()
    };
    let script = Engine::new().compile("def run; value; end").unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    assert_eq!(environment.incomplete.data.len(), 2);
    for reason in &environment.incomplete.data {
        assert!(matches!(reason, Incomplete::Capability(name) if name.as_bytes()==Some(b"value")));
    }
    let world = environment.world();
    assert_eq!(world.globals.len(), 1);
    assert!(
        matches!(world.globals[0].1, super::calls::Target::Deferred(0))
            && world.inputs[0].as_int() == Some(7)
    );
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            script.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert!(!report.incomplete.data.is_empty());
    assert_eq!(report.contexts, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], options.clone())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[test]
fn initializers_are_analyzed_in_local_and_foreign_sources() {
    let mut engine = Engine::new();
    engine.register("mark", |_, _| panic!("initializer executed"));
    let script = engine
        .compile("class Widget; mark(); end; def run; 7; end")
        .unwrap();
    let options = CallOptions::default();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    assert!(environment.incomplete.data.is_empty());
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            script.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    let namespace = script.inner.code.program.declarations[0].clone();
    let consumer = Engine::new().compile("def run; Widget; end").unwrap();
    let globals = CallOptions {
        globals: [("Widget".into(), namespace)].into(),
        ..CallOptions::default()
    };
    let environment = Environment::new(&mut ctx, &mut facts, &consumer, &globals).unwrap();
    assert!(environment.incomplete.data.is_empty());
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            consumer.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    engine.set_strict_effects(true);
    let strict = engine.compile("def run; 7; end").unwrap();
    let globals = CallOptions {
        globals: [("data".into(), Value::int(1))].into(),
        ..CallOptions::default()
    };
    let environment = Environment::new(&mut ctx, &mut facts, &strict, &globals).unwrap();
    assert!(environment.incomplete.data.is_empty());
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            strict.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert_eq!(report.contexts, 1);
    assert!(report.incomplete.data.is_empty());
}

#[test]
fn admitted_classes_and_instances_share_source_declaration_identities() {
    let script = Engine::new()
        .compile("enum Status; Ready; end; class First; end; class Second; end; module Outer; module Inner; end; end; def run; 7; end")
        .unwrap();
    let mut producer = CallContext::new(CallOptions::default());
    let mut options = CallOptions::default();
    let mut expected = Vec::new();
    for declaration in &script.inner.code.program.declarations {
        let Kind::Namespace(namespace) = &declaration.0 else {
            continue;
        };
        let name = &namespace.definition.name;
        options.globals.insert(name.clone(), declaration.clone());
        expected.push((name.clone(), name.clone(), true));
        if namespace.definition.constructor.is_some() {
            let instance = crate::objects::new(&mut producer, namespace).unwrap();
            let root = format!("{name}_instance");
            options
                .globals
                .insert(root.clone(), Value(Kind::Instance(instance)));
            expected.push((root, name.clone(), false));
        }
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    let world = environment.world();
    let mut bindings = Bindings::new();
    let scope = bindings
        .source(&mut ctx, &mut facts, world.program, world.source_owner)
        .unwrap();
    let mut values = Values::new();
    for (root, name, is_type) in expected {
        let Resolution::Known(expected) =
            bindings.resolve(&mut ctx, &[scope], &name, false).unwrap()
        else {
            panic!("missing source declaration: {name}");
        };
        let (_, super::calls::Target::Deferred(index)) = world
            .globals
            .iter()
            .find(|(key, _)| key.as_bytes() == Some(root.as_bytes()))
            .unwrap()
        else {
            panic!("missing root: {root}");
        };
        let admitted = values.read(&mut ctx, &mut facts, &world, *index).unwrap();
        assert!(!admitted.incomplete);
        let actual = admitted.value;
        let actual = if is_type {
            let Node::TypeValue(value) = facts.node(actual) else {
                panic!("expected type value: {root}");
            };
            *value
        } else {
            let Node::Instance { class, .. } = facts.node(actual) else {
                panic!("expected instance: {root}")
            };
            *class
        };
        assert_eq!(actual, expected, "{root}");
    }
    drop(values);
    drop((bindings, environment, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn source_owners_distinguish_code_and_captured_scopes_without_retaining_heaps() {
    let first = Engine::new().compile("def run; 7; end").unwrap();
    let second = Engine::new().compile("def run; 7; end").unwrap();
    let mut producer = CallContext::new(CallOptions::default());
    let a = crate::objects::environment(&mut producer).unwrap();
    let b = crate::objects::environment(&mut producer).unwrap();
    let weak_scope = Arc::downgrade(&a);
    let weak_code = Arc::downgrade(&first.inner.code);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let plain = facts
        .source_owner(&mut ctx, &first.inner.code, None)
        .unwrap();
    assert_eq!(
        plain,
        facts
            .source_owner(&mut ctx, &first.inner.code, None)
            .unwrap()
    );
    let captured = facts
        .source_owner(&mut ctx, &first.inner.code, Some(&a))
        .unwrap();
    assert_eq!(
        captured,
        facts
            .source_owner(&mut ctx, &first.inner.code, Some(&a))
            .unwrap()
    );
    assert_ne!(plain, captured);
    let plain_id = facts.source_id(&mut ctx, plain).unwrap();
    let captured_id = facts.source_id(&mut ctx, captured).unwrap();
    assert_ne!(plain_id, captured_id);
    for source in [plain_id, captured_id] {
        let code = facts.source_code(&mut ctx, source).unwrap().unwrap();
        assert!(Arc::ptr_eq(&code, &first.inner.code));
    }
    assert_ne!(
        captured,
        facts
            .source_owner(&mut ctx, &first.inner.code, Some(&b))
            .unwrap()
    );
    assert_ne!(
        plain,
        facts
            .source_owner(&mut ctx, &second.inner.code, None)
            .unwrap()
    );
    for _ in 0..64 {
        let scope = crate::objects::environment(&mut producer).unwrap();
        facts
            .source_owner(&mut ctx, &first.inner.code, Some(&scope))
            .unwrap();
    }
    assert_eq!(
        captured,
        facts
            .source_owner(&mut ctx, &first.inner.code, Some(&a))
            .unwrap()
    );
    assert_eq!(facts.source_id(&mut ctx, captured).unwrap(), captured_id);
    assert_eq!(facts.source_id(&mut ctx, plain).unwrap(), plain_id);
    assert_eq!(
        facts.source_id(&mut ctx, 0).unwrap(),
        super::sources::SourceId::ROOT
    );
    assert!(
        facts
            .source_code(&mut ctx, super::sources::SourceId::ROOT)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        facts.source_id(&mut ctx, 1).unwrap_err().kind,
        ErrorKind::Runtime
    );
    drop((a, b, producer, first));
    assert!(weak_scope.upgrade().is_none());
    assert!(weak_code.upgrade().is_some());
    drop(facts);
    assert!(weak_code.upgrade().is_none());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn captured_functions_keep_distinct_owners_and_resolve_in_their_source() {
    let script = Engine::new()
        .compile("def helper; 99; end; def run; foreign.helper(); end")
        .unwrap();
    let code = crate::code::Code::compile_file("def helper; 7; end", &Default::default()).unwrap();
    let mut producer = CallContext::new(CallOptions::default());
    let first = crate::objects::environment(&mut producer).unwrap();
    let second = crate::objects::environment(&mut producer).unwrap();
    let index = code.program.names["helper"];
    let a = Value(Kind::Function(
        crate::exports::Function::new(&mut producer, code.clone(), first, index).unwrap(),
    ));
    let b = Value(Kind::Function(
        crate::exports::Function::new(&mut producer, code.clone(), second, index).unwrap(),
    ));
    let options = CallOptions {
        globals: [(
            "foreign".into(),
            Value::object(vec![(b"helper".to_vec(), a), (b"other".to_vec(), b)]),
        )]
        .into(),
        ..CallOptions::default()
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    let world = environment.world();
    let mut values = Values::new();
    let object = values.read(&mut ctx, &mut facts, &world, 0).unwrap().value;
    let a = facts
        .selected_field(&mut ctx, object, b"helper")
        .unwrap()
        .unwrap()
        .0;
    let b = facts
        .selected_field(&mut ctx, object, b"other")
        .unwrap()
        .unwrap()
        .0;
    let Node::Callable {
        owner: a,
        target: Callable::Function(_),
    } = facts.node(a)
    else {
        panic!()
    };
    let Node::Callable {
        owner: b,
        target: Callable::Function(_),
    } = facts.node(b)
    else {
        panic!()
    };
    assert_ne!(a, b);
    assert_ne!(*a, world.source_owner);
    assert_ne!(*b, world.source_owner);
    let report = environment
        .analyze(
            &mut ctx,
            &mut facts,
            script.inner.code.program.names["run"],
            &[],
        )
        .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(matches!(facts.node(report.returns), Node::Integer(7)));
}

fn accounting_script() -> (Script, CallOptions) {
    let mut engine = Engine::new();
    engine.register_method(
        "echo",
        HostMethod::new("echo", |_, _, _| panic!("callback executed"))
            .with_signature(signature(Some("array<Status>"), "Status", false))
            .unwrap(),
    );
    let script = engine
        .compile("enum Status; Draft; Sent; end; def run; echo([:draft]).name; end")
        .unwrap();
    let shared = Value::array(vec![Value::int(1), Value::bytes(vec![b'x'; 512])]);
    let options = CallOptions {
        globals: (0..20)
            .map(|index| (format!("root{index}"), shared.clone()))
            .collect(),
        ..CallOptions::default()
    };
    (script, options)
}

fn work(ctx: &mut CallContext, script: &Script, options: &CallOptions) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let environment = Environment::new(ctx, &mut facts, script, options)?;
    let report =
        environment.analyze(ctx, &mut facts, script.inner.code.program.names["run"], &[])?;
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn environment_preparation_and_analysis_obey_exact_and_sampled_quotas() {
    let (script, options) = accounting_script();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &script, &options).unwrap();
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
            work(&mut ctx, &script, &options)
                .err()
                .map(|error| error.kind),
            expected
        );
        if let Some(kind) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
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
                work(&mut ctx, &script, &options).unwrap_err().kind,
                expected
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn environment_preparation_preserves_cancellation_and_deadlines() {
    let (script, options) = accounting_script();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let expected = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        let result = Environment::new(&mut ctx, &mut facts, &script, &options);
        assert!(matches!(result,Err(error) if error.kind==expected));
        assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
