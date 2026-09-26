mod common;

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
    mpsc,
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value};

#[test]
fn scans_account_each_element_without_materializing_a_range() {
    for method in ["sum", "min", "max"] {
        let script = Engine::new()
            .compile(&format!("def run(n: int) -> int?; (1..n).{method}; end"))
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
#[cfg_attr(target_os = "wasi", ignore = "WASI has no threads")]
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
