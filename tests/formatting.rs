use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

type Writes = Arc<Mutex<Vec<Vec<u8>>>>;

fn engine() -> (Engine, Writes) {
    let mut engine = Engine::new();
    let writes = Writes::default();
    let captured = writes.clone();
    engine.set_output_writer(move |_, bytes| {
        captured.lock().unwrap().push(bytes.to_vec());
        Ok(())
    });
    (engine, writes)
}

fn text(value: &str) -> Value {
    Value::bytes(value.as_bytes().to_vec())
}

fn stats(value: vibescript::Stats) -> (u64, usize, usize) {
    (
        value.steps,
        value.peak_memory_bytes,
        value.retained_memory_bytes,
    )
}

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn format_calls_and_percent_assignments_share_the_renderer() {
    let source = r#"
class C
def %(other)
"class:"+other.to_s
end
end
def wrapped(*args)
sprintf(*args)
end
def run
local="%s:%03d"
local%=["id",7]
a=["%q"]
a[0]%="é\xff"
h={x:"%#O"}
h.x%=8
[
local,a[0],h.x,wrapped("%2$s %1$d",7,"id"),
(missing rescue format)("%s",:ok),
format("%[2]s%[1]s","a","b"),
"%s"%nil,5%2,C.new%7,
format("%#08x|%+08.2f|%#U",31,1.5,233)
]
end
"#;
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            "id:007",
            "\"é\\xff\"",
            "0o010",
            "id 7",
            "ok",
            "ba",
            "",
            1,
            "class:7",
            "0x0000001f|+0001.50|U+00E9 'é'"
        ])
    );
}

#[test]
fn direct_instances_convert_once_in_order_before_pattern_validation() {
    let (engine, writes) = engine();
    let script = engine
        .compile(
            r#"
class C
def initialize(label)
@label=label
end
def to_s
print(@label)
format("<%s>",@label)
end
end
def run(pattern)
begin
format(pattern,C.new("a"),C.new("b"))
rescue RuntimeError=>e
e.message
end
end
"#,
        )
        .unwrap();
    for (pattern, expected, effects) in [
        (text("%s%s"), "<a><b>", "ab"),
        (text("%[2]s"), "<b>", "ab"),
        (text(""), "format has 2 unused operand(s)", "ab"),
        (text("%d"), "format %d expects integer operand", "ab"),
        (Value::int(7), "format expects a string format", ""),
    ] {
        writes.lock().unwrap().clear();
        let result = script
            .call("run", &[pattern], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
        assert_eq!(writes.lock().unwrap().concat(), effects.as_bytes());
    }
}

#[test]
fn nested_instances_percent_and_ineligible_conversions_keep_default_rendering() {
    for definition in [
        "def to_s\nprint(\"called\");7\nend",
        "def to_s\nprint(\"called\");:symbol\nend",
        "def to_s(required)\nraise \"called\"\nend",
        "def to_s(required:)\nraise \"called\"\nend",
    ] {
        let (engine, writes) = engine();
        let script = engine.compile(&format!("class C\n{definition}\nend\ndef run\n[format(\"%s\",C.new),sprintf(\"%s\",[C.new]),\"%s\"%C.new,\"%s\"%[C.new]]\nend")).unwrap();
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([
                "<C instance>",
                "[<C instance>]",
                "<C instance>",
                "<C instance>"
            ])
        );
        assert_eq!(
            writes.lock().unwrap().len(),
            usize::from(definition.contains("print"))
        );
    }
}

#[test]
fn helper_validation_precedes_conversions_and_hosts_can_override_helpers() {
    for helper in ["format", "sprintf"] {
        for (arguments, expected) in [
            ("()", "expects a format string"),
            ("(7,C.new)", "expects a string format"),
            ("(\"%s\",C.new,a:1)", "does not take keyword arguments"),
            ("(\"%s\",C.new) {raise \"block\"}", "does not accept blocks"),
            (
                "(\"%s\",C.new,a:1) {raise \"block\"}",
                "does not take keyword arguments",
            ),
        ] {
            let source =
                format!("class C\ndef to_s\nraise \"converted\"\nend\nend\n{helper}{arguments}");
            let error = Engine::new()
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.message, format!("{helper} {expected}"));
        }
        let mut engine = Engine::new();
        engine.register(helper, |_, args| Ok(args[0].clone()));
        let output = engine
            .compile(&format!("{helper}(7)"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_int(), Some(7));
    }
}

#[test]
fn formatting_helpers_cannot_escape_through_member_calls() {
    for helper in ["format", "sprintf"] {
        for tail in [
            ".call(effect())",
            ".itself(effect())",
            ".send(:itself,effect())",
            "&.public_send(:dup,effect())",
        ] {
            let (mut engine, effects) = engine();
            let captured = effects.clone();
            engine.register("effect", move |_, _| {
                captured.lock().unwrap().push(b"effect".to_vec());
                Ok(Value::nil())
            });
            let error = engine
                .compile(&format!("{helper}{tail}"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(
                error.message,
                format!(
                    "{helper} is a method and cannot be used as a value; call it with {helper}(...)"
                )
            );
            assert!(effects.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn conversion_failures_unwind_before_later_operands_and_allow_reentry() {
    let (mut engine, writes) = engine();
    let inner = Engine::new()
        .compile("def inner\nsprintf(\"%d\",7)\nend")
        .unwrap();
    engine.register("nested", move |_, _| {
        Ok(inner.call("inner", &[], CallOptions::default())?.value)
    });
    let script = engine
        .compile(
            r#"
class C
def initialize(label)
@label=label
end
def to_s
print(@label)
raise "conversion" if @label=="b"
nested()
end
end
def run
begin
format("%s%s%s",C.new("a"),C.new("b"),C.new("c"))
rescue RuntimeError=>e
print(e.message)
ensure
print("ensure")
end
nil
end
"#,
        )
        .unwrap();
    for _ in 0..3 {
        writes.lock().unwrap().clear();
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(writes.lock().unwrap().concat(), b"abconversionensure");
        assert_eq!(output.stats.retained_memory_bytes, 0);
    }
}

#[test]
fn converted_values_remain_accounted_during_later_conversions_and_failure() {
    let mut engine = Engine::new();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let observed = samples.clone();
    engine.register("payload", move |ctx, _| {
        observed
            .lock()
            .unwrap()
            .push(ctx.stats().retained_memory_bytes);
        ctx.bytes(&[b'x'; 8192])
    });
    let script = engine
        .compile(
            r#"
class C
def to_s
payload()
end
end
def run
begin
format("",C.new,C.new,C.new,C.new)
rescue RuntimeError=>e
e.message
end
nil
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    let seen = samples.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(
        seen.windows(2).all(|pair| pair[1] >= pair[0] + 8192),
        "{seen:?}"
    );
    drop(seen);
    let error = script
        .call(
            "run",
            &[],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(30_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    let fresh = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(stats(fresh.stats), stats(output.stats));
}

#[test]
fn fixed_output_limits_are_recoverable_and_padding_obeys_memory_limits() {
    let script = Engine::new()
        .compile(
            r#"
def run(pattern)
begin
format(pattern,"")
rescue LimitError=>e
e.message
end
end
"#,
        )
        .unwrap();
    for (pattern, label) in [("%1048577s", "width"), ("%.1048577s", "precision")] {
        let result = script
            .call("run", &[text(pattern)], CallOptions::default())
            .unwrap();
        assert_eq!(
            result.value.as_bytes(),
            Some(format!("format {label} exceeds limit 1048576 bytes").as_bytes())
        );
        assert!(result.stats.peak_memory_bytes < 10_000);
    }
    let error = script
        .call(
            "run",
            &[text("%1000000s")],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(16_384),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    let result = script
        .call("run", &[text("%s")], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(&b""[..]));
}

#[test]
fn cancellation_in_conversion_cannot_be_rescued_or_run_later_effects() {
    let (mut engine, writes) = engine();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            r#"
class C
def to_s
cancel()
"text"
end
end
begin
format("%s%s",C.new,C.new)
rescue RuntimeError | LimitError
print("rescued")
ensure
print("ensure")
end
"#,
        )
        .unwrap();
    let error = script.run(CallOptions::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert!(writes.lock().unwrap().is_empty());
}

#[test]
fn all_step_and_memory_boundaries_release_conversions_and_preserve_effect_order() {
    let (engine, writes) = engine();
    let script = engine
        .compile(
            r#"
class C
def to_s
print("convert")
"é"
end
end
def run
begin
print(format("%#08x|%10q|%.20f",31,C.new,1.5))
rescue RuntimeError | LimitError
print("rescued")
ensure
print("ensure")
end
nil
end
"#,
        )
        .unwrap();
    let baseline = script.call("run", &[], CallOptions::default()).unwrap();
    let expected = writes.lock().unwrap().clone();
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    for limit in 0..=baseline.stats.steps {
        writes.lock().unwrap().clear();
        let result = script.call(
            "run",
            &[],
            CallOptions {
                limits: Limits {
                    steps: Some(limit),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        );
        if limit < baseline.stats.steps {
            assert_eq!(
                result.unwrap_err().kind,
                ErrorKind::Steps,
                "step budget {limit}"
            );
        } else {
            assert_eq!(result.unwrap().stats.retained_memory_bytes, 0);
        }
        assert!(
            expected.starts_with(&writes.lock().unwrap()),
            "step budget {limit}"
        );
    }
    let peak = baseline.stats.peak_memory_bytes;
    for limit in (0..peak).step_by(64).chain([peak - 1, peak]) {
        writes.lock().unwrap().clear();
        let result = script.call(
            "run",
            &[],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(limit),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        );
        if limit < peak {
            assert_eq!(
                result.unwrap_err().kind,
                ErrorKind::Memory,
                "memory budget {limit}"
            );
        } else {
            assert_eq!(result.unwrap().stats.retained_memory_bytes, 0);
        }
        assert!(
            expected.starts_with(&writes.lock().unwrap()),
            "memory budget {limit}"
        );
    }
    writes.lock().unwrap().clear();
    let fresh = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(stats(fresh.stats), stats(baseline.stats));
    assert_eq!(*writes.lock().unwrap(), expected);
}

#[test]
fn reference_output_boundaries_and_pointer_shapes_remain_stable() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../docs/formatting-boundaries.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let (engine, writes) = engine();
        let output = engine
            .compile(case["source"].as_str().unwrap())
            .unwrap()
            .call(
                "run",
                &[Value::nil()],
                CallOptions {
                    limits: Limits {
                        steps: None,
                        memory_bytes: Some(64 << 20),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{}: {e}", case["name"]));
        assert_eq!(json(&output.value), case["expected"], "{}", case["name"]);
        let bytes = writes.lock().unwrap().concat();
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            case["stdout_hex"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(case["stderr_hex"], "");
    }
}

#[test]
fn known_formatting_differences_keep_explicit_expected_results() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../docs/formatting-differences.json")).unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let (engine, _) = engine();
        let output = engine
            .compile(case["source"].as_str().unwrap())
            .unwrap()
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), case["expected"], "{}", case["name"]);
    }
}
