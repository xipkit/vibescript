use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn interpolations_preserve_bytes_and_evaluate_parts_in_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("part", move |_, args| {
        let next = seen.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(args[0].as_int(), Some(next as i64));
        Ok(args[0].clone())
    });
    let result = engine
        .compile(r##""\x80#{part(1)}\u0041#{part(2)}#{part(3)}\xFF""##)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(&b"\x801A23\xff"[..]));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn percent_symbols_keep_their_kind_and_do_not_change_host_inputs() {
    let input = Value::bytes(vec![0xff, b'a']);
    let result = Engine::new()
        .compile("def run(input)\n%I[pre#{1}post #{nil} #{input}]\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(values.len(), 3);
    for (value, expected) in values.iter().zip([&b"pre1post"[..], b"", b"\xffa"]) {
        assert_eq!(value.type_name(), "symbol");
        assert_eq!(value.as_bytes(), Some(expected));
    }
    assert_eq!(input.as_bytes(), Some(&b"\xffa"[..]));
}

#[test]
fn retained_prefixes_and_completed_words_count_against_later_allocations() {
    for source in [
        format!("\"{}\"", "#{allocate()}".repeat(100)),
        format!("%I[{}]", "prefix#{allocate()}suffix ".repeat(100)),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = Engine::new();
        engine.register("allocate", move |ctx, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            ctx.bytes(&[b'x'; 8192])
        });
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(96_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        let calls = calls.load(Ordering::SeqCst);
        assert!(calls > 1 && calls < 20, "{calls} allocations");
    }
}

#[test]
fn nonlocal_exits_release_partial_strings_and_percent_words() {
    let mut engine = Engine::new();
    engine.register("allocate", |ctx, _| ctx.bytes(&[b'x'; 8192]));
    for body in [
        r##""#{allocate()}#{[1].map {return 7}}#{allocate()}""##,
        r##"%I[#{allocate()} next#{allocate()}#{[1].each {return 7}}]"##,
        r##"a=[];a.push("#{allocate()}#{[1].map {return 7}}");a"##,
        r##"while true; "#{allocate()}#{[1].map {return 7}}";end"##,
    ] {
        let source = format!("def work()\n{body}\nend\ndef run()\n200.times {{work()}};7\nend");
        let result = engine
            .compile(&source)
            .unwrap()
            .call(
                "run",
                &[],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(96_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
}

#[test]
fn cancellation_during_interpolation_prevents_later_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::int(7))
    });
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(9))
    });
    for source in [
        r##""before #{cancel()} after #{effect()}""##,
        r##"%W[first second#{cancel()} #{effect()}]"##,
        r##"%I[first second#{cancel()} #{effect()}]"##,
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

#[test]
fn interpolation_rendering_bounds_shared_graph_expansion() {
    let mut input = Value::int(7);
    for _ in 0..30 {
        input = Value::array(vec![input.clone(), input]);
    }
    let error = Engine::new()
        .compile("def run(input)\n\"prefix #{input} suffix\"\nend")
        .unwrap()
        .call(
            "run",
            &[input],
            CallOptions {
                limits: Limits {
                    steps: Some(5000),
                    memory_bytes: Some(1 << 20),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}

#[test]
fn interpolation_nesting_reaches_eight_and_rejects_excessive_source() {
    for (prefix, suffix) in [("\"#{", "}\""), ("%W[#{", "}]")] {
        let source = format!("{}7{}", prefix.repeat(8), suffix.repeat(8));
        Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    recursion: 1,
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        for depth in [9, 20_000] {
            let source = format!("{}7{}", prefix.repeat(depth), suffix.repeat(depth));
            assert_eq!(
                Engine::new().compile(&source).err().unwrap().kind,
                ErrorKind::Syntax
            );
        }
    }
    let nested = format!(
        "{}\"#{{7}}\"{}",
        "case 1;when 1;".repeat(300),
        ";end".repeat(300)
    );
    assert_eq!(
        Engine::new().compile(&nested).err().unwrap().kind,
        ErrorKind::Syntax
    );
}

#[test]
fn repeated_modulo_disambiguation_preserves_following_tokens() {
    let body = "n += total %w[0];".repeat(10_000);
    let source = format!("w=[3];total=10;n=0;{body}n");
    let result = Engine::new()
        .compile(&source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(10_000));
    for key in ["]", "x]y", "a]23_", "z]\\q"] {
        let quoted = serde_json::to_string(key).unwrap();
        let source = format!("w={{{quoted}:3}};n=10;n %w[{quoted}];17");
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(17), "{source}");
    }
}

#[test]
fn malformed_literal_combinations_return_errors_without_panicking() {
    let pieces = [
        "\"", "'", "#{", "}", "%w[", "%W{", "%I|", "\\", "\\u00", "[", "]", "(", ")", "1", "a",
        ";", "\n", "é", "💠",
    ];
    let mut state = 0xa874_d159_782b_f519u64;
    for _ in 0..4096 {
        let mut source = String::new();
        for _ in 0..24 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            source.push_str(pieces[state as usize % pieces.len()]);
        }
        assert!(
            std::panic::catch_unwind(|| Engine::new().compile(&source)).is_ok(),
            "{source:?}"
        );
    }
}

#[test]
fn interpolation_publishes_loop_bindings_without_publishing_block_locals() {
    for (body, expected) in [
        (
            "w=[3];r=%W[#{for x in [10];x;end} #{x %w[0]}];[r,x]",
            serde_json::json!([["[10]", "1"], 10]),
        ),
        (
            "r=\"#{for x in [];x;end}\";[r,x]",
            serde_json::json!(["[]", null]),
        ),
        (
            "r=\"#{[10].map {|x|x}}\";w=[3];[r,x %w[0]]",
            serde_json::json!(["[10]", 1]),
        ),
        (
            "r=\"#{for it in [10];it;end}\";[r,[1,2].map {it}]",
            serde_json::json!(["[10]", [10, 10]]),
        ),
    ] {
        let source = format!("def x(*args)\nargs.length\nend\ndef run()\n{body}\nend");
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap();
        let encoded = vibescript::stringify_json(&result.value, CallOptions::default()).unwrap();
        let actual: serde_json::Value =
            serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
        assert_eq!(actual, expected, "{body}");
    }
}

#[test]
fn float_interpolation_uses_reference_special_values_and_exponents() {
    let script = Engine::new()
        .compile("def run(input)\n\"#{input}\"\nend")
        .unwrap();
    for (value, expected) in [
        (f64::NAN, "NaN"),
        (f64::INFINITY, "Infinity"),
        (f64::NEG_INFINITY, "-Infinity"),
        (-0.0, "-0"),
        (1e6, "1e+06"),
        (1e-5, "1e-05"),
        (1e-4, "0.0001"),
        (f64::from_bits(1), "5e-324"),
    ] {
        let result = script
            .call("run", &[Value::float(value)], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
    }
}
