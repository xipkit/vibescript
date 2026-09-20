use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc,
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value};

#[test]
fn aggregates_support_automatic_and_dynamic_calls() {
    for (expression, expected) in [
        ("(1..4).sum", 10),
        ("(1..4).sum()", 10),
        ("(1..4).send(:sum,10)", 20),
        ("(1..4).public_send('sum',10)", 20),
        ("(4...1).min", 2),
        ("(1...4).max", 3),
        ("(4..1).send(:min)", 1),
        ("(1..4).public_send(:max)", 4),
        ("(9223372036854775807..9223372036854775807).min", i64::MAX),
        ("(-9223372036854775808..-9223372036854775808).max", i64::MIN),
        ("(0..1).sum(-9223372036854775809)", i64::MIN),
        ("(-1..0).sum(9223372036854775808)", i64::MAX),
    ] {
        let source = format!("def run -> int; {expression}; end");
        let script = Engine::new().compile(&source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(result.value.as_int(), Some(expected), "{expression}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{expression}");
    }
}

#[test]
fn empty_ranges_keep_the_seed_or_return_nil() {
    for seed in ["2**100", "-2**100"] {
        let source = format!("def run; (2...2).sum({seed}) == {seed}; end");
        let script = Engine::new().compile(&source).unwrap();
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
                .to_string(),
            "true"
        );
    }
    for expression in ["(2...2).min", "(2...2).max"] {
        let script = Engine::new()
            .compile(&format!(
                "def run -> int; if {expression} == nil; 7; else; 'wrong'; end; end"
            ))
            .unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{expression}: {report:?}");
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
    }
}

#[test]
fn invalid_calls_fail_before_iteration_or_block_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(0))
    });
    for (expression, message) in [
        (
            "(1..).sum(1,2,k:3){effect()}",
            "range.sum expects at most one argument",
        ),
        (
            "(1..).sum(1.5,k:3){effect()}",
            "range.sum does not take keyword arguments",
        ),
        (
            "(1..).sum(1.5){effect()}",
            "range.sum does not take a block",
        ),
        (
            "(1..).sum(1.5)",
            "range.sum expects an integer initial value",
        ),
        (
            "(1..).min(1,k:3){effect()}",
            "range.min does not take arguments",
        ),
        (
            "(1..).max(k:3){effect()}",
            "range.max does not take keyword arguments",
        ),
        ("(1..).min{effect()}", "range.min does not accept a block"),
        ("(..3).sum", "cannot iterate a beginless range"),
        ("(1..).max", "cannot iterate an endless range"),
        (
            "(1...1).sum('bad')",
            "range.sum expects an integer initial value",
        ),
    ] {
        let script = engine
            .compile(&format!("def run; {expression}; end"))
            .unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(!report.is_clean(), "{expression}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{expression}");
        assert!(error.message.contains(message), "{expression}: {error}");
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn scans_account_each_element_without_materializing_a_range() {
    for method in ["sum", "min", "max"] {
        let script = Engine::new()
            .compile(&format!("def run(n); (1..n).{method}; end"))
            .unwrap();
        let short = script
            .call("run", &[Value::int(100)], CallOptions::default())
            .unwrap();
        let long = script
            .call(
                "run",
                &[Value::int(10000)],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(32_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert!(long.stats.steps >= short.stats.steps + 9900, "{method}");
        assert_eq!(long.stats.retained_memory_bytes, 0);
        assert_eq!(
            long.stats.peak_memory_bytes, short.stats.peak_memory_bytes,
            "{method}"
        );
        let expected = match method {
            "sum" => 50_005_000,
            "min" => 1,
            _ => 10000,
        };
        assert_eq!(long.value.as_int(), Some(expected));
    }
}

#[test]
fn quota_failures_are_latched_and_release_temporary_state() {
    let checkpoint = Arc::new(AtomicU64::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let mark = checkpoint.clone();
    engine.register("mark", move |ctx, _| {
        mark.store(ctx.stats().steps, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in ["(1..1000).sum(2**100)", "(1..1000).min", "(1..1000).max"] {
        let script = engine
            .compile(&format!(
                "mark();begin;{expression};rescue;effect();end;effect()"
            ))
            .unwrap();
        let successful = script.run(CallOptions::default()).unwrap();
        assert_eq!(successful.stats.retained_memory_bytes, 0);
        let steps = checkpoint.load(Ordering::SeqCst) + 100;
        effects.store(0, Ordering::SeqCst);
        let error = script
            .run(CallOptions {
                limits: Limits {
                    steps: Some(steps),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
        assert_eq!(
            script
                .run(CallOptions::default())
                .unwrap()
                .stats
                .retained_memory_bytes,
            0
        );
    }
}

#[test]
fn cancellation_interrupts_all_range_aggregates() {
    for method in ["sum", "min", "max"] {
        let token = CancellationToken::new();
        let (send, receive) = mpsc::channel();
        let cancel = token.clone();
        let worker = std::thread::spawn(move || {
            receive.recv().unwrap();
            cancel.cancel();
        });
        let mut engine = Engine::new();
        engine.register("started", move |_, _| {
            send.send(()).unwrap();
            Ok(Value::nil())
        });
        let script = engine
            .compile(&format!("started();(1..9223372036854775807).{method}"))
            .unwrap();
        let result = script.run(CallOptions {
            cancellation: token,
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        worker.join().unwrap();
        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled, "{method}");
    }
}
