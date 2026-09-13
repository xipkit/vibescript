use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

fn script() -> vibescript::Script {
    Engine::new().compile(
        "def go(t,layout)\nt.format(layout)\nend\ndef percent(t,layout)\nt.strftime(layout)\nend",
    ).unwrap()
}

#[test]
fn calendar_layouts_and_percent_directives_have_independent_expected_results() {
    let script = script();
    let time = Value::time(1704164645, 123456789).unwrap();
    for (method, layout, expected) in [
        (
            "go",
            "2006-01-02T15:04:05.000000000Z07:00",
            "2024-01-02T03:04:05.123456789Z",
        ),
        (
            "go",
            "Monday January _2 15:04:05 MST 2006",
            "Tuesday January  2 03:04:05 UTC 2024",
        ),
        (
            "go",
            "06 002 __2 _2006 Janitor Mondayx",
            "24 002   2 _2024 Janitor Tuesdayx",
        ),
        ("percent", "%F %T.%3N %Z", "2024-01-02 03:04:05.123 UTC"),
        (
            "percent",
            "%Y %C %y %j %w %u %s",
            "2024 20 24 002 2 2 1704164645",
        ),
        (
            "percent",
            "%^c | %#c | %#^p",
            "TUE JAN  2 03:04:05 2024 | Tue Jan  2 03:04:05 2024 | am",
        ),
        (
            "percent",
            "%-12F|%012F|%_6z|%:::z",
            "  2024-01-02|002024-01-02|+00000|+00",
        ),
        ("percent", "%2L|%6N|%12N", "12|123456|123456789000"),
        ("percent", "%Q|%:Y|%::::z|%%", "%Q|%:Y|%::::z|%"),
        ("percent", "%9223372036854775808N", "123456789"),
    ] {
        let result = script
            .call(
                method,
                &[time.clone(), Value::bytes(layout)],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(
            result.value.as_bytes(),
            Some(expected.as_bytes()),
            "{method}: {layout}"
        );
    }
    let result = Engine::new()
        .compile("t=Time.utc(-1,1,2);[t.format(\"2006 06\"),t.strftime(\"%Y|%y|%C|%4Y|%_Y|%-Y\")]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(values[0].as_bytes(), Some(b"-0001 01".as_slice()));
    assert_eq!(
        values[1].as_bytes(),
        Some(b"-0001|99|-1|-001|   -1|-1".as_slice())
    );
}

#[test]
fn crossed_formats_are_rejected_without_misclassifying_literal_text() {
    let script = script();
    let time = Value::time(1704164645, 0).unwrap();
    for layout in ["%Y-%m-%d", "%1000000000N", "%5%", "%#p"] {
        assert_eq!(
            script
                .call(
                    "go",
                    &[time.clone(), Value::bytes(layout)],
                    CallOptions::default()
                )
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
    }
    for layout in ["2006-01-02", "15:04", "prefix 2006 suffix"] {
        assert_eq!(
            script
                .call(
                    "percent",
                    &[time.clone(), Value::bytes(layout)],
                    CallOptions::default()
                )
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
    }
    for (method, layout) in [
        ("go", "%Y%"),
        ("go", "%:Y"),
        ("percent", "Section 3"),
        ("percent", "2026 Report"),
        ("percent", "2006 %Q"),
    ] {
        let result = script
            .call(
                method,
                &[time.clone(), Value::bytes(layout)],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(layout.as_bytes()));
    }
    let reference = Value::time(1136214245, 0).unwrap();
    let result = script
        .call(
            "percent",
            &[reference, Value::bytes("2006-01-02 15:04:05")],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(
        result.value.as_bytes(),
        Some(b"2006-01-02 15:04:05".as_slice())
    );
}

#[test]
fn exact_output_cap_is_supported_and_larger_results_fail() {
    let script = script();
    let time = Value::time(0, 123456789).unwrap();
    for layout in [
        "%1048576N",
        "%1048576F",
        "%1048576Y",
        "%1048576z",
        "%1048576Z",
    ] {
        let result = script
            .call(
                "percent",
                &[time.clone(), Value::bytes(layout)],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.as_bytes().unwrap().len(), 1 << 20, "{layout}");
        assert!(result.stats.retained_memory_bytes >= 1 << 20);
        assert!(result.stats.peak_memory_bytes < (1 << 20) + 12000);
    }
    for layout in [
        "%1048577N".to_owned(),
        "%1000000000F".to_owned(),
        "x".repeat((1 << 20) + 1),
        "%c".repeat(50000),
    ] {
        let result = script.call(
            "percent",
            &[time.clone(), Value::bytes(layout)],
            CallOptions {
                limits: Limits {
                    steps: None,
                    memory_bytes: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        );
        assert_eq!(result.unwrap_err().kind, ErrorKind::OutputLimit);
    }
    let literal = Value::bytes("x".repeat((1 << 20) + 1));
    let result = script
        .call("go", &[time, literal], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_bytes().unwrap().len(), (1 << 20) + 1);
}

#[test]
fn long_formats_release_inputs_and_bound_scanning_and_expansion() {
    let script = script();
    let time = Value::time(1704164645, 0).unwrap();
    for (method, text, expected) in [
        ("percent", format!("%{}d", "1".repeat(131072)), "02"),
        ("percent", format!("%{}d", "-".repeat(131072)), "2"),
        ("go", format!(".{}", "0".repeat(131073)), ".0"),
    ] {
        let capacity = text.capacity();
        let input = Value::bytes(text.into_bytes());
        let result = script
            .call(
                method,
                &[time.clone(), input.clone()],
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
        assert!(result.stats.retained_memory_bytes < 256);
        assert!(result.stats.peak_memory_bytes < capacity + 12000);
        let error = script
            .call(
                method,
                &[time.clone(), input],
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
    for (method, text) in [
        ("percent", "%c".repeat(4096)),
        ("percent", "%A".repeat(16384)),
        ("go", "Monday".repeat(8192)),
        ("go", "x".repeat(65536)),
    ] {
        let error = script
            .call(
                method,
                &[time.clone(), Value::bytes(text)],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(32768),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
    }
}

#[test]
fn formatter_signatures_and_cancellation_prevent_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("%Y"))
    });
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::bytes("%Y"))
    });
    for method in ["format", "strftime"] {
        let layout = if method == "format" { "2006" } else { "%Y" };
        for call in [
            format!("t.{method}(nil)"),
            format!("t.{method}(1)"),
            format!("t.{method}()"),
            format!("t.{method}(\"x\",\"y\")"),
            format!("t.{method}(\"x\",other:1)"),
            format!("t.{method}"),
        ] {
            assert!(
                engine
                    .compile(&format!("t=Time.utc(2024);{call};effect()"))
                    .unwrap()
                    .run(CallOptions::default())
                    .is_err()
            );
        }
        for suffix in [" {effect()}", " do;effect();end"] {
            let result = engine
                .compile(&format!("Time.utc(2024).{method}(\"{layout}\"){suffix}"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap();
            assert_eq!(result.value.as_bytes(), Some(b"2024".as_slice()));
        }
        for (argument, kind) in [
            ("cancel()", ErrorKind::Cancelled),
            ("exhaust()", ErrorKind::Steps),
        ] {
            assert_eq!(
                engine
                    .compile(&format!("Time.utc(2024).{method}({argument});effect()"))
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err()
                    .kind,
                kind
            );
        }
    }
    for expression in [
        "Time.utc(2024).strftime(\"%1048577N\")",
        "Time.utc(2024).iso8601(101)",
    ] {
        assert_eq!(
            engine
                .compile(&format!("{expression};effect()"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}
