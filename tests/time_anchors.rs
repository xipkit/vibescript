use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn anchors_preserve_inputs_and_return_inline_utc_instants() {
    for (name, sign) in [
        ("after", 1),
        ("since", 1),
        ("from_now", 1),
        ("ago", -1),
        ("before", -1),
        ("until", -1),
    ] {
        let script = Engine::new()
            .compile(&format!("def run(input)\n5.minutes.{name}(input)\nend"))
            .unwrap();
        let zoned = Engine::new()
            .compile("Time.at(0,123456789,:nsec,in:\"+05:30\")")
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        for input in [
            Value::time(0, 123456789).unwrap(),
            zoned.value,
            Value::bytes("1970-01-01T05:30:00.123456789+05:30"),
        ] {
            let original = input.to_string();
            let result = script
                .call("run", std::slice::from_ref(&input), CallOptions::default())
                .unwrap();
            assert_eq!(result.value.as_time(), Some((300 * sign, 123456789)));
            assert_eq!(result.stats.retained_memory_bytes, 0);
            assert_eq!(input.to_string(), original);
            assert!(result.value.to_string().ends_with('Z'));
        }
    }
    let result = Engine::new()
        .compile("t=Time.new(2024,3,9,12,0,0,\"America/New_York\");1.days.after(t)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "2024-03-10T17:00:00Z");
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn extreme_duration_anchors_preserve_wrapping_conversion() {
    let script = Engine::new()
        .compile(
            "def after(d)\nd.after(Time.at(0,in:\"UTC\"))\nend\n\
             def before(d)\nd.before(Time.at(0,in:\"UTC\"))\nend",
        )
        .unwrap();
    for (seconds, after, before) in [
        (i64::MIN, (0, 0), (0, 0)),
        (
            -(1 << 54),
            (-9223372037, 145224192),
            (-9223372037, 145224192),
        ),
        (
            -9223372037,
            (9223372036, 709551616),
            (-9223372037, 290448384),
        ),
        (-1, (-1, 0), (1, 0)),
        (0, (0, 0), (0, 0)),
        (1, (1, 0), (-1, 0)),
        (
            9223372037,
            (-9223372037, 290448384),
            (9223372036, 709551616),
        ),
        (1 << 54, (-9223372037, 145224192), (-9223372037, 145224192)),
        (i64::MAX, (-1, 0), (1, 0)),
    ] {
        for (name, expected) in [("after", after), ("before", before)] {
            let result = script
                .call(name, &[Value::duration(seconds)], CallOptions::default())
                .unwrap();
            assert_eq!(result.value.as_time(), Some(expected), "{name}: {seconds}");
            assert_eq!(result.stats.retained_memory_bytes, 0);
        }
    }
}

#[test]
fn long_timestamp_fractions_use_fixed_scratch_and_bounded_work() {
    let script = Engine::new()
        .compile("def run(input)\n0.seconds.after(input)\nend")
        .unwrap();
    for separator in ['.', ','] {
        for digit in ['0', '9'] {
            let text = format!(
                "1970-01-01T00:00:00{separator}{}1Z",
                digit.to_string().repeat(131072)
            );
            let capacity = text.capacity();
            let input = Value::bytes(text.into_bytes());
            let result = script
                .call("run", std::slice::from_ref(&input), CallOptions::default())
                .unwrap();
            assert_eq!(
                result.value.as_time(),
                Some((0, if digit == '0' { 0 } else { 999999999 }))
            );
            assert_eq!(result.stats.retained_memory_bytes, 0);
            assert!(result.stats.peak_memory_bytes < capacity + 12000);
            for (limits, kind) in [
                (
                    Limits {
                        steps: Some(64),
                        ..Limits::default()
                    },
                    ErrorKind::Steps,
                ),
                (
                    Limits {
                        memory_bytes: Some(12000),
                        ..Limits::default()
                    },
                    ErrorKind::Memory,
                ),
            ] {
                assert_eq!(
                    script
                        .call(
                            "run",
                            std::slice::from_ref(&input),
                            CallOptions {
                                limits,
                                ..CallOptions::default()
                            },
                        )
                        .unwrap_err()
                        .kind,
                    kind
                );
            }
        }
    }
}

#[test]
fn clock_defaults_require_calls_and_ignore_attached_blocks() {
    let mut engine = Engine::new();
    engine.register("unexpected", |_, _| panic!("ignored anchor block executed"));
    for (name, before) in [
        ("after", false),
        ("since", false),
        ("from_now", false),
        ("ago", true),
        ("before", true),
        ("until", true),
    ] {
        for suffix in [
            "()",
            "(*[])",
            "(**{})",
            " {unexpected()}",
            " do;unexpected();end",
        ] {
            let script = engine
                .compile(&format!("5.minutes.{name}{suffix}"))
                .unwrap();
            let lower = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let result = script.run(CallOptions::default()).unwrap();
            let upper = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            let (seconds, nanos) = result.value.as_time().unwrap();
            let delta = Duration::from_secs(300);
            let instant = Duration::new(seconds as u64, nanos);
            let clock = if before {
                instant + delta
            } else {
                instant - delta
            };
            assert!(lower <= clock && clock <= upper, "{name}{suffix}");
            assert_eq!(result.stats.retained_memory_bytes, 0);
        }
        for suffix in ["", ".to_s", ".call()"] {
            assert_eq!(
                engine
                    .compile(&format!("1.seconds.{name}{suffix}"))
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::Type
            );
        }
    }
}

#[test]
fn invalid_and_cancelled_anchors_stop_before_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("2024-01-01T00:00:00Z"))
    });
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::bytes("2024-01-01T00:00:00Z"))
    });
    for source in [
        "1.seconds.after(nil)",
        "1.seconds.after(1)",
        "1.seconds.after(Time.utc(2024),Time.utc(2025))",
        "1.seconds.after(in:\"UTC\")",
        "1.seconds.after(\"2024-01-01\")",
        "1.seconds.after(\"2023-02-29T00:00:00Z\")",
        "1.seconds.after(\"2024-01-01T23:59:60Z\")",
        "1.seconds.after(\"2024-01-01T00:00:00.123Ztail\")",
        "1.seconds.after(\"2024-01-01T00:00:00+00:61\")",
    ] {
        assert!(
            engine
                .compile(&format!("{source};effect()"))
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{source}"
        );
    }
    for (argument, kind) in [
        ("cancel()", ErrorKind::Cancelled),
        ("exhaust()", ErrorKind::Steps),
    ] {
        for name in ["after", "before"] {
            assert_eq!(
                engine
                    .compile(&format!("1.seconds.{name}({argument});effect()"))
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err()
                    .kind,
                kind
            );
        }
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn unchanged_upstream_anchor_helpers_accept_strings_and_host_times() {
    let script = Engine::new()
        .compile(include_str!("site/upstream/time/duration.vibe"))
        .unwrap();
    for input in [
        Value::bytes("2024-01-01T00:00:00Z"),
        Value::time(1704067200, 0).unwrap(),
    ] {
        for (method, expected) in [
            ("after_time", "2024-01-01T00:05:00Z"),
            ("ago_time", "2023-12-31T22:00:00Z"),
            ("duration_until", "2023-12-31T22:30:00Z"),
        ] {
            let result = script
                .call(method, std::slice::from_ref(&input), CallOptions::default())
                .unwrap();
            assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
        }
    }
}
