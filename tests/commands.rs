use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn command_nesting_reaches_its_limit_and_rejects_hostile_input() {
    let source = format!("def id(x)\nx\nend\n{}7", "id ".repeat(64));
    let result = Engine::new()
        .compile(&source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    for depth in [65, 500_000] {
        let source = format!("def run\n{}7\nend", "id ".repeat(depth));
        let error = Engine::new().compile(&source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax);
        assert!(error.message.contains("parenless call nesting too deep"));
    }
}

#[test]
fn syntax_rejections_match_the_reference() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("syntax-errors.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let error = Engine::new()
            .compile(case["source"].as_str().unwrap())
            .err()
            .unwrap_or_else(|| panic!("{} compiled", case["name"]));
        assert_eq!(error.kind, ErrorKind::Syntax, "{}", case["name"]);
    }
}

#[test]
fn assignment_syntax_errors_use_the_reference_text_and_position() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("syntax-errors.json")).unwrap();
    let mut checked = 0;
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if !name.starts_with("assignment_") {
            continue;
        }
        let error = Engine::new()
            .compile(case["source"].as_str().unwrap())
            .err()
            .unwrap_or_else(|| panic!("{name} compiled"));
        let position = error.diagnostic.as_ref().unwrap().position;
        assert_eq!(
            format!(
                "parse error at {}:{}: {}",
                position.line, position.column, error.message
            ),
            case["go_error"].as_str().unwrap(),
            "{name}"
        );
        checked += 1;
    }
    assert_eq!(checked, 71);
    let error = Engine::new().compile("case 1\nend").err().unwrap();
    assert_eq!(error.message, "expected when, got 'end'");
}

#[test]
fn call_parentheses_preserve_argument_accounting_and_exhaustion() {
    let input = Value::array((0..512).map(Value::int).collect());
    let mut baseline = None;
    for invocation in ["sink(*input,mode:true)", "sink *input,mode:true"] {
        let source = format!(
            "def sink(*args,**keywords)\nargs.length+keywords.length\nend\n\
             def run(input)\n{invocation}\nend"
        );
        let script = Engine::new().compile(&source).unwrap();
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(513));
        let stats = result.stats;
        let counters = (
            stats.steps,
            stats.peak_memory_bytes,
            stats.retained_memory_bytes,
        );
        if let Some(expected) = baseline {
            assert_eq!(counters, expected);
        }
        baseline = Some(counters);
        assert_eq!(stats.retained_memory_bytes, 0);
        for (limits, kind) in [
            (
                Limits {
                    steps: Some(stats.steps - 1),
                    ..Limits::default()
                },
                ErrorKind::Steps,
            ),
            (
                Limits {
                    memory_bytes: Some(stats.peak_memory_bytes - 1),
                    ..Limits::default()
                },
                ErrorKind::Memory,
            ),
        ] {
            let error = script
                .call(
                    "run",
                    std::slice::from_ref(&input),
                    CallOptions {
                        limits,
                        ..CallOptions::default()
                    },
                )
                .unwrap_err();
            assert_eq!(error.kind, kind, "{invocation}");
        }
    }
}

#[test]
fn bare_calls_resolve_names_and_cancel_before_later_arguments() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::int(1))
    });
    for source in ["missing tick 0", "missing.push tick 0"] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Name);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let error = engine
        .compile("def sink(*args)\n0\nend\nsink cancel(0),tick(0)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn statement_boundaries_preserve_host_call_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, args| {
        assert_eq!(args.len(), 1);
        let expected = seen.fetch_add(1, Ordering::SeqCst) as i64;
        assert_eq!(args[0].as_int(), Some(expected));
        Ok(Value::int(expected))
    });
    let result = engine
        .compile("tick(0) tick 1\nreturn tick(2), tick 3")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let values = result.value.as_array().unwrap();
    assert_eq!(values[0].as_int(), Some(2));
    assert_eq!(values[1].as_int(), Some(3));
}
