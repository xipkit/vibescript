use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

fn run(source: &str) -> vibescript::Outcome {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
}

#[test]
fn float_strings_match_independent_binary64_rounding_expectations() {
    let script = Engine::new()
        .compile("def convert(input)\nto_float(input)\nend")
        .unwrap();
    // Python float/fromhex supplies these bits independently of either interpreter.
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("float-conversions.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let result = script.call("convert", &[Value::bytes(input)], CallOptions::default());
        if let Some(bits) = case["bits"].as_u64() {
            let result = result.unwrap_or_else(|error| panic!("{input}: {error}"));
            assert_eq!(result.value.type_name(), "float", "{input}");
            assert_eq!(result.value.as_float().unwrap().to_bits(), bits, "{input}");
            assert_eq!(result.stats.retained_memory_bytes, 0);
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Argument, "{input}");
        }
    }
}

#[test]
fn conversion_and_math_preserve_integer_types_and_ieee_special_values() {
    for (source, expected) in [
        (
            "to_int(\" 123456789012345678901234567890 \" )",
            "123456789012345678901234567890",
        ),
        ("to_int(1e30)", "1000000000000000019884624838656"),
        ("to_int(-0.0)", "0"),
    ] {
        let value = run(source).value;
        assert!(value.is_integer(), "{source}");
        assert_eq!(value.to_string(), expected, "{source}");
    }
    for (source, expected) in [
        ("Math.sqrt(4)", 2.0f64),
        ("Math.hypot(3,4)", 5.0),
        ("Math.cbrt(-0.0)", -0.0),
        ("Math.sin(-0.0)", -0.0),
        ("Math.tan(-0.0)", -0.0),
        ("Math.asin(-0.0)", -0.0),
        ("Math.atan(-0.0)", -0.0),
        ("Math.atan2(-0.0,1)", -0.0),
        ("Math.atan2(-0.0,-1)", -std::f64::consts::PI),
        ("Math.log(0)", f64::NEG_INFINITY),
        ("Math.log2(0)", f64::NEG_INFINITY),
        ("Math.exp(-1e309)", 0.0),
        ("Math.hypot(1e309,0.0/0.0)", f64::INFINITY),
        ("to_float(2**2000)", f64::INFINITY),
    ] {
        let value = run(source).value;
        assert_eq!(value.type_name(), "float", "{source}");
        assert_eq!(
            value.as_float().unwrap().to_bits(),
            expected.to_bits(),
            "{source}"
        );
    }
    for source in ["Math.sin(1e309)", "Math.log(1,1)", "to_float(0.0/0.0)"] {
        assert!(run(source).value.as_float().unwrap().is_nan(), "{source}");
    }
}

#[test]
fn namespace_assignments_are_visible_within_one_execution_and_reset_between_calls() {
    let script = Engine::new().compile(
        "def read()\nMath.PI\nend\ndef change()\nMath={PI:7};read()\nend\ndef constants()\n[Math.PI,JSON.keys]\nend"
    ).unwrap();
    for _ in 0..3 {
        assert_eq!(
            script
                .call("change", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
        let result = script
            .call("constants", &[], CallOptions::default())
            .unwrap();
        let values = result.value.as_array().unwrap();
        assert_eq!(values[0].as_float(), Some(std::f64::consts::PI));
        assert_eq!(values[1].to_string(), "[parse, parse_as, stringify]");
    }
    assert_eq!(
        run("m=Math;m.PI=7;[m.PI,Math.PI]").value.to_string(),
        "[7, 3.141592653589793]"
    );
    assert_eq!(run("Math=7;[1].each {Math=8};Math").value.as_int(), Some(7));
    assert_eq!(
        run("[1].each {if false;Math=7;end;Math.PI}")
            .value
            .to_string(),
        "[1]"
    );
    assert_eq!(run("Math.clear;Math=={}").value.to_string(), "false");
    assert_eq!(run("Math.replace({});Math=={}").value.to_string(), "false");
}

#[test]
fn host_and_parameter_bindings_override_builtins_and_blocks_capture_parameters() {
    let mut engine = Engine::new();
    engine.register("to_int", |_, _| Ok(Value::int(11)));
    assert_eq!(
        engine
            .compile("to_int(\"2\")")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(11)
    );
    let script = Engine::new()
        .compile("def run(Math)\n[1].map {[2].map {Math.PI}}\nend")
        .unwrap();
    let input = Value::hash(vec![(b"PI".to_vec(), Value::int(7))]);
    assert_eq!(
        script
            .call("run", &[input], CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "[[7]]"
    );
    assert_eq!(run("f=Math::sqrt;f(9)").value.as_float(), Some(3.0));
    assert_eq!(
        run("Math[\"map\"]=Math[\"sqrt\"];Math.map(9)")
            .value
            .as_float(),
        Some(3.0)
    );
    assert_eq!(
        run("to_int=Math::sqrt;to_int(9)").value.as_float(),
        Some(3.0)
    );
}

#[test]
fn namespaces_retained_by_hosts_keep_their_memory_charge_until_released() {
    let retained = Arc::new(Mutex::new(None));
    let mut engine = Engine::new();
    let held = retained.clone();
    engine.register("retain", move |_, args| {
        *held.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    let held = retained.clone();
    engine.register("release", move |_, _| {
        held.lock().unwrap().take();
        Ok(Value::nil())
    });
    engine.register("tracked", |ctx, _| {
        Ok(Value::int(ctx.stats().retained_memory_bytes as i64))
    });
    let retained_result = engine
        .compile("retain(Math);Math=nil;nil")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(retained_result.stats.retained_memory_bytes > 512);
    let original = retained.lock().unwrap().take().unwrap();
    assert_eq!(original.type_name(), "object");
    let script = Engine::new()
        .compile("def run(input)\ninput.clear;input\nend")
        .unwrap();
    let cleared = script
        .call(
            "run",
            std::slice::from_ref(&original),
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(cleared.value.type_name(), "object");
    assert!(cleared.value.as_hash().unwrap().is_empty());
    assert_eq!(original.as_hash().unwrap().len(), 16);
    let result = engine
        .compile("retain(Math);Math=nil;a=tracked();release();b=tracked();a-b")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(result.value.as_int().unwrap() >= retained_result.stats.retained_memory_bytes as i64);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn replacing_an_ordinary_hash_reuses_the_imported_storage() {
    let input = Value::hash(
        (0..1024)
            .map(|n| (format!("key{n}").into_bytes(), Value::int(n)))
            .collect(),
    );
    let engine = Engine::new();
    let baseline = engine
        .compile("def run(input)\ninput\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let result = engine
        .compile("def run(input)\na={};a.replace(input);nil\nend")
        .unwrap()
        .call("run", &[input], CallOptions::default())
        .unwrap();
    assert!(
        result.stats.peak_memory_bytes < baseline.stats.peak_memory_bytes + 8192,
        "baseline: {:?}, replacement: {:?}",
        baseline.stats,
        result.stats
    );
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn long_float_scans_bound_work_and_temporary_memory() {
    let script = Engine::new()
        .compile("def run(input)\nto_float(input)\nend")
        .unwrap();
    for text in [
        format!("0.{}1e131073", "0".repeat(131072)),
        format!("0x0.{}1p262148", "0".repeat(65536)),
    ] {
        let retained_capacity = text.capacity();
        let value = Value::bytes(text.into_bytes());
        let result = script
            .call("run", std::slice::from_ref(&value), CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_float(), Some(1.0));
        assert_eq!(result.stats.retained_memory_bytes, 0);
        assert!(
            result.stats.peak_memory_bytes < retained_capacity + 12000,
            "{:?}",
            result.stats
        );
        let error = script
            .call(
                "run",
                &[value],
                CallOptions {
                    limits: Limits {
                        steps: Some(32),
                        memory_bytes: Some(1 << 20),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
    }
    let text = format!("0.{}1e32769", "0_".repeat(32768));
    let retained_capacity = text.capacity();
    let value = Value::bytes(text.into_bytes());
    let result = script
        .call("run", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_float(), Some(1.0));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    let error = script
        .call(
            "run",
            std::slice::from_ref(&value),
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(retained_capacity + 12000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
}

#[test]
fn invalid_builtin_calls_fail_before_later_effects_and_do_not_invoke_blocks() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "to_int(1.5)",
        "to_int(\"1_000\")",
        "to_float(\"NaN\")",
        "to_float(:one)",
        "Math.sqrt(-1)",
        "Math.log(2,-1)",
        "Math.asin(2)",
        "Math.sqrt(9,x:1)",
        "Math::sqrt(9,2)",
        "Math.PI()",
        "Math.sqrt",
        "{a:1}::a",
        "Math.sqrt(9) {effect()}",
        "JSON.parse(\"1\") {effect()}",
        "JSON.stringify(1) {effect()}",
        "JSON.parse()",
        "JSON.stringify(1,2)",
        "JSON.parse_as(\"1\",\"int\")",
        "Math=7;Math(9)",
    ] {
        let script = engine.compile(&format!("{expression};effect()")).unwrap();
        assert!(script.run(CallOptions::default()).is_err(), "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn cancellation_and_ignored_quota_errors_prevent_builtin_results() {
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("123"))
    });
    engine.register("ignore", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::int(9))
    });
    for source in [
        "to_float(cancel())",
        "to_int(cancel())",
        "JSON.parse(cancel())",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled, "{source}");
    }
    for source in [
        "Math.sqrt(ignore())",
        "to_float(ignore())",
        "JSON.stringify(ignore())",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{source}");
    }
}
