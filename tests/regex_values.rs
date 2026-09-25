use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let output = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn regex_values_preserve_flags_operators_and_literal_boundaries() {
    assert_eq!(size_of::<Value>(), 16);
    let host = Value::regex(b"a.b", "mi").unwrap();
    assert_eq!(host.as_regex(), Some((&b"a.b"[..], "im")));
    for flags in ["ii", "mm", "x", "s"] {
        assert_eq!(
            Value::regex(b"a", flags).unwrap_err().kind,
            ErrorKind::Argument
        );
    }
    let script = Engine::new()
        .compile(
            r##"
def take(*args)
 args.map {|v| "#{v}"}
end
def run(r)
 [r.source,r.flags,r.match?("A\nB"),"éA\nB" =~ r,r !~ "ab",
  r === "A\nB",r === 7,/a/i == Regexp.new("a"),/a/ == Regexp.new("a"),
  Regexp.union("a.b","c?").match?("xxc?yy"),Regexp.escape("a+b/c"),
  take(/#/,7),take(/[/]/),"#{/a\/b/im}",12/3/2]
end
"##,
        )
        .unwrap();
    let output = script.call("run", &[host], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            "a.b",
            "im",
            true,
            1,
            true,
            true,
            false,
            false,
            true,
            true,
            "a\\+b/c",
            ["/#/", "7"],
            ["/[\\/]/"],
            "/a\\/b/im",
            2
        ])
    );
    let script = Engine::new()
        .compile("def take(x,y);[x.source,y];end\ntake /#/,7")
        .unwrap();
    assert_eq!(
        json(&script.run(CallOptions::default()).unwrap().value),
        serde_json::json!(["#", 7])
    );
    assert_eq!(
        Engine::new().compile("Regexp::begin").err().unwrap().kind,
        ErrorKind::Syntax
    );
    assert_eq!(
        stringify_json(&Value::regex(b"a", "").unwrap(), CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Json
    );
}

#[test]
fn match_data_has_named_captures_character_offsets_and_detached_windows() {
    let script = Engine::new()
        .compile(
            r##"
m="éab!".match(/(?<x>a)(b)?(?<missing>z)?/)
n="a".match(/(?<x>a)|(?<x>b)/)
f=m[:begin]
g=m[:end]
[m[0],m[-1],m[-2],m[9],m[:x],m[:missing],m.captures,m.named_captures,
 m.pre_match,m.post_match,m.to_s,"#{m}",f(0),g(0),m.begin(1),m.end(1),
 m.begin(3),m.end(-1),n[:x],"éab".match("a",-2)[0],"éab".match("$",99)[0],
 "éab".match("a",-9),"a\xffb".match(/(.)b/)[1].bytes]
"##,
        )
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            "ab",null,"b",null,"a",null,["a","b",null],{"x":"a","missing":null},
            "é","!","ab","ab",1,3,1,2,null,null,"a","a","",null,[255]
        ])
    );
}

#[test]
fn scan_blocks_preserve_match_shapes_context_and_control_flow() {
    let script = Engine::new()
        .compile(
            r##"
def early()
 "abab".scan(/ab/) {return 41}
 9
end
out=[]
original="ab ab".scan(/(a)(b)/) {|a,b| out.push([a,b])}
ignored=[]
data=/a/.match("ab") {ignored.push(1)}
["abc".scan(/a*/),"éa".scan(//),"ab cd".scan(/\b[a-z]{2}/),
 "ab".scan(/(a)|(b)/),"aaa".scan(/(a){0}/),out,original,
 "aba".scan(/a/) {break 17},"aba".scan(/a/) {next 7},early(),
 "ab".match(/a/) {|m| m.to_s+"!"},"ab".match(/z/) {17},ignored,data[0]]
"##,
        )
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            ["a", "", ""],
            ["", "", ""],
            ["ab", "cd"],
            [["a", null], [null, "b"]],
            [[null], [null], [null], [null]],
            [["a", "b"], ["a", "b"]],
            "ab ab",
            17,
            "aba",
            41,
            "a!",
            null,
            [],
            "a"
        ])
    );
}

#[test]
fn escaped_accessors_retain_only_offsets_and_survive_host_imports() {
    let mut subject = vec![b'x'; 1 << 20];
    subject[17] = b'A';
    let script = Engine::new()
        .compile("def run(text)\nm=text.match(/A/);m[:begin]\nend")
        .unwrap();
    let output = script
        .call(
            "run",
            &[Value::bytes(subject)],
            CallOptions {
                limits: Limits {
                    steps: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(output.value.type_name(), "builtin");
    assert!(
        output.stats.retained_memory_bytes < 1024,
        "{:?}",
        output.stats
    );
    let consumer = Engine::new().compile("def run(f)\nf(0)\nend").unwrap();
    let imported = consumer
        .call("run", &[output.value], CallOptions::default())
        .unwrap();
    assert_eq!(json(&imported.value), serde_json::json!(17));
    assert_eq!(imported.stats.retained_memory_bytes, 0);
    let script = Engine::new()
        .compile("def run(text)\ntext.match(/A/)[0]\nend")
        .unwrap();
    let output = script
        .call(
            "run",
            &[Value::bytes(b"xA".repeat(32768))],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(output.value.as_bytes(), Some(&b"A"[..]));
    assert!(output.stats.retained_memory_bytes < 1024);
}

#[test]
fn protected_fields_and_bad_accessors_stop_later_host_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for operation in [
        "m[:to_s]=7",
        "m.captures.push(\"x\")",
        "m.captures[0]=\"x\"",
        "m.named_captures[:x]=\"q\"",
        "m.dup.clear",
        "m.dup.captures.push(\"x\")",
        "m.dup.captures[0]=\"x\"",
        "m.dup.named_captures[:x]=\"x\"",
        "m.dup.captures.delete_if{effect()}",
        "m.itself.captures.clear",
        "\"a\".match(/(a)/).captures.push(\"x\")",
        "[m].dup[0].clear",
        "m.to_s=7",
        "m.clear",
        "m.replace({})",
        "m.delete(:absent)",
        "m.store(:x,1)",
        "m.keep_if {effect()}",
        "m.delete_if {effect()}",
        "m.begin",
        "m.begin(-4)",
        "m.end(2)",
        "m.begin(0,extra:1)",
        "m.begin(0){effect()}",
        "f=m[:begin];f",
        "f=m[:begin];f(nil)",
        "Time=m[:begin];Time",
    ] {
        let source = format!("m=\"a\".match(/(a)/);{operation};effect()");
        assert!(
            engine
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{operation}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn streaming_matches_observe_limits_and_cancellation() {
    for source in [
        "\"a\".match(/(a?){1000}/)",
        "\"a\".scan(/(a?){1000}/) {nil}",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let error = script
            .run(CallOptions {
                limits: Limits {
                    steps: Some(500),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let token = CancellationToken::new();
    let cancel = token.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        cancel.cancel();
        Ok(Value::nil())
    });
    let error = engine
        .compile("\"aaa\".scan(/a/) {stop()}")
        .unwrap()
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn selected_regex_policies_preserve_anchors_and_match_data_identity() {
    let copy = Engine::new()
        .compile("m=\"a\".match(/(a)/);x=m.captures.dup.push(\"x\");[x,m.captures,m.dup.to_s]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&copy.value),
        serde_json::json!([["a", "x"], ["a"], "a"])
    );
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("../docs/compatibility-cases.json")).unwrap();
    for case in cases.as_array().unwrap() {
        if !matches!(
            case["policy"].as_str(),
            Some("honor_regex_anchors" | "protected_match_data")
        ) {
            continue;
        }
        let script = Engine::new()
            .compile(case["source"].as_str().unwrap())
            .unwrap();
        let result = script.call("run", &[Value::nil()], CallOptions::default());
        if case.get("expected_error").is_some() {
            assert_eq!(
                result.unwrap_err().kind,
                ErrorKind::Argument,
                "{}",
                case["name"]
            );
        } else {
            assert_eq!(
                json(&result.unwrap().value),
                case["expected"],
                "{}",
                case["name"]
            );
        }
    }
    let source = r##"m="ab".match(/(?<x>a)(b)/);alias=m;captures=m.captures;captures.push("x");[captures,m.captures,alias.captures,m.dup.to_s,"#{m.dup}"]"##;
    let value = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(
        json(&value),
        serde_json::json!([["a", "b", "x"], ["a", "b"], ["a", "b"], "ab", "ab"])
    );
}

#[test]
fn scans_enforce_fixed_output_caps_and_release_discarded_block_results() {
    let script = Engine::new()
        .compile("def run(text)\ntext.scan(/^a+$/).length\nend")
        .unwrap();
    let options = || CallOptions {
        limits: Limits {
            steps: None,
            memory_bytes: Some(3 << 20),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let result = script
        .call(
            "run",
            &[Value::bytes(vec![b'a'; (1 << 20) - 112])],
            options(),
        )
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!(1));
    let error = script
        .call(
            "run",
            &[Value::bytes(vec![b'a'; (1 << 20) - 111])],
            options(),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::OutputLimit);
    let script = Engine::new()
        .compile("def run(text)\ntext.scan(/a/) {\"x\"*65536}\nend")
        .unwrap();
    let result = script
        .call(
            "run",
            &[Value::bytes(vec![b'a'; 128])],
            CallOptions {
                limits: Limits {
                    steps: None,
                    memory_bytes: Some(160 << 10),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.as_bytes().unwrap(), &[b'a'; 128]);
    assert!(result.stats.retained_memory_bytes < 1024);
}
