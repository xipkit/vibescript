use std::sync::{
    Arc,
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
fn rounding_preserves_types_decimal_boundaries_and_signed_zero() {
    for (source, expected) in [
        ("1.005.round(2)", 1.01f64),
        ("1.005.floor(3)", 1.005),
        ("(-1.005).ceil(3)", -1.004),
        ("(-0.0).round(5)", -0.0),
        ("(-0.01).ceil(1)", -0.0),
        ("(-1e-300).round(2)", 0.0),
        ("(-1e-300).ceil(2)", 0.0),
        ("(-1e-300).floor(2)", -0.01),
        ("1e-308.round(320)", 1e-308),
        ("1.234567890123455.round(15)", 1.234567890123455),
    ] {
        let value = run(source).value;
        assert_eq!(value.type_name(), "float", "{source}");
        assert_eq!(
            value.as_float().unwrap().to_bits(),
            expected.to_bits(),
            "{source}"
        );
    }
    for (source, expected) in [
        ("(-2.5).round", "-3"),
        ("123.round(3)", "123"),
        ("(-0.0).floor", "0"),
        ("1e30.floor", "1000000000000000019884624838656"),
        ("9223372036854775807.ceil(-1)", "9223372036854775810"),
        ("(-9223372036854775808).floor(-1)", "-9223372036854775810"),
        ("9223372036854775807.succ", "9223372036854775808"),
        ("(-9223372036854775808).pred", "-9223372036854775809"),
    ] {
        let value = run(source).value;
        assert!(value.is_integer(), "{source}");
        assert_eq!(value.to_string(), expected, "{source}");
    }
    for source in ["1e-308.floor(320)", "1e-308.ceil(320)"] {
        assert!(run(source).value.as_float().unwrap().is_nan());
    }
}

#[test]
fn division_helpers_preserve_ieee_values_and_their_distinct_sign_rules() {
    let result = run(
        "[(-7).divmod(3),7.divmod(-3),(-7).remainder(3),7.remainder(-3),(-7).modulo(3),7.modulo(-3)]",
    );
    assert_eq!(
        result.value.to_string(),
        "[[-3, 2], [-3, -2], -1, 1, 2, -2]"
    );
    for (source, expected) in [
        ("(-1.0).div(1e309)", "0"),
        ("(-9223372036854775808).remainder(-1)", "0"),
        (
            "(-9223372036854775808).divmod(-1)",
            "[9223372036854775808, 0]",
        ),
    ] {
        assert_eq!(run(source).value.to_string(), expected, "{source}");
    }
    for (source, quotient, modulo) in [
        ("(-1.0).divmod(1e309)", -1, f64::INFINITY),
        ("1.0.divmod(1e309)", 0, 1.0),
    ] {
        let result = run(source);
        let values = result.value.as_array().unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].as_int(), Some(quotient));
        assert_eq!(values[1].type_name(), "float");
        assert_eq!(values[1].as_float(), Some(modulo));
    }
    for source in ["0.fdiv(0)", "(-1.0).remainder(1e309)", "1e309.modulo(2)"] {
        assert!(run(source).value.as_float().unwrap().is_nan(), "{source}");
    }
    assert_eq!(
        run("1.fdiv(-0.0)").value.as_float(),
        Some(f64::NEG_INFINITY)
    );
    assert_eq!(
        run("(-0.0).remainder(2)")
            .value
            .as_float()
            .unwrap()
            .to_bits(),
        (-0.0f64).to_bits()
    );
}

#[test]
fn clamp_compares_bounds_exactly_and_preserves_the_selected_value_type() {
    let value = run("9007199254740993.clamp(nil,9007199254740992.0)").value;
    assert_eq!(value.type_name(), "float");
    assert_eq!(value.as_float(), Some(9007199254740992.0));
    let value = run("9007199254740992.0.clamp(9007199254740993,nil)").value;
    assert_eq!(value.as_int(), Some(9007199254740993));
    let value = run("(0.0/0.0).clamp(nil,nil)").value;
    assert!(value.as_float().unwrap().is_nan());
    assert_eq!(run("1.between?(2,\"unused\")").value.to_string(), "false");
}

#[test]
fn extreme_rounding_rejects_growth_before_materializing_a_bucket() {
    for source in [
        "1.ceil(-2147483648)",
        "(-1).floor(-2147483648)",
        "1.5.ceil(-1000000000)",
        "(-1.5).floor(-1000000000)",
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: None,
                    memory_bytes: Some(32 << 10),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory, "{source}");
    }
    for source in [
        "1.round(-2147483648)",
        "(-1).round(-2147483648)",
        "1.floor(-2147483648)",
        "(-1).ceil(-2147483648)",
        "(-1.5).round(-1000000000)",
    ] {
        let result = run(source);
        assert_eq!(result.value.as_int(), Some(0), "{source}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{source}");
        assert!(
            result.stats.peak_memory_bytes < 4096,
            "{source}: {:?}",
            result.stats
        );
        assert!(result.stats.steps < 32, "{source}: {:?}", result.stats);
    }
}

#[test]
fn numeric_work_consumes_steps_and_reclaims_temporary_storage() {
    let input = Value::parse_integer(&"f".repeat(8192), 16).unwrap();
    for body in [
        "input.round(-100)",
        "input.divmod(7)",
        "input.remainder(7)",
        "input.clamp(input,input)",
    ] {
        let script = Engine::new()
            .compile(&format!("def run(input)\n{body}\nend"))
            .unwrap();
        let error = script
            .call(
                "run",
                std::slice::from_ref(&input),
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
        assert_eq!(error.kind, ErrorKind::Steps, "{body}");
    }
    let a = Value::parse_integer(&format!("1{}", "0".repeat(2500)), 16).unwrap();
    let b = Value::parse_integer(&format!("1{}", "0".repeat(2475)), 16).unwrap();
    let script = Engine::new()
        .compile("def run(a,b)\na.divmod(b)\nend")
        .unwrap();
    let result = script
        .call("run", &[a.clone(), b], CallOptions::default())
        .unwrap();
    assert_eq!(
        result.value.to_string(),
        "[1267650600228229401496703205376, 0]"
    );
    assert!(
        result.stats.retained_memory_bytes < 512,
        "{:?}",
        result.stats
    );
    assert_eq!(a.to_string().len(), 3011);
    assert_eq!(run("1e-308.round(320)").stats.retained_memory_bytes, 0);
}

#[test]
fn numeric_validation_fails_during_execution_before_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "1.round(1.5)",
        "1.ceil(2147483648)",
        "1.floor(-2147483649)",
        "1.clamp(2,0)",
        "1.clamp(0...2)",
        "1.clamp(0,0.0/0.0)",
        "1.clamp(0,2) {effect()}",
        "1.between?(0,2) {effect()}",
        "1.div(0)",
        "1.divmod(0)",
        "1.modulo(0)",
        "1.remainder(0)",
        "1e309.div(1)",
        "1e309.divmod(1)",
        "1e309.floor",
        "1e309.round",
    ] {
        assert!(
            engine
                .compile(&format!("{expression};effect()"))
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{expression}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn cancellation_prevents_numeric_results_and_later_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::int(7))
    });
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "(2**1000).divmod(cancel());effect()",
        "1.5.round(cancel());effect()",
        "(2**1000).clamp(cancel(),nil);effect()",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled, "{source}");
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}
