mod common;

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
fn utc_host_values_remain_inline_and_preserve_nanoseconds() {
    assert_eq!(std::mem::size_of::<Value>(), 16);
    for (seconds, nanos, expected) in [
        (0, 0, "1970-01-01T00:00:00Z"),
        (-1, 999999999, "1969-12-31T23:59:59.999999999Z"),
        (0, 123456789, "1970-01-01T00:00:00.123456789Z"),
        (-62135596800, 100, "0001-01-01T00:00:00.0000001Z"),
        (i64::MIN, 0, "292277026596-12-04T15:30:08Z"),
        (i64::MAX, 0, "292277026596-12-04T15:30:07Z"),
    ] {
        let value = Value::time(seconds, nanos).unwrap();
        assert_eq!(value.as_time(), Some((seconds, nanos)));
        assert_eq!(value.type_name(), "time");
        assert_eq!(value.to_string(), expected);
    }
    assert_eq!(Value::int(1).as_time(), None);
    for nanos in [1000000000, u32::MAX] {
        assert_eq!(Value::time(0, nanos).unwrap_err().kind, ErrorKind::Argument);
    }
    let script = Engine::new()
        .compile("def run(input: int | time) -> int | time\ninput\nend")
        .unwrap();
    let time = script
        .call(
            "run",
            &[Value::time(0, 123).unwrap()],
            CallOptions::default(),
        )
        .unwrap();
    let integer = script
        .call("run", &[Value::int(0)], CallOptions::default())
        .unwrap();
    assert_eq!(time.stats.retained_memory_bytes, 0);
    assert_eq!(
        time.stats.peak_memory_bytes,
        integer.stats.peak_memory_bytes
    );
    assert_eq!(
        stringify_json(&time.value, CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Json
    );
}

#[test]
fn fractional_offsets_match_independent_rational_expectations() {
    let script = Engine::new()
        .compile(
            "def microseconds(f: float) -> time\nTime.at(0, f, :microsecond, in:\"UTC\")\nend\n\
         def milliseconds(f: float) -> time\nTime.at(0,f,:millisecond,in:\"UTC\")\nend\n\
         def nanoseconds(f: float) -> time\nTime.at(0, f, :nanosecond, in:\"UTC\")\nend\n\
         def calendar(f: float) -> time\nTime.utc(2024,1,1,0,0,0,f)\nend\n\
         def addition(f: float) -> time\nTime.at(0,in:\"UTC\")+f\nend\n\
         def subtraction(f: float) -> time\nTime.at(0,in:\"UTC\")-f\nend",
        )
        .unwrap();
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("time-float-cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let factor = f64::from_bits(case["factor_bits"].as_u64().unwrap());
        let result = script.call(
            case["method"].as_str().unwrap(),
            &[Value::float(factor)],
            CallOptions::default(),
        );
        if let Some(expected) = case["expected"].as_array() {
            let result = result.unwrap_or_else(|error| panic!("{case}: {error}"));
            assert_eq!(
                result.value.as_time(),
                Some((
                    expected[0].as_i64().unwrap(),
                    expected[1].as_u64().unwrap() as u32
                )),
                "{case}"
            );
            assert_eq!(result.stats.retained_memory_bytes, 0, "{case}");
        } else {
            assert!(result.is_err(), "{case}");
        }
    }
}

#[test]
fn zoned_imports_charge_each_call_and_release_shared_views() {
    for source in [
        "Time.at(0,in:\"+05:30\")",
        "Time.at(0,in:\"America/New_York\")",
    ] {
        let original = run(source);
        assert!(original.stats.retained_memory_bytes > 0);
        let host = original.value.clone();
        let mut engine = Engine::new();
        engine.register("inspect", move |ctx, _| {
            let before = ctx.stats().retained_memory_bytes;
            let first = ctx.import(&host)?;
            let used = ctx.stats().retained_memory_bytes;
            assert!(used > before);
            let clone = ctx.import(&first)?;
            assert_eq!(ctx.stats().retained_memory_bytes, used);
            drop(first);
            assert_eq!(ctx.stats().retained_memory_bytes, used);
            drop(clone);
            assert_eq!(ctx.stats().retained_memory_bytes, before);
            ctx.import(&host)
        });
        let result = engine
            .compile("inspect()")
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_time(), Some((0, 0)));
        assert_eq!(result.value.to_string(), original.value.to_string());
        assert_eq!(
            result.stats.retained_memory_bytes,
            original.stats.retained_memory_bytes
        );
        let utc = engine
            .compile("inspect().as(time).utc")
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(utc.value.to_string(), "1970-01-01T00:00:00Z");
        assert_eq!(utc.stats.retained_memory_bytes, 0);
    }
}

#[test]
fn timezone_arithmetic_shares_rules_without_retaining_abandoned_timestamps() {
    let original = run("Time.local(2024, 3, 9, 12, 0, 0, in: \"America/New_York\")").value;
    let script = Engine::new()
        .compile("def run(t: time,n: int) -> time\nn.times {|i|t+=86400};t\nend")
        .unwrap();
    let short = script
        .call(
            "run",
            &[original.clone(), Value::int(1)],
            CallOptions::default(),
        )
        .unwrap();
    let long = script
        .call(
            "run",
            &[original.clone(), Value::int(1000)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(original.to_string(), "2024-03-09T12:00:00-05:00");
    assert_eq!(short.value.to_string(), "2024-03-10T13:00:00-04:00");
    assert_eq!(
        short.stats.retained_memory_bytes,
        long.stats.retained_memory_bytes
    );
    assert!(
        long.stats.peak_memory_bytes <= short.stats.peak_memory_bytes + 1024,
        "{:?} {:?}",
        short.stats,
        long.stats
    );
    let converted = Engine::new()
        .compile("def run(t: time) -> time\nt.localtime(\"+05:30\")\nend")
        .unwrap()
        .call(
            "run",
            std::slice::from_ref(&original),
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(converted.value.as_time(), original.as_time());
    assert!(converted.stats.retained_memory_bytes < long.stats.retained_memory_bytes);
}

#[test]
fn timezone_loads_and_retained_arrays_obey_memory_and_step_limits() {
    for source in [
        "(0..1000).map {|i|Time.at(i,in:\"UTC\")}",
        "(0..1000).map {|i|Time.at(i,in:\"+05:30\")}",
        "(0..1000).map {|i|Time.at(i,in:\"America/New_York\")}",
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(12000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory, "{source}");
    }
    let script = Engine::new()
        .compile("def run(zone: string) -> time\nTime.at(0,in:zone)\nend")
        .unwrap();
    let input = Value::bytes("a".repeat(1048576));
    let error = script
        .call(
            "run",
            &[input],
            CallOptions {
                limits: Limits {
                    steps: Some(500),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    let zone = run("Time.at(0,in:\"America/New_York\").zone");
    assert_eq!(zone.value.as_bytes(), Some(b"EST".as_slice()));
    assert!(zone.stats.retained_memory_bytes < 512);
}

#[test]
fn invalid_time_operations_and_cancellation_prevent_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("America/New_York"))
    });
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::int(1))
    });
    for source in [
        "Time.at(2**63)",
        "Time.at(2**63-1,1000000,in:\"UTC\")",
        "Time.utc(2024,1,1,0,0,0,1000000)",
        "Time.at(0,in:\"../UTC\")",
        "Time.at(0,in:\"UTC\")+1e100",
        "Time.at(0,in:\"UTC\").iso8601(101)",
        "Time.utc(2024).round(-1)",
        "JSON.stringify(Time.utc(2024))",
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
    for (source, kind) in [
        (
            "Time.at(0,in:cancel().as(string));effect()",
            ErrorKind::Cancelled,
        ),
        ("Time.utc(exhaust().as(int));effect()", ErrorKind::Steps),
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            kind
        );
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    // Arguments of the wrong type, parentheses on an attribute, a block
    // and an unknown keyword are refused before anything runs.
    let mut checked = vibescript::Engine::new();
    checked.register("effect", |_, _| panic!("effect ran"));
    checked.register("exhaust", |_, _| panic!("exhaust ran"));
    for (source, code) in [
        ("Time.at(0,nil)", "V0101"),
        ("Time.utc(nil)", "V0101"),
        ("Time.utc(2024).year()", "V0412"),
        ("Time.utc(2024).to_s {effect()}", "V0305"),
        ("Time.now(ignored:exhaust())", "V0302"),
    ] {
        let error = checked
            .compile(&format!("{source};effect()"))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
}

#[test]
fn blocks_and_clock_aliases_follow_the_call_contracts() {
    let mut engine = Engine::new();
    engine.register("unexpected", |_, _| panic!("ignored block executed"));
    // A block on a time builtin, a removed alias and a local called as a
    // function are refused before anything runs.
    let mut checked = vibescript::Engine::new();
    checked.register("unexpected", |_, _| panic!("ignored block executed"));
    for (source, code) in [
        ("Time.utc(2024) {unexpected()}", "V0305"),
        ("Time.at(0,in:\"UTC\").round {unexpected()}", "V0305"),
        ("Time.utc(2024).iso8601 {unexpected()}", "V0305"),
        (
            "Time.utc(2024).eql?(Time.utc(2024)) {unexpected()}",
            "V0403",
        ),
        ("Time.now()", "V0412"),
        ("now", "V0401"),
        ("now()", "V0401"),
    ] {
        let error = checked.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
    // A clock function named with `::` is a removed spelling first.
    for (source, codes) in [
        ("f=Time::gm;f(2024) {unexpected()}", &["V0416", "V0310"][..]),
        ("f=Time::now;f()", &["V0416", "V0310"]),
    ] {
        let error = checked.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), codes, "{source}");
    }
    for source in [
        "Time.now",
        "f=Time::now;f",
        "def call(f: any) -> any\nf\nend\ncall(Time::now)",
    ] {
        // `::` is refused with static types (V0416) but still runs without.
        engine.set_static_types(vibescript::STATIC_TYPES_BY_DEFAULT && !source.contains("::"));
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let result = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let (seconds, nanos) = result.value.as_time().unwrap();
        let timestamp = std::time::Duration::new(seconds as u64, nanos);
        assert!(before <= timestamp && timestamp <= after);
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
    // The static checker does not report the removed `now` when it is
    // called with a keyword or a block yet, so that call still runs.
    let result = engine
        .compile("now(ignored:1) {unexpected()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let text = result.value.as_bytes().unwrap();
    assert_eq!(text.len(), 20);
    assert_eq!(text[19], b'Z');
    for source in [
        "f=Time::now;f.utc?",
        "now=Time::now;now.utc?",
        "def call(f: any) -> any\nf.as(time).utc?\nend\ncall(Time::now)",
    ] {
        assert!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn time_local_places_its_parts_in_the_requested_zone() {
    let result = run(
        "[Time.local(2024, 1, 2, 3, 4, 5, in: \"Asia/Tokyo\").to_s, \
         Time.local(2024, 7, 1, in: \"+05:30\").to_s, \
         Time.local(2024, 1, 2, in: nil) == Time.local(2024, 1, 2), \
         Time.local(2024, 1, 2, in: \"\") == Time.local(2024, 1, 2), \
         Time.local(2024, 1, 2, 3, 4, 5, 250000, in: \"UTC\").usec]",
    );
    assert_eq!(
        stringify_json(&result.value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes(),
        Some(
            b"[\"2024-01-02T03:04:05+09:00\",\"2024-07-01T00:00:00+05:30\",true,true,250000]"
                .as_slice()
        )
    );
    let error = Engine::new()
        .compile("Time.local(2024, in: \"Nowhere/City\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    let expected = Engine::new()
        .compile("Time.at(0, in: \"Nowhere/City\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, expected.message);
    assert_eq!(error.kind, expected.kind);
}
