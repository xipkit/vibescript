use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn evaluate(source: &str) -> serde_json::Value {
    json(
        &Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value,
    )
}

#[test]
fn concat_preserves_value_bindings_and_argument_order() {
    assert_eq!(
        evaluate(
            r#"
s="a";alias=s;out=s.concat("b".tap{s="b"},"c")
a=["x"];h={text:"y"}
[s,alias,out,a[0].concat("!"),a,h.text.concat("?"),h,"q".concat(*["r","s"]),
 "é\xff".concat("\x00").bytes,"a".concat,"a".concat("","")]
"#
        ),
        serde_json::json!(["b","a","abc","x!",["x"],"y?",{"text":"y"},"qrs",[195,169,255,0],"a","a"])
    );
}

#[test]
fn symbol_conversion_preserves_raw_bytes_without_becoming_a_string() {
    assert_eq!(
        evaluate(
            r#"
s="a\xff\x00";symbol=s.to_sym
[symbol==s,symbol==s.intern,symbol.to_s.bytes,"".intern=="".to_sym,
 "two words".to_sym=="two words".intern,"+".intern==:+]
"#
        ),
        serde_json::json!([false, true, [97, 255, 0], true, true, true])
    );
}

#[test]
fn numeric_string_conversions_preserve_strict_and_prefix_parsing_rules() {
    assert_eq!(
        evaluate(
            r#"
[" -42 ".to_i,"9223372036854775808".to_i,"0x1.8p+1".to_f,
 "1_0.5".to_f,"1e-400".to_f,"ff tail".hex,"-0X1a".hex,
 "garbage".hex,"1__2".hex,"1_2".hex,"0b101".oct,"0d99".oct,
 "17".oct,"08".oct,"0xff".oct,"-8000000000000000".hex,
 "7fffffffffffffff".hex,"-1000000000000000000000".oct]
"#
        ),
        serde_json::json!([
            -42,
            9223372036854775808u64,
            3,
            10.5,
            0,
            255,
            -26,
            0,
            1,
            18,
            5,
            99,
            15,
            0,
            255,
            i64::MIN,
            i64::MAX,
            i64::MIN
        ])
    );
    let script = Engine::new().compile("\"-0.0\".to_f").unwrap();
    assert_eq!(
        script
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_float()
            .unwrap()
            .to_bits(),
        (-0.0f64).to_bits()
    );
    for source in [
        "\"12tail\".to_i",
        "\"1_000\".to_i",
        "\"NaN\".to_f",
        "\"Infinity\".to_f",
        "\"1e400\".to_f",
        "\"8000000000000000\".hex",
        "\"-8000000000000001\".hex",
        "\"fffffffffffffffff\".hex",
        "\"1000000000000000000000\".oct",
    ] {
        assert!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn string_bounds_follow_byte_order_and_short_circuit_between() {
    assert_eq!(
        evaluate(
            r#"
["a".clamp("m","z"),"zz".clamp("m","z"),"n".clamp("m","z"),
 "x".clamp(nil,nil),"a".clamp("b",nil),"z".clamp(nil,"y"),
 "m".between?("a","z"),"a".between?("z",nil),
 "\xff".clamp("\xfe","\xff").bytes,"é".between?("z","\xff")]
"#
        ),
        serde_json::json!(["m", "z", "n", "x", "b", "y", true, false, [255], true])
    );
    for source in [
        "\"a\".clamp(\"z\",\"a\")",
        "\"a\".clamp(\"b\",:z)",
        "\"a\".between?(nil,\"z\")",
        "\"m\".between?(\"a\",nil)",
    ] {
        assert!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn split_distinguishes_whitespace_literal_and_character_limits() {
    assert_eq!(
        evaluate(
            r#"
[
 "  a b c  ".split,"  a b c  ".split(nil,2),"  a b c  ".split(" ",-1),
 "  ".split(nil,-1),"  ".split(nil,1),"".split(",",1),
 "a,,b,,".split(","),"a,,b,,".split(",",-1),"a,,b,,".split(",",2),
 "é🙂".split("",-1),"é🙂".split("",2),"abc".split("",9223372036854775807),
 "a,b,".split(",",-9223372036854775808),"a\u00a0b".split,
 "a\xffb\xff".split("\xff",-1).map{|v|v.bytes},
 "a\xffé".split("").map{|v|v.bytes}
]
"#
        ),
        serde_json::json!([
            ["a", "b", "c"],
            ["a", "b c  "],
            ["a", "b", "c", ""],
            [""],
            ["  "],
            [],
            ["a", "", "b"],
            ["a", "", "b", "", ""],
            ["a", ",b,,"],
            ["é", "🙂", ""],
            ["é", "🙂"],
            ["a", "b", "c", ""],
            ["a", "b", ""],
            ["a\u{a0}b"],
            [[97], [98], []],
            [[97], [255], [195, 169]]
        ])
    );
}

#[test]
fn substring_offsets_count_characters_and_invalid_bytes_match_replacement_runes() {
    assert_eq!(
        evaluate(
            r#"
[
 "héllo hello".index("llo"),"héllo hello".index("llo",6),
 "héllo hello".rindex("llo",4),"hello".index("l",-3),"hello".rindex("l",-2),
 "hello".index("l",-9),"hello".rindex(nil,-9),"a".rindex("",99),"a".index("",99),
 "ababa".rindex("aba",1),"ababa".rindex("aba",2),"éa".index("a",1.9),
 "a\xffb\xfe".index("�"),"a\xffb\xfe".rindex("\xff"),"a\xffb\xfe".index("\xfe",2),
 "é".index("\xa9"),"\xff".index("�"),"�".index("\xff")
]
"#
        ),
        serde_json::json!([
            2, 8, 2, 2, 3, null, null, 1, null, 0, 2, 1, 1, 3, 3, null, 0, 0
        ])
    );
}

#[test]
fn signatures_validate_arguments_before_later_host_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    for source in [
        "\"x\".concat(:x)",
        "\"x\".concat(\"y\",nil)",
        "\"x\".split(:x)",
        "\"x\".split(nil,1.0)",
        "\"x\".split(nil,9223372036854775808)",
        "\"x\".split(nil,0,1)",
        "\"x\".to_sym{effect()}",
        "\"x\".intern(extra:1)",
        "\"1\".to_i{effect()}",
        "\"1\".to_f(extra:1)",
        "\"x\".to_s{effect()}",
        "\"x\".string{effect()}",
        "\"x\".clamp(nil,nil){effect()}",
        "\"x\".between?(\"a\",\"z\",extra:1)",
        "\"aba\".find_index(\"a\")",
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
    for source in [
        "\"x\".concat(\"y\",extra:1){effect()}",
        "\"ff\".hex(extra:1){effect()}",
        "\"17\".oct(extra:1){effect()}",
        "\"a b\".split(nil,2,extra:1){effect()}",
    ] {
        engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        engine
            .compile("\"x\".concat(effect());effect()")
            .unwrap()
            .run(CallOptions::default())
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn split_results_detach_and_repeated_discarded_results_release_memory() {
    let mut input = vec![b'a'; 1 << 20];
    input[1] = b',';
    let input = Value::bytes(input);
    let options = CallOptions {
        limits: Limits {
            steps: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let script = Engine::new()
        .compile("def run(text)\ntext.split(\",\")[0]\nend")
        .unwrap();
    let output = script
        .call("run", std::slice::from_ref(&input), options.clone())
        .unwrap();
    assert_eq!(output.value.as_bytes().unwrap(), b"a");
    assert_ne!(
        output.value.as_bytes().unwrap().as_ptr(),
        input.as_bytes().unwrap().as_ptr()
    );
    assert!(output.stats.retained_memory_bytes < 1024);
    let script = Engine::new()
        .compile("def run(text)\n30.times{text.split(\",\");text.concat(\"!\")};nil\nend")
        .unwrap();
    let output = script.call("run", &[input], options).unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert!(output.stats.peak_memory_bytes < 4 << 20);
}

#[test]
fn cancellation_deadlines_and_work_limits_stop_before_followup_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let input = Value::bytes(vec![b'0'; 65536]);
    for body in [
        "text.concat(text)",
        "text.hex",
        "text.oct",
        "text.split(\"x\")",
        "text.split(\"\")",
        "text.clamp(text,nil)",
    ] {
        let script = engine
            .compile(&format!("def run(text)\n{body};effect()\nend"))
            .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        for (options, kind) in [
            (
                CallOptions {
                    cancellation,
                    ..CallOptions::default()
                },
                ErrorKind::Cancelled,
            ),
            (
                CallOptions {
                    deadline: Some(Instant::now()),
                    ..CallOptions::default()
                },
                ErrorKind::Deadline,
            ),
            (
                CallOptions {
                    limits: Limits {
                        steps: Some(20),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
                ErrorKind::Steps,
            ),
        ] {
            assert_eq!(
                script
                    .call("run", std::slice::from_ref(&input), options)
                    .unwrap_err()
                    .kind,
                kind,
                "{body}"
            );
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
