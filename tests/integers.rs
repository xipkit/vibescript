use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, parse_json, stringify_json};

fn run(source: &str) -> vibescript::Outcome {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
}

#[test]
fn promotion_normalizes_results_without_expanding_the_value_representation() {
    assert_eq!(std::mem::size_of::<Value>(), 16);
    for (source, expected) in [
        ("(9223372036854775807 + 1) - 1", i64::MAX),
        ("-9223372036854775809 + 1", i64::MIN),
        ("n=2**200;n-n", 0),
        ("n=2**200;n//n", 1),
        ("n=2**200;n*0", 0),
        ("-9223372036854775808 % -1", 0),
        ("n=2**200;n**0", 1),
    ] {
        let result = run(source);
        assert_eq!(result.value.as_int(), Some(expected), "{source}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{source}");
    }
    let result = run("-(-9223372036854775808)");
    assert!(result.value.is_integer());
    assert_eq!(result.value.type_name(), "int");
    assert_eq!(result.value.as_int(), None);
    assert_eq!(result.value.to_string(), "9223372036854775808");
}

#[test]
fn source_and_host_integers_preserve_every_digit_through_json() {
    let text = "1234567890".repeat(150);
    let expected = Value::parse_integer(&text, 10).unwrap();
    let decoded = parse_json(text.as_bytes(), CallOptions::default()).unwrap();
    let encoded = stringify_json(&decoded.value, CallOptions::default()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(text.as_bytes()));
    let result = run(&text);
    assert_eq!(result.value.to_string(), expected.to_string());
    for (text, radix, expected) in [
        ("-8000000000000000", 16, "-9223372036854775808"),
        ("10000000000000000", 16, "18446744073709551616"),
        ("zzzzzzzzzzzzzz", 36, "6140942214464815497215"),
        ("+000", 10, "0"),
    ] {
        assert_eq!(
            Value::parse_integer(text, radix).unwrap().to_string(),
            expected
        );
    }
}

#[test]
fn finite_float_conversion_preserves_binary_values_and_special_values() {
    let script = Engine::new()
        .compile("def run(input: float) -> int\ninput.to_i\nend")
        .unwrap();
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            script
                .call("run", &[Value::float(value)], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Arithmetic
        );
    }
    for (source, expected) in [
        ("1e30.to_i", "1000000000000000019884624838656"),
        ("(-1e30).to_i", "-1000000000000000019884624838656"),
        ("(2**1024).to_f.to_s", "Infinity"),
        ("(-(2**1024)).to_f.to_s", "-Infinity"),
    ] {
        let value = run(source).value;
        assert_eq!(value.to_string(), expected);
    }
    let result = run("n=2**100;[n == n.to_f,n+1 == n.to_f,n-1 < n.to_f,n+1 > n.to_f]");
    assert_eq!(result.value.to_string(), "[true, false, true, true]");
}

#[test]
fn large_arithmetic_conversion_and_comparison_obey_step_limits() {
    let input = Value::parse_integer(&"f".repeat(8192), 16).unwrap();
    for body in [
        "input * input",
        "input // (input-1)",
        "input.to_s",
        "input == input",
    ] {
        let script = Engine::new()
            .compile(&format!("def run(input: int) -> any\n{body}\nend"))
            .unwrap();
        let result = script.call(
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
        );
        assert_eq!(result.unwrap_err().kind, ErrorKind::Steps, "{body}");
    }
}

#[test]
fn power_rejects_impossible_growth_before_building_the_result() {
    for source in ["2**1000000000", "(-2)**1000000000", "(2**1000)**1000000"] {
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
}

#[test]
fn conversion_exhaustion_stays_latched_when_a_host_ignores_it() {
    let mut engine = Engine::new();
    engine.register("convert", |ctx, _| {
        assert_eq!(
            ctx.parse_integer(&"0".repeat(10_000), 10).unwrap_err().kind,
            ErrorKind::Steps
        );
        Ok(Value::nil())
    });
    let error = engine
        .compile("convert()")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                steps: Some(100),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    let called = Arc::new(AtomicUsize::new(0));
    let observed = called.clone();
    let mut engine = Engine::new();
    engine.register("convert", move |ctx, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            ctx.parse_integer(&"f".repeat(16384), 16).unwrap_err().kind,
            ErrorKind::Memory
        );
        Ok(Value::nil())
    });
    let error = engine
        .compile("convert()")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(8192),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    assert_eq!(called.load(Ordering::SeqCst), 1);
}

#[test]
fn cancelled_numeric_work_prevents_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::int(1))
    });
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "x=2**1000; y=(cancel().as(int)+x)*x;effect()",
        "x=2**1000; y=x//(cancel().as(int)+1);effect()",
        "x=cancel(); y=JSON.parse(\"123456789012345678901234567890\");effect()",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn small_quotients_release_the_large_division_buffers() {
    let a = Value::parse_integer(&format!("1{}", "0".repeat(2500)), 16).unwrap();
    let b = Value::parse_integer(&format!("1{}", "0".repeat(2475)), 16).unwrap();
    let script = Engine::new()
        .compile("def run(a: int,b: int) -> int\na//b\nend")
        .unwrap();
    let result = script
        .call("run", &[a.clone(), b], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "1267650600228229401496703205376");
    assert!(
        result.stats.retained_memory_bytes < 256,
        "{:?}",
        result.stats
    );
    assert_eq!(a.to_string().len(), 3011);
}

#[test]
fn literal_limits_and_bounded_integer_domains_remain_enforced() {
    for source in ["9".repeat(100_001), format!("0x{}", "f".repeat(99_999))] {
        assert_eq!(
            Engine::new().compile(&source).err().unwrap().kind,
            ErrorKind::Syntax
        );
    }
    for source in ["[1][2**100]", "(1..2**100).to_a", "\"x\"*(2**100)"] {
        assert!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .is_err()
        );
    }
    assert_eq!(
        run("0d123456789012345678901234567890").value.to_string(),
        "123456789012345678901234567890"
    );
}

#[test]
fn long_json_floats_preserve_halfway_rounding_and_exponent_scaling() {
    let midpoint = "1.00000000000000011102230246251565404236316680908203125";
    for (text, expected) in [
        (format!("{midpoint}{}", "0".repeat(2000)), 1.0f64),
        (
            format!("{midpoint}{}1", "0".repeat(2000)),
            f64::from_bits(1.0f64.to_bits() + 1),
        ),
        (format!("0.{}1e10001", "0".repeat(10000)), 1.0),
        (format!("1.{}e-100000", "0".repeat(2000)), 0.0),
        (format!("0.{}e{}", "0".repeat(2000), "9".repeat(2000)), 0.0),
    ] {
        for negative in [false, true] {
            let text = if negative {
                format!("-{text}")
            } else {
                text.clone()
            };
            let value = parse_json(text.as_bytes(), CallOptions::default())
                .unwrap()
                .value;
            let expected = if negative { -expected } else { expected };
            assert_eq!(value.as_float().unwrap().to_bits(), expected.to_bits());
        }
    }
    let text = format!("0.{}1", "0".repeat(100_000));
    let error = parse_json(
        text.as_bytes(),
        CallOptions {
            limits: Limits {
                steps: Some(100),
                ..Limits::default()
            },
            ..CallOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}

#[test]
fn nonfinite_powers_fail_during_execution_before_following_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in ["0 ** -1", "0 ** -(2**80)", "10.0 ** 1000", "(-1.0) ** 0.5"] {
        let error = engine
            .compile(&format!("{expression}; effect()"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Arithmetic, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn reference_sized_integers_fit_the_default_quota() {
    // The reference evaluates each of these under its default step quota.
    let sixty = (0..60)
        .map(|i| format!("k{i}: 2 ** {}", 900_000 + i))
        .collect::<Vec<_>>()
        .join(", ");
    for (source, expected) in [
        (format!("x = {}\nx % 7", "9".repeat(20_000)), 1),
        (format!("x = {}\nx % 1000003", "9".repeat(50_000)), 754_969),
        ("x = 2 ** 400000\nx % 1000".to_owned(), 376),
        ("x = 3 ** 50000\n(x * x) % 1000003".to_owned(), 799_099),
        (format!("{{{sixty}}}.length"), 60),
    ] {
        let outcome = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(outcome.value.as_int(), Some(expected), "{}", &source[..40]);
    }
    // Rendering 100,000 bits still converts within the quota.
    let outcome = run("(2 ** 100000).to_s.length");
    assert_eq!(outcome.value.as_int(), Some(30_103));
}

/// FNV-1a over the digits, matching the reference hashes below.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

#[test]
fn long_decimal_conversions_match_python() {
    // The length and FNV-1a hash of Python's str() for each value: powers
    // of ten and their neighbors, all nines, single bits, products,
    // quotients and remainders of both signs, around conversion and
    // division thresholds.
    for (source, length, hash) in [
        ("3 ** 209590", 100000, 0x3b24_3676_2505_a83b),
        ("-(7 ** 60000)", 50707, 0x65e7_79c8_ba51_5706),
        ("10 ** 100000 - 1", 100000, 0x3e3a_fb82_516f_4705),
        ("10 ** 99999", 100000, 0x2212_d716_587d_fb24),
        ("10 ** 50000 + 1", 50001, 0x9358_6db8_a04f_20af),
        ("-(10 ** 77777 - 1)", 77778, 0xe98b_4e6e_0a54_3c83),
        ("2 ** 800000", 240824, 0xa446_c5db_71f0_4906),
        ("2 ** 332193 - 1", 100001, 0xc30a_7a16_3425_0d70),
        (
            "3 ** 100000 * 7 ** 30000 - 5 ** 20000",
            73066,
            0xa6d3_3d26_4edc_5692,
        ),
        (
            "(10 ** 40000 - 1) * (10 ** 40000 + 1)",
            80000,
            0x2b16_5e4d_6b58_e5a5,
        ),
        ("3 ** 150000 // 7 ** 40000", 37765, 0xf3c4_1d76_2af0_eb43),
        ("3 ** 150000 % 7 ** 40000", 33804, 0x6a1c_95d1_7615_a6ae),
        ("-(3 ** 150000) // 7 ** 40000", 37766, 0x3b2e_7436_340c_7345),
        ("-(3 ** 150000) % 7 ** 40000", 33803, 0xe6a7_6a7d_7290_77e4),
        ("2 ** 1056 - 1", 318, 0xf310_fa44_524c_6284),
        ("2 ** 1024 + 1", 309, 0xa5cb_b29d_f993_6abf),
        ("-(2 ** 47104) - 1", 14181, 0xf95b_608a_56d5_0403),
        ("2 ** 47104 - 1", 14180, 0xef74_599c_ba97_cf60),
        ("10 ** 14179 * 3 + 7", 14180, 0x2e91_89a2_8fd8_f09f),
        ("(10 ** 7 - 1) ** 30000", 210000, 0x40ae_2578_6515_f8d2),
        ("999999 ** 12345", 74070, 0x6fb3_4718_bf9c_5d84),
        (
            "12345678901234567890 ** 4000 + 10 ** 3000",
            76367,
            0xdb07_c307_2c21_b030,
        ),
        ("3 ** 503000 // 7 ** 142000", 119989, 0x5821_fb06_e4a2_944f),
        ("3 ** 503000 % 7 ** 142000", 120004, 0x9e91_1546_497c_510e),
        (
            "-(10 ** 200000) // (10 ** 70000 - 1)",
            130002,
            0x9dfd_4bbe_13bd_1c6b,
        ),
        (
            "(10 ** 200000) % -(10 ** 70000 + 1)",
            70001,
            0xecb8_842e_9bfa_103b,
        ),
        (
            "(2 ** 400000 - 1) // (2 ** 99999 + 1)",
            90310,
            0xeec9_4e74_88de_3f99,
        ),
        (
            "(2 ** 400000 - 1) % (2 ** 99999 + 1)",
            2,
            0x07f8_8f07_b4ba_038b,
        ),
    ] {
        let outcome = Engine::new()
            .compile(&format!("x = {source}\n[x.to_s, \"#{{x}}\" == x.to_s]"))
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        let [text, interpolated] = outcome.value.as_array().unwrap() else {
            panic!("{source}");
        };
        let text = text.as_bytes().unwrap();
        assert_eq!((text.len(), fnv(text)), (length, hash), "{source}");
        assert!(interpolated.truthy(), "{source}");
    }
}

#[test]
fn rendering_reference_sized_integers_fits_the_default_quota_with_room() {
    // The reference renders 2 ** 800000 into a template under its default
    // quota; converting it, or a dense value as long, takes about half.
    for (source, length, limit) in [
        (
            "\"{{v}}\".template({v: 2 ** 800000}).bytesize",
            240_824,
            300_000,
        ),
        ("x = 3 ** 503000\n1", 1, 400_000),
        ("(3 ** 503000).to_s.bytesize", 239_992, 1_000_000),
    ] {
        let outcome = run(source);
        assert_eq!(outcome.value.as_int(), Some(length), "{source}");
        assert!(outcome.stats.steps < limit, "{source}: {:?}", outcome.stats);
    }
    let cost = |exponent: u32| {
        let steps = |source: &str| {
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions {
                    limits: Limits {
                        steps: None,
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                })
                .unwrap()
                .stats
                .steps
        };
        steps(&format!("x = 3 ** {exponent}\nx.to_s.bytesize"))
            - steps(&format!("x = 3 ** {exponent}\n1"))
    };
    let (small, large) = (cost(125_000), cost(500_000));
    // Quadrupling the digits costs about six times the steps, where the
    // schoolbook conversion cost sixteen.
    assert!(large < small * 7, "{small} then {large}");
}

#[test]
fn long_digit_strings_parse_exactly() {
    // Parsing by halves meets the powers of ten built by multiplication and
    // renders back to the same digits.
    let digits = "31415926535897932384626433832795028841971693993751".repeat(1999);
    for (source, expected) in [
        (
            format!("\"1{}\".to_i == 10 ** 99999", "0".repeat(99999)),
            "true",
        ),
        (
            format!("\"{}\".to_i == 10 ** 99999 - 1", "9".repeat(99999)),
            "true",
        ),
        (format!("\"{digits}\".to_i.to_s == \"{digits}\""), "true"),
        (
            format!("\"-{digits}\".to_i == -(\"{digits}\".to_i)"),
            "true",
        ),
    ] {
        let outcome = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        assert_eq!(outcome.value.to_string(), expected, "{}", &source[..20]);
    }
}
