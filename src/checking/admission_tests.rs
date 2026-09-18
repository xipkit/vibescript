use super::{
    admission,
    calls::{self, Target, World},
    facts::{Atom, Callable, Facts, HashKind, Node},
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, Error, ErrorKind, HostMethod, Limits, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn produced(source: &str) -> Value {
    Engine::new()
        .compile(source)
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap()
        .value
}

fn witness(input: &Value, expression: &str, expected: &str, rejected: bool) {
    let script = Engine::new()
        .compile(&format!("def run; {expression}; end"))
        .unwrap();
    let program = &script.inner.code.program;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let value = admission::value(&mut ctx, &mut facts, input, |_, _, _| Ok(None)).unwrap();
    assert!(!value.incomplete, "{input:?}");
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let value = facts.annotation(&mut ctx, ty, |_, _| Ok(None)).unwrap();
        contracts.push(&mut ctx, value).unwrap();
    }
    let name = Value::bytes(b"input");
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            inputs: &[],
            source_owner: 0,
            program,
            contracts: &contracts.data,
            hosts: &[],
            globals: &[(name, Target::Value(value.value))],
        },
        program.names["run"],
        &[],
    )
    .unwrap();
    assert!(
        report.incomplete.data.is_empty(),
        "{expression}: {report:?}"
    );
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{expression}: {report:?}"
    );
    let result = script
        .call(
            "run",
            &[],
            CallOptions {
                globals: [("input".into(), input.clone())].into(),
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.to_string(), expected, "{expression}");
    let actual = observed(&mut ctx, &mut facts, program, &result.value);
    assert_ne!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Rejected,
        "{expression}: {report:?}"
    );
    drop((report, contracts, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn admitted_scalars_preserve_literals_and_execute_known_operations() {
    for (input, expression, expected) in [
        (Value::int(7), "input+2", "9"),
        (Value::boolean(false), "input ? missing : 9", "9"),
        (Value::nil(), "input || 9", "9"),
        (Value::bytes(b"hello"), "input.length", "5"),
        (Value::symbol(b"hello"), "input.to_s", "hello"),
    ] {
        witness(&input, expression, expected, false);
    }
    for value in [0.0, -0.0, 1.5, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let source = Value::float(value);
        let result = admission::value(&mut ctx, &mut facts, &source, |_, _, _| Ok(None)).unwrap();
        assert!(!result.incomplete);
        assert!(matches!(facts.node(result.value), Node::Float(bits) if *bits == value.to_bits()));
    }
}

#[test]
fn admitted_hash_keys_preserve_string_symbol_and_raw_byte_representations() {
    let canonical = produced("def run; h={}; h[:symbol]=7; h[\"string\"]=9; h; end");
    let Kind::Hash(hash) = &canonical.0 else {
        panic!()
    };
    assert!(matches!(hash.buffer.data[0].0.0, Kind::Bytes(_)));
    assert!(matches!(hash.buffer.data[1].0.0, Kind::Bytes(_)));
    witness(&canonical, "input.keys", "[symbol, string]", false);
    let input = Value(Kind::Hash(crate::hash::Hash::untracked(
        vec![
            (Value::symbol(b"symbol"), Value::int(7)),
            (Value::bytes(b"string"), Value::int(9)),
        ],
        1,
    )));
    witness(&input, "input.keys", "[symbol, string]", false);
    witness(&input, "input.values", "[7, 9]", false);
    witness(&input, "input[:string]+input[\"symbol\"]", "16", false);
    let input = Value::hash(vec![(vec![0xff, 0], Value::int(7))]);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = admission::value(&mut ctx, &mut facts, &input, |_, _, _| Ok(None)).unwrap();
    let Node::Shape(fields, false, keys, HashKind::Plain) = facts.node(result.value) else {
        panic!("{result:?}")
    };
    assert_eq!(fields.data[0].name.as_bytes(), Some(&[0xff, 0][..]));
    assert!(
        matches!(facts.node(*keys), Node::String(value) if value.as_bytes() == Some(&[0xff,0][..]))
    );
}

#[test]
fn admitted_objects_keep_member_overrides_and_nested_value_copies() {
    let input = Value::object(vec![
        (b"length".to_vec(), Value::int(7)),
        (b"items".to_vec(), Value::array(vec![Value::int(1)])),
    ]);
    witness(&input, "input.length", "7", false);
    witness(
        &input,
        "copy=input; copy.items.push(2); input.items",
        "[1]",
        false,
    );
    witness(
        &input,
        "copy=input; copy.items.push(2); copy.items",
        "[1, 2]",
        false,
    );
}

#[test]
fn admitted_ranges_keep_bounds_and_extreme_endpoints() {
    for (input, expected) in [
        (Value::range(Some(1), Some(3), false), "[1, 2, 3]"),
        (Value::range(Some(1), Some(3), true), "[1, 2]"),
        (
            Value::range(Some(i64::MAX), Some(i64::MAX), false),
            "[9223372036854775807]",
        ),
    ] {
        witness(
            &input,
            "a=[]; for n in input; a.push(n); end; a",
            expected,
            false,
        );
    }
    let input = Value::range(None, Some(3), true);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let value = admission::value(&mut ctx, &mut facts, &input, |_, _, _| Ok(None)).unwrap();
    assert!(matches!(
        facts.node(value.value),
        Node::Range(None, Some(3), true)
    ));
}

#[test]
fn admitted_regexes_retain_patterns_flags_and_capture_structure() {
    let input = Value::regex(b"(?P<word>a+)", "i").unwrap();
    witness(&input, "input.source", "(?P<word>a+)", false);
    witness(&input, "input.match(\"AA\")&.captures", "[AA]", false);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let admitted = admission::value(&mut ctx, &mut facts, &input, |_, _, _| Ok(None)).unwrap();
    let Node::Regex(value) = facts.node(admitted.value) else {
        panic!()
    };
    assert_eq!(value.as_regex(), input.as_regex());
    assert!(ctx.stats().retained_memory_bytes > 0);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn admitted_match_data_and_errors_keep_protection_and_nested_fields() {
    let matched = produced("def run; /(?P<word>a)(b)?/.match(\"a\"); end");
    witness(&matched, "input.captures", "[a, nil]", false);
    witness(&matched, "input.begin(0)", "0", false);
    witness(
        &matched,
        "begin; input.captures.push(\"bad\"); rescue; input.captures; end",
        "[a, nil]",
        true,
    );
    let error =
        produced("def run; begin; raise ArgumentError, \"bad\"; rescue => error; error; end; end");
    witness(&error, "input.message", "bad", false);
    witness(
        &error,
        "begin; input.clear; rescue; input.message; end",
        "bad",
        true,
    );
}

#[test]
fn admitted_enums_keep_definition_identity_across_imported_copies() {
    let input = produced("enum Status; Draft; Sent; end; def run; [Status, Status::Draft]; end");
    let mut ctx = CallContext::new(CallOptions::default());
    let copy = ctx.import(&input).unwrap();
    let mut facts = Facts::new(&mut ctx).unwrap();
    let first = admission::value(&mut ctx, &mut facts, &input, |_, _, _| Ok(None)).unwrap();
    let second = admission::value(&mut ctx, &mut facts, &copy, |_, _, _| Ok(None)).unwrap();
    assert_eq!(first.value, second.value);
    let Node::Tuple(elements) = facts.node(first.value) else {
        panic!()
    };
    assert!(
        matches!(facts.node(elements.data[1]), Node::EnumMember { enumeration, index: Some(0) } if *enumeration == elements.data[0])
    );
    drop((facts, copy));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn admitted_type_literals_preserve_contracts_and_flag_unresolved_names() {
    for (spelling, incomplete) in [
        ("array<int>", false),
        ("{name:string}", false),
        ("array<Missing>", true),
    ] {
        let ty = crate::syntax::parse_type(spelling).unwrap();
        let value = crate::shapes::compile(ty);
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let admitted = admission::value(&mut ctx, &mut facts, &value, |_, _, _| Ok(None)).unwrap();
        assert_eq!(admitted.incomplete, incomplete, "{spelling}");
        assert!(matches!(facts.node(admitted.value), Node::TypeValue(_)));
    }
}

#[test]
fn unresolved_nominal_values_stay_explicit_without_running_initializers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, _| {
        invoked.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile("class Widget; mark(); end; def run; Widget; end")
        .unwrap();
    let namespace = &script.inner.code.program.declarations[0];
    let input = Value::array(vec![Value::int(7), namespace.clone(), namespace.clone()]);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut resolutions = 0;
    let result = admission::value(&mut ctx, &mut facts, &input, |_, _, _| {
        resolutions += 1;
        Ok(None)
    })
    .unwrap();
    assert!(result.incomplete);
    assert_eq!(resolutions, 1);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let Node::Tuple(elements) = facts.node(result.value) else {
        panic!()
    };
    assert!(matches!(facts.node(elements.data[0]), Node::Integer(7)));
    assert_eq!(elements.data[1], Atom::Unknown.fact());
}

#[test]
fn shared_host_graphs_are_walked_once_without_callbacks_or_native_recursion() {
    let callback = HostMethod::new("never", |_, _, _| panic!("host callback executed"));
    let mut input = callback.value();
    for _ in 0..MAX_VALUE_DEPTH {
        input = Value::array(vec![input.clone(), input]);
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut resolutions = 0;
    let result = admission::value(&mut ctx, &mut facts, &input, |ctx, facts, _| {
        resolutions += 1;
        facts.callable(ctx, 42, Callable::Host(0)).map(Some)
    })
    .unwrap();
    assert!(!result.incomplete);
    assert_eq!(resolutions, 1);
    let mut cursor = result.value;
    for _ in 0..MAX_VALUE_DEPTH {
        let Node::Tuple(elements) = facts.node(cursor) else {
            panic!()
        };
        assert_eq!(elements.data.len(), 2);
        assert_eq!(elements.data[0], elements.data[1]);
        cursor = elements.data[0];
    }
    assert!(matches!(facts.node(cursor), Node::Callable { .. }));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn identical_storage_with_distinct_value_kinds_does_not_share_a_fact() {
    let string = Value::bytes([0xff, 0]);
    let Kind::Bytes(bytes) = &string.0 else {
        panic!()
    };
    let symbol = Value(Kind::Symbol(bytes.clone()));
    let input = Value::array(vec![string, symbol]);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = admission::value(&mut ctx, &mut facts, &input, |_, _, _| Ok(None)).unwrap();
    let Node::Tuple(elements) = facts.node(result.value) else {
        panic!()
    };
    assert_ne!(elements.data[0], elements.data[1]);
    assert!(matches!(facts.node(elements.data[0]), Node::String(_)));
    assert!(matches!(facts.node(elements.data[1]), Node::Symbol(_)));
}

#[test]
fn excessive_value_depth_is_recoverable_and_never_hidden_by_shared_storage() {
    let mut input = Value::nil();
    for _ in 0..=MAX_VALUE_DEPTH {
        input = Value::array(vec![input]);
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let error = admission::value(&mut ctx, &mut facts, &input, |_, _, _| Ok(None)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    ctx.checkpoint().unwrap();
    let good = Value::int(7);
    assert!(
        !admission::value(&mut ctx, &mut facts, &good, |_, _, _| Ok(None))
            .unwrap()
            .incomplete
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting_input() -> Value {
    let leaf = Value::object(vec![
        (b"text".to_vec(), Value::bytes(vec![b'x'; 1024])),
        (b"pattern".to_vec(), Value::regex(b"(a+)(b)?", "").unwrap()),
    ]);
    Value::array(
        (0..40)
            .map(|index| Value::array(vec![Value::int(index), leaf.clone()]))
            .collect(),
    )
}

fn work(ctx: &mut CallContext, input: &Value) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let value = admission::value(ctx, &mut facts, input, |_, _, _| Ok(None))?;
    assert!(!value.incomplete);
    Ok(())
}

#[test]
fn admission_has_exact_limits_and_reclaims_interrupted_tables_and_facts() {
    let input = accounting_input();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &input).unwrap();
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
            work(&mut ctx, &input).err().map(|error| error.kind),
            expected
        );
        if let Some(kind) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..32 {
        for memory in [false, true] {
            let mut limits = Limits::default();
            let expected = if memory {
                limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 32);
                ErrorKind::Memory
            } else {
                limits.steps = Some(stats.steps * sample as u64 / 32);
                ErrorKind::Steps
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(work(&mut ctx, &input).unwrap_err().kind, expected);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn admission_preserves_cancelled_and_expired_contexts_before_allocating() {
    let input = accounting_input();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        ctx.options.limits.memory_bytes = Some(0);
        let expected = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        let before = ctx.stats().peak_memory_bytes;
        assert_eq!(
            admission::value(&mut ctx, &mut facts, &input, |_, _, _| panic!())
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(ctx.stats().peak_memory_bytes, before);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn metadata_resolution_errors_release_work_and_allow_a_fresh_analysis() {
    let method = HostMethod::new("never", |_, _, _| panic!()).value();
    let input = Value::array(vec![Value::bytes(vec![b'x'; 1024]), method.clone(), method]);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let error = admission::value(&mut ctx, &mut facts, &input, |_, _, _| {
        Err(Error::new(ErrorKind::Type, "unknown owner"))
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    ctx.checkpoint().unwrap();
    let mut calls = 0;
    let result = admission::value(&mut ctx, &mut facts, &input, |ctx, facts, _| {
        calls += 1;
        facts.callable(ctx, 42, Callable::Host(0)).map(Some)
    })
    .unwrap();
    assert!(!result.incomplete);
    assert_eq!(calls, 1);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
