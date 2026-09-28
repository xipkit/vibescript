mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use vibescript::{CallOptions, Engine, ErrorKind, Value};

#[test]
fn anchors_preserve_inputs_and_return_inline_utc_instants() {
    for (name, sign) in [("after", 1), ("before", -1)] {
        let script = Engine::new()
            .compile(&format!(
                "def run(input: time) -> time\n5.minutes.{name}(input)\nend"
            ))
            .unwrap();
        let zoned = Engine::new()
            .compile("Time.at(0, 123456789, :nanosecond, in:\"+05:30\")")
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        for input in [Value::time(0, 123456789).unwrap(), zoned.value] {
            let original = input.to_string();
            let result = script
                .call("run", std::slice::from_ref(&input), CallOptions::default())
                .unwrap();
            assert_eq!(result.value.as_time(), Some((300 * sign, 123456789)));
            assert_eq!(result.stats.retained_memory_bytes, 0);
            assert_eq!(input.to_string(), original);
            assert!(result.value.to_string().ends_with('Z'));
        }
        // An anchor takes a time, not a timestamp string.
        let error = vibescript::Engine::new()
            .compile(&format!(
                "def run(input: string) -> time\n5.minutes.{name}(input)\nend"
            ))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), ["V0101"], "{name}");
    }
    // since and until are after and before, and ago and from_now count
    // from now only.
    for name in ["since", "from_now", "ago", "until"] {
        let source = format!("def run(input: time) -> time\n5.minutes.{name}(input)\nend");
        let error = vibescript::Engine::new().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0401"], "{name}");
    }
    let result = Engine::new()
        .compile("t=Time.local(2024, 3, 9, 12, 0, 0, in: \"America/New_York\");1.days.after(t)")
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
            "def after(d: duration) -> time\nd.after(Time.at(0,in:\"UTC\"))\nend\n\
             def before(d: duration) -> time\nd.before(Time.at(0,in:\"UTC\"))\nend",
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
fn clock_defaults_count_from_now_and_refuse_other_forms() {
    let engine = Engine::new();
    // `ago` and `from_now` count from now, so they need no parentheses,
    // and empty splats leave them unchanged.
    for (source, before) in [
        ("5.minutes.from_now", false),
        ("5.minutes.ago", true),
        ("5.minutes.from_now(*[])", false),
        ("5.minutes.from_now(**{})", false),
        ("5.minutes.ago(*[])", true),
        ("5.minutes.ago(**{})", true),
    ] {
        let script = engine.compile(source).unwrap();
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
        assert!(lower <= clock && clock <= upper, "{source}");
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
    // The other clock-default forms, parentheses and blocks are refused
    // before anything runs, with or without empty splats.
    let mut checked = vibescript::Engine::new();
    checked.register("unexpected", |_, _| panic!("ignored anchor block executed"));
    for (source, expected) in [
        ("5.minutes.after(*[])", &["V0401"][..]),
        ("5.minutes.since(*[])", &["V0401"]),
        ("5.minutes.since(**{})", &["V0401"]),
        ("5.minutes.before(*[])", &["V0401"]),
        ("5.minutes.until(*[])", &["V0401"]),
        ("5.minutes.until(**{})", &["V0401"]),
        ("5.minutes.after", &["V0401"]),
        ("5.minutes.after()", &["V0401"]),
        ("5.minutes.after(**{})", &["V0401"]),
        ("5.minutes.after {unexpected()}", &["V0301", "V0305"]),
        ("5.minutes.since", &["V0401"]),
        ("5.minutes.since()", &["V0401"]),
        ("5.minutes.since {unexpected()}", &["V0401"]),
        ("5.minutes.from_now()", &["V0412"]),
        ("5.minutes.from_now {unexpected()}", &["V0305"]),
        ("5.minutes.ago()", &["V0412"]),
        ("5.minutes.ago {unexpected()}", &["V0305"]),
        ("5.minutes.before", &["V0401"]),
        ("5.minutes.before()", &["V0401"]),
        ("5.minutes.before(**{})", &["V0401"]),
        ("5.minutes.before {unexpected()}", &["V0301", "V0305"]),
        ("5.minutes.until", &["V0401"]),
        ("5.minutes.until()", &["V0401"]),
        ("5.minutes.until {unexpected()}", &["V0401"]),
        ("1.seconds.after.to_s", &["V0401"]),
        ("1.seconds.after.call()", &["V0401", "V0203"]),
    ] {
        let error = checked.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), expected, "{source}");
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
        Ok(Value::time(1704067200, 0).unwrap())
    });
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::time(1704067200, 0).unwrap())
    });
    for (argument, kind) in [
        ("cancel().as(time)", ErrorKind::Cancelled),
        ("exhaust().as(time)", ErrorKind::Steps),
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
    // Anchors of the wrong type or count, and keywords, are refused before
    // anything runs.
    let mut checked = vibescript::Engine::new();
    checked.register("effect", |_, _| panic!("effect ran"));
    for (source, expected) in [
        ("1.seconds.after(nil)", &["V0101"][..]),
        ("1.seconds.after(1)", &["V0101"]),
        ("1.seconds.after(Time.utc(2024),Time.utc(2025))", &["V0301"]),
        ("1.seconds.after(in:\"UTC\")", &["V0301", "V0302"]),
        ("1.seconds.after(\"2024-01-01T00:00:00Z\")", &["V0101"]),
    ] {
        let error = checked
            .compile(&format!("{source};effect()"))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), expected, "{source}");
    }
}

#[test]
fn upstream_anchor_helpers_take_host_times() {
    let script = Engine::new()
        .compile(include_str!("site/upstream/time/duration.vibe"))
        .unwrap();
    let input = Value::time(1704067200, 0).unwrap();
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
    // The typed helpers refuse the string anchors the untyped ones parsed.
    let text = Value::bytes("2024-01-01T00:00:00Z");
    let error = script
        .call("after_time", &[text], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{error}");
}
