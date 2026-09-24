use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn run(source: &str) -> vibescript::Outcome {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
}

#[test]
fn float_scaling_matches_independent_exact_rational_expectations() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("duration-float-cases.json")).unwrap();
    let script = Engine::new()
        .compile("def multiply(d,f)\nd*f\nend\ndef divide(d,f)\nd/f\nend")
        .unwrap();
    for case in cases.as_array().unwrap() {
        let seconds = case["seconds"].as_i64().unwrap();
        let factor = f64::from_bits(case["factor_bits"].as_u64().unwrap());
        let function = if case["divide"].as_bool().unwrap() {
            "divide"
        } else {
            "multiply"
        };
        let result = script.call(
            function,
            &[Value::duration(seconds), Value::float(factor)],
            CallOptions::default(),
        );
        if let Some(expected) = case["expected"].as_i64() {
            let result = result.unwrap_or_else(|error| panic!("{case}: {error}"));
            assert_eq!(result.value.as_duration(), Some(expected), "{case}");
            assert_eq!(result.stats.retained_memory_bytes, 0, "{case}");
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Arithmetic, "{case}");
        }
    }
}

#[test]
fn duration_imports_remain_inline_and_preserve_the_host_value() {
    assert_eq!(std::mem::size_of::<Value>(), 16);
    for seconds in [0, 1, -1, i64::MIN, i64::MAX] {
        let value = Value::duration(seconds);
        assert_eq!(value.as_duration(), Some(seconds));
        assert_eq!(value.to_string(), format!("{seconds}s"));
        assert_eq!(value.type_name(), "duration");
    }
    assert_eq!(Value::int(1).as_duration(), None);
    let input = Value::duration(3600);
    let script = Engine::new()
        .compile("def run(input)\ninput+=30.minutes;input\nend")
        .unwrap();
    let result = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert_eq!(input.as_duration(), Some(3600));
    assert_eq!(result.value.as_duration(), Some(5400));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    assert_eq!(
        stringify_json(&input, CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Json
    );
}

#[test]
fn duration_parts_and_strings_retain_accounted_output_storage() {
    let result = run("Duration.build(days:1,hours:2,minutes:3,seconds:4).parts");
    let values = result.value.as_hash().unwrap();
    for ((key, value), (name, expected)) in
        values
            .iter()
            .zip([("days", 1), ("hours", 2), ("minutes", 3), ("seconds", 4)])
    {
        assert_eq!(key.as_bytes(), Some(name.as_bytes()));
        assert_eq!(value.as_int(), Some(expected));
    }
    assert_eq!(values.len(), 4);
    assert!(result.stats.retained_memory_bytes >= 8 * std::mem::size_of::<Value>());
    let rendered = run("Duration.build(days:-1,seconds:-1).iso8601");
    assert_eq!(rendered.value.as_bytes(), Some(b"-P1DT1S".as_slice()));
    assert!(rendered.stats.retained_memory_bytes >= 7);
    let error = Engine::new()
        .compile("(1..1024).map {|n|n.seconds.parts}")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(12000),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
}

#[test]
fn long_duration_parsing_bounds_work_and_uses_fixed_scratch() {
    let script = Engine::new()
        .compile("def run(input)\nDuration.parse(input)\nend")
        .unwrap();
    for (text, expected) in [
        (format!("{}1s", "0".repeat(131072)), 1),
        (format!("0.{}1s", "0".repeat(131072)), 0),
        (format!("PT{}1H", "0".repeat(131072)), 3600),
        (format!("P-{}1W", "0".repeat(131072)), -604800),
        ("0s".repeat(65536), 0),
    ] {
        let capacity = text.capacity();
        let input = Value::bytes(text.into_bytes());
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_duration(), Some(expected));
        assert_eq!(result.stats.retained_memory_bytes, 0);
        assert!(
            result.stats.peak_memory_bytes < capacity + 12000,
            "{:?}",
            result.stats
        );
        let error = script
            .call(
                "run",
                &[input],
                CallOptions {
                    limits: Limits {
                        steps: Some(64),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
    }
}

#[test]
fn negative_literals_bind_before_members_and_preserve_power_precedence() {
    for (expression, expected) in [
        ("-5.abs", "5"),
        ("-1.5.abs", "1.5"),
        ("-5.to_s", "-5"),
        ("-7.negative?", "true"),
        ("-1.minutes", "-60s"),
        ("-9223372036854775808.seconds", "-9223372036854775808s"),
        ("-9223372036854775808.abs", "9223372036854775808"),
        ("-99999999999999999999.abs", "99999999999999999999"),
        ("-2 ** 2", "-4"),
        ("-2\n**2", "-4"),
        ("2 ** -2", "0.25"),
        ("- 5.abs", "-5"),
        ("-\n5.abs", "-5"),
        ("x=5;-x.abs", "-5"),
        ("x=5;x -2", "3"),
    ] {
        assert_eq!(run(expression).value.to_string(), expected, "{expression}");
    }
    for source in ["-(1.seconds)", "- 1.seconds", "+1.seconds"] {
        assert_eq!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
    }
    assert_eq!(
        Engine::new().compile("-;1").err().unwrap().kind,
        ErrorKind::Syntax
    );
}

#[test]
fn invalid_duration_operations_stop_before_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let observed = effects.clone();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "Duration.build",
        "Duration.build(1,seconds:2)",
        "Duration.build(hours:1e309)",
        "Duration.build(2**63)",
        "Duration.parse(\"1.5s\")",
        "Duration.parse(\"9223372037s\")",
        "Duration.parse(\"PT1S1M\")",
        "Duration.build(2**63-1)+1.seconds",
        "Duration.build(-2**63)-1.seconds",
        "Duration.build(-2**63)/-1",
        "Duration.build(2**63-1)*2.0",
        "1.seconds/(0.0/0.0)",
        "1.seconds/0.0",
        "1.seconds/0.seconds",
        "1.seconds%0.seconds",
        "1.seconds.seconds()",
        "1.seconds.iso8601()",
        "1.seconds.to_s {effect()}",
        "1.seconds.equal?(1.seconds) {effect()}",
        "JSON.stringify(1.seconds)",
    ] {
        let result = engine
            .compile(&format!("{source};effect()"))
            .unwrap()
            .run(CallOptions::default());
        assert!(result.is_err(), "{source}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{source}");
    }
}

#[test]
fn duration_builtin_aliases_preserve_keywords_and_do_not_invoke_ignored_blocks() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let observed = effects.clone();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "Duration.build(hours:1,minutes:2) {effect()}",
        "f=Duration::build;f(hours:1,minutes:2) {effect()}",
        "Duration.parse(\"PT1H2M\",ignored:1) {effect()}",
    ] {
        let result = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_duration(), Some(3720));
    }
    let equal = engine
        .compile("1.seconds.eql?(1.seconds) {effect()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(equal.value.to_string(), "true");
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_and_ignored_quota_failures_prevent_duration_results() {
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("1s"))
    });
    engine.register("ignore", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::int(1))
    });
    for (source, expected) in [
        ("Duration.parse(cancel())", ErrorKind::Cancelled),
        ("Duration.build(seconds:ignore())", ErrorKind::Steps),
        ("Duration.parse(\"1s\",ignored:ignore())", ErrorKind::Steps),
        ("1.seconds*ignore()", ErrorKind::Steps),
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, expected, "{source}");
    }
}

#[test]
fn unchanged_duration_example_returns_typed_host_values() {
    let script = Engine::new()
        .compile(include_str!("site/upstream/durations/durations.vibe"))
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    let values = result.value.as_hash().unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0].0.as_bytes(), Some(b"reminder_delay".as_slice()));
    assert_eq!(values[0].1.as_int(), Some(300));
    assert_eq!(values[1].0.as_bytes(), Some(b"event_window".as_slice()));
    assert_eq!(values[1].1.as_duration(), Some(7200));
    assert_eq!(values[2].0.as_bytes(), Some(b"combined".as_slice()));
    assert_eq!(values[2].1.as_int(), Some(900));
}

#[test]
fn bare_duration_builders_and_clock_anchors_run_like_empty_calls() {
    for source in ["Duration.build", "Duration.build()"] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(
            error.message, "Duration.build expects seconds or named parts",
            "{source}"
        );
    }
    let result = run("[5.minutes.from_now > 4.minutes.from_now, 5.minutes.ago < 4.minutes.ago]");
    assert_eq!(
        stringify_json(&result.value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes(),
        Some(b"[true,true]".as_slice())
    );
}
