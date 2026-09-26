mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, HostMethod, Limits, Signature, Value};

/// The code and offset of each static diagnostic that refuses `source`.
fn refused(source: &str) -> Vec<(String, usize)> {
    let error = vibescript::Engine::new()
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| (d.code.to_string(), d.span.start))
        .collect()
}

fn at(source: &str, expected: &[(&str, &str)]) -> Vec<(String, usize)> {
    expected
        .iter()
        .map(|(code, text)| {
            let offset = source
                .find(text)
                .unwrap_or_else(|| panic!("{text} in {source}"));
            ((*code).to_owned(), offset)
        })
        .collect()
}

#[test]
fn json_parse_requires_strings_through_every_call_form() {
    // Each is refused before running; indexing a namespace and `send` are
    // removed.
    for (source, expected) in [
        ("JSON.parse(:\"7\")", vec![("V0101", ":\"7")]),
        (
            "JSON::parse(:\"7\")",
            vec![("V0416", "::"), ("V0101", ":\"7")],
        ),
        ("(JSON.parse)(:\"7\")", vec![("V0101", ":\"7")]),
        (
            "JSON[:parse](:\"7\")",
            vec![("V0112", "JSON"), ("V0409", ":parse")],
        ),
        ("JSON.send(:parse,:\"7\")", vec![("V0405", "send")]),
        ("JSON.parse_as(:\"7\",int)", vec![("V0101", ":\"7")]),
    ] {
        assert_eq!(refused(source), at(source, &expected), "{source}");
    }
}

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
        .compile("def convert(input: string) -> float\nto_float(input)\nend")
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
fn namespaces_cannot_be_rebound_and_have_no_fields() {
    // A namespace cannot be rebound, at the top level or in a function.
    for (source, expected) in [
        (
            "def read -> float\nMath.PI\nend\ndef change -> float\nMath={PI:7.0};read\nend",
            vec![("V0102", "Math=")],
        ),
        ("Math=7;[1].each {Math=8};Math", vec![("V0102", "Math=7")]),
    ] {
        assert_eq!(refused(source), at(source, &expected), "{source}");
    }
    // A namespace is not a hash, so it has no fields to write, list, clear
    // or replace, and a local assigned on one path cannot be read.
    for (source, expected) in [
        ("m=Math;m.PI=7;[m.PI,Math.PI]", vec![("V0203", "PI=")]),
        ("JSON.keys", vec![("V0203", "keys")]),
        (
            "[1].each {if false;Math=7;end;Math.PI}",
            vec![("V0102", "Math=7"), ("V0202", "Math.PI"), ("V0203", "PI}")],
        ),
        ("Math.clear;Math=={}", vec![("V0203", "clear")]),
        ("Math.replace({});Math=={}", vec![("V0203", "replace")]),
    ] {
        assert_eq!(refused(source), at(source, &expected), "{source}");
    }
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
        .compile("def run(Math: hash<string, int>) -> array<array<int?>>\n[1].map {[2].map {Math[\"PI\"]}}\nend")
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
    // A builtin is not a value to bind or store.
    for (source, expected) in [
        (
            "f=Math::sqrt;f(9)",
            vec![("V0416", "::"), ("V0301", "sqrt"), ("V0310", "f(9)")],
        ),
        (
            "Math[\"map\"]=Math[\"sqrt\"];Math.map(9)",
            vec![
                ("V0112", "Math[\"map\"]"),
                ("V0112", "Math[\"sqrt\"]"),
                ("V0203", "map(9)"),
            ],
        ),
        (
            "to_int=Math::sqrt;to_int(9)",
            vec![("V0416", "::"), ("V0301", "sqrt"), ("V0310", "to_int(9)")],
        ),
    ] {
        assert_eq!(refused(source), at(source, &expected), "{source}");
    }
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
    // Typed, so the script reads the count without a cast, which would
    // hold memory of its own.
    let tracked = HostMethod::new("tracked", |ctx, _, _| {
        Ok(Value::int(ctx.stats().retained_memory_bytes as i64))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: false,
    })
    .unwrap();
    engine.register_method("tracked", tracked);
    let retained_result = engine
        .compile("retain(Math);nil")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(retained_result.stats.retained_memory_bytes > 512);
    let original = retained.lock().unwrap().take().unwrap();
    assert_eq!(original.type_name(), "object");
    let script = Engine::new()
        .compile("def run(input: hash<string, any>) -> hash<string, any>\ninput.clear;input\nend")
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
    // The namespace comes back as an argument, so the script can drop its
    // own reference: a namespace cannot be rebound.
    let result = engine
        .compile(
            "def run(ns: any) -> int\nretain(ns);ns=nil;a=tracked();release();b=tracked();a-b\nend",
        )
        .unwrap()
        .call("run", &[original], CallOptions::default())
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
        .compile("def run(input: hash<string, int>) -> hash<string, int>\ninput\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let result = engine
        .compile("def run(input: hash<string, int>)\na: hash<string, int> = {};a.replace(input);nil\nend")
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
        .compile("def run(input: string) -> float\nto_float(input)\nend")
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
        "Math.sqrt(-1)",
        "Math.log(2,-1)",
        "Math.asin(2)",
        "Math.PI()",
    ] {
        let script = engine.compile(&format!("{expression};effect()")).unwrap();
        assert!(script.run(CallOptions::default()).is_err(), "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
    // Calls whose shape is wrong are refused before anything runs.
    let mut engine = vibescript::Engine::new();
    engine.register("effect", |_, _| panic!("effect ran"));
    for (expression, code, text) in [
        ("to_float(:one)", "V0101", ":one"),
        ("Math.sqrt(9,x:1)", "V0302", "x:"),
        ("Math.sqrt(9,2)", "V0301", "sqrt"),
        ("Math.sqrt", "V0301", "sqrt"),
        ("{a:1}::a", "V0203", "::a"),
        ("Math.sqrt(9) {effect()}", "V0305", "{effect"),
        ("JSON.parse(\"1\") {effect()}", "V0305", "{effect"),
        ("JSON.stringify(1) {effect()}", "V0305", "{effect"),
        ("JSON.parse()", "V0301", "parse"),
        ("JSON.stringify(1,2)", "V0301", "stringify"),
        ("Math=7;Math(9)", "V0310", "Math(9)"),
        ("JSON.parse_as(\"1\",\"int\")", "V0101", "\"int\""),
    ] {
        let source = format!("{expression};effect()");
        let error = engine.compile(&source).err().unwrap();
        let found: Vec<(String, usize)> = error
            .diagnostics()
            .iter()
            .map(|d| (d.code.to_string(), d.span.start))
            .collect();
        let mut expected = at(&source, &[(code, text)]);
        if expression.starts_with("Math=7") {
            expected.insert(0, ("V0102".to_owned(), 0));
        }
        if text == "::a" {
            expected[0].1 += 2;
        }
        assert_eq!(found, expected, "{expression}");
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
        "to_float(cancel().as(string))",
        "to_int(cancel().as(string))",
        "JSON.parse(cancel().as(string))",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled, "{source}");
    }
    for source in [
        "Math.sqrt(ignore().as(int))",
        "to_float(ignore().as(int))",
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

#[test]
fn builtin_catalog_lists_the_names_scripts_reach() {
    let catalog = vibescript::builtins();
    let names: Vec<_> = catalog.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "Duration",
            "JSON",
            "Math",
            "Regex",
            "Time",
            "assert",
            "format",
            "loop",
            "money",
            "money_cents",
            "p",
            "print",
            "puts",
            "rand",
            "random_id",
            "require",
            "srand",
            "to_float",
            "to_int",
            "uuid",
            "warn",
        ]
    );
    assert_eq!(catalog["puts"].type_name(), "builtin");
    assert_eq!(catalog["puts"].to_string(), "<builtin puts>");
    let members = |name: &str| -> Vec<(String, &'static str)> {
        catalog[name]
            .as_hash()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                let key = String::from_utf8(key.as_bytes().unwrap().to_vec()).unwrap();
                (key, value.type_name())
            })
            .collect()
    };
    assert_eq!(catalog["JSON"].type_name(), "object");
    assert_eq!(
        members("JSON"),
        [
            ("parse".to_owned(), "builtin"),
            ("parse_as".to_owned(), "builtin"),
            ("stringify".to_owned(), "builtin"),
        ]
    );
    let math = members("Math");
    assert!(math.contains(&("PI".to_owned(), "float")), "{math:?}");
    assert!(math.contains(&("sqrt".to_owned(), "builtin")), "{math:?}");
    // Every listed name and member resolves in a script: reading one either
    // yields a value or refuses a callable, never an undefined name.
    for (name, value) in &catalog {
        let mut paths = vec![name.clone()];
        if let Some(entries) = value.as_hash() {
            for (key, _) in entries {
                let member = std::str::from_utf8(key.as_bytes().unwrap()).unwrap();
                paths.push(format!("{name}::{member}"));
            }
        }
        for path in paths {
            match Engine::new().compile(&path) {
                Ok(script) => {
                    if let Err(error) = script.run(CallOptions::default()) {
                        assert_ne!(error.kind, ErrorKind::Name, "{path}: {error}");
                    }
                }
                // Static types refuse a callable read as a value, or a
                // removed name with its replacement, but know every name.
                Err(error) => {
                    let codes: Vec<String> = error
                        .diagnostics()
                        .iter()
                        .map(|d| d.code.to_string())
                        .collect();
                    assert!(!codes.contains(&"V0201".to_owned()), "{path}: {error}");
                }
            }
        }
    }
}
