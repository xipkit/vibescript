mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn default_and_custom_layouts_preserve_independently_known_instants() {
    let script = Engine::new()
        .compile("def run(text: string,layout: string?) -> time\nTime.parse(text,layout,in:\"UTC\")\nend")
        .unwrap();
    for (text, layout, seconds, nanos) in [
        ("1970-01-01", None, 0, 0),
        ("1970/01/01 00:00:01", None, 1, 0),
        ("01/01/1970 00:00:02", None, 2, 0),
        ("1970-01-01T05:30:00.123456789+05:30", None, 0, 123456789),
        ("Thu, 01 Jan 1970 05:30:00 +0530", None, 0, 0),
        ("Tue, 01 Jan 1970 00:00:00 UTC", None, 0, 0),
        (
            "2024 060 11:59:58 PM",
            Some("2006 002 03:04:05 PM"),
            1709251198,
            0,
        ),
        ("29 FEBRUARY 2024", Some("2 January 2006"), 1709164800, 0),
        ("0000-01-01", None, -62167219200, 0),
        ("69", Some("06"), -31536000, 0),
        ("", Some(""), -62167219200, 0),
        ("1970 00,123456789012345", Some("2006 05"), 0, 123456789),
    ] {
        let input = Value::bytes(text);
        let original = input.to_string();
        let result = script
            .call(
                "run",
                &[input.clone(), layout.map(Value::bytes).unwrap_or_default()],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.as_time(), Some((seconds, nanos)), "{text}");
        assert_eq!(result.stats.retained_memory_bytes, 0);
        assert_eq!(input.to_string(), original);
    }
}

#[test]
fn explicit_zones_resolve_wall_times_and_preserve_offset_instants() {
    let script = Engine::new()
        .compile("def run(text: string,layout: string?,zone: string) -> time\nTime.parse(text,layout,in:zone)\nend")
        .unwrap();
    for (text, layout, zone, seconds, rendered) in [
        (
            "1970-01-01T00:00:00Z",
            None,
            "+05:30",
            0,
            "1970-01-01T05:30:00+05:30",
        ),
        (
            "1970-01-01 05:30:00",
            None,
            "+05:30",
            0,
            "1970-01-01T05:30:00+05:30",
        ),
        (
            "2024-03-10 03:30:00",
            None,
            "America/New_York",
            1710055800,
            "2024-03-10T03:30:00-04:00",
        ),
        (
            "2024-07-02 12:00:00 EDT",
            Some("2006-01-02 15:04:05 MST"),
            "America/New_York",
            1719936000,
            "2024-07-02T12:00:00-04:00",
        ),
        (
            "1970-01-01T00:00:00-00:00",
            None,
            "UTC",
            0,
            "1970-01-01T00:00:00Z",
        ),
    ] {
        let result = script
            .call(
                "run",
                &[
                    Value::bytes(text),
                    layout.map(Value::bytes).unwrap_or_default(),
                    Value::bytes(zone),
                ],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.as_time(), Some((seconds, 0)), "{text}");
        assert_eq!(result.value.to_string(), rendered);
    }
    let script = Engine::new().compile(
        "def run(zone: string?) -> array<string>\nt=Time.parse(\"1970-01-01T00:00:00-00:00\",in:zone);[t.rfc2822,t.zone]\nend",
    ).unwrap();
    for (zone, suffix, name) in [
        (Value::nil(), "-0000", "-00:00"),
        (Value::bytes(""), "-0000", "-00:00"),
        (Value::bytes("UTC"), "-0000", "UTC"),
    ] {
        let result = script.call("run", &[zone], CallOptions::default()).unwrap();
        let values = result.value.as_array().unwrap();
        assert!(values[0].as_bytes().unwrap().ends_with(suffix.as_bytes()));
        assert_eq!(values[1].as_bytes(), Some(name.as_bytes()));
    }
}

#[test]
fn long_inputs_and_layouts_use_bounded_scratch_and_release_storage() {
    let script = Engine::new()
        .compile("def run(text: string,layout: string?) -> time\nTime.parse(text,layout,in:\"UTC\")\nend")
        .unwrap();
    for (text, layout, seconds, nanos) in [
        (
            format!("1970-01-01T00:00:00.{}Z", "1".repeat(131072)),
            None,
            0,
            111111111,
        ),
        (
            format!("{}1970", "x".repeat(131072)),
            Some(format!("{}2006", "x".repeat(131072))),
            0,
            0,
        ),
        (
            format!("1970{}01", " ".repeat(131072)),
            Some("2006 01".to_owned()),
            0,
            0,
        ),
        (
            format!("GMT+{}1", "0".repeat(131072)),
            Some("MST".to_owned()),
            -62167219200,
            0,
        ),
    ] {
        let capacity = text.capacity() + layout.as_ref().map_or(0, String::capacity);
        let args = [
            Value::bytes(text.into_bytes()),
            layout
                .map(|s| Value::bytes(s.into_bytes()))
                .unwrap_or_default(),
        ];
        let result = script.call("run", &args, CallOptions::default()).unwrap();
        assert_eq!(result.value.as_time(), Some((seconds, nanos)));
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
                        &args,
                        CallOptions {
                            limits,
                            ..CallOptions::default()
                        }
                    )
                    .unwrap_err()
                    .kind,
                kind
            );
        }
    }
}

#[test]
fn parsed_zone_imports_charge_each_call_and_release_abandoned_headers() {
    for source in [
        "Time.parse(\"1970-01-01 +0530\",\"2006-01-02 -0700\")",
        "Time.parse(\"1970-01-01T00:00:00Z\",in:\"America/New_York\")",
    ] {
        let original = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        let host = original.value.clone();
        let mut engine = Engine::new();
        engine.register("host_time", move |ctx, _| ctx.import(&host));
        let script = engine
            .compile("def run(n: int) -> time\nt=host_time().as(time);n.times {t=t+1;t=t-1};t\nend")
            .unwrap();
        let short = script
            .call("run", &[Value::int(1)], CallOptions::default())
            .unwrap();
        let long = script
            .call("run", &[Value::int(100)], CallOptions::default())
            .unwrap();
        assert_eq!(short.value.as_time(), original.value.as_time());
        assert_eq!(long.value.to_string(), original.value.to_string());
        assert_eq!(
            short.stats.retained_memory_bytes,
            original.stats.retained_memory_bytes
        );
        assert_eq!(
            long.stats.retained_memory_bytes,
            short.stats.retained_memory_bytes
        );
        assert!(long.stats.peak_memory_bytes <= short.stats.peak_memory_bytes + 1024);
        let utc = engine
            .compile("host_time().as(time).utc")
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(utc.stats.retained_memory_bytes, 0);
    }
}

#[test]
fn aliases_parse_and_exhaustion_stops_before_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("1970-01-01"))
    });
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::bytes("1970-01-01"))
    });
    let result = engine
        .compile("ns=Time;ns.parse(\"1970-01-01\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_time(), Some((0, 0)));
    for argument in ["\"2023-02-29\"", "\"2024-01-01T23:59:60Z\""] {
        let script = engine
            .compile(&format!("Time.parse({argument});effect()"))
            .unwrap();
        assert_eq!(
            script.run(CallOptions::default()).unwrap_err().kind,
            ErrorKind::Argument
        );
    }
    for (call, kind) in [
        ("cancel().as(string)", ErrorKind::Cancelled),
        ("exhaust().as(string)", ErrorKind::Steps),
    ] {
        let script = engine
            .compile(&format!("Time.parse({call});effect()"))
            .unwrap();
        assert_eq!(script.run(CallOptions::default()).unwrap_err().kind, kind);
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    // The parser read as a value, a block it never takes and arguments of
    // the wrong type are refused before anything runs.
    let mut checked = vibescript::Engine::new();
    checked.register("effect", |_, _| panic!("effect ran"));
    for (source, expected) in [
        ("Time::parse(\"1970-01-01\")", &["V0416"][..]),
        (
            "f=Time::parse;f(\"1970-01-01\") {effect()}",
            &["V0416", "V0301", "V0310"],
        ),
        ("ns=Time;ns.parse(\"1970-01-01\") {effect()}", &["V0305"]),
        (
            "Time::parse(\"1970-01-01\") {effect()}",
            &["V0416", "V0305"],
        ),
        ("Time.parse(nil);effect()", &["V0101"]),
        ("Time.parse(1);effect()", &["V0101"]),
        ("Time.parse(true);effect()", &["V0101"]),
        ("Time.parse([]);effect()", &["V0101"]),
        ("Time.parse({});effect()", &["V0101"]),
        ("Time.parse(:\"1970-01-01\");effect()", &["V0101"]),
        (
            "Time.parse(\"2024-01-01\",:\"2006-01-02\");effect()",
            &["V0101"],
        ),
    ] {
        let error = checked.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), expected, "{source}");
    }
}
