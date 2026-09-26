mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn regex_helpers_preserve_match_order_captures_and_replacement_syntax() {
    let script = Engine::new()
        .compile(
            r#"
[
  Regex.match("a|ab", "ab"),
  Regex.match("ab|a", "ab"),
  Regex.replace("abb", "((a)?b)*", "$0|$1|$2"),
  Regex.replace("b", "(?P<x>a)|(?P<x>b)", "<$x>"),
  Regex.replace("ab", "(?P<01>a)(?P<1000000000>b)", "$01/${01}/$1000000000"),
  Regex.replace("a", "a", "$$${0}/$1x/${1}x/${}/\\$0"),
  Regex.replace_all("abc", "a*", "X"),
  Regex.replace_all("éa", "", "-")
]
"#,
        )
        .unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            "a",
            "ab",
            "abb|b|a",
            "<b>",
            "a/a/b",
            "$a//x/${}/\\a",
            "XbXcX",
            "-é-a-"
        ])
    );
}

#[test]
fn unicode_flags_raw_bytes_and_offsets_use_re2_rules() {
    let script = Engine::new()
        .compile(
            r#"
[
  "Kſ".match?("(?i)ks"),
  "İı".match?("(?i)i"),
  "é".match?("\\w"),
  "é".match?("\\p{ upper-case_letter }"),
  "É".match?("\\p{ upper-case_letter }"),
  "\v".match?("\\s"),
  "\v".match?("[[:space:]]"),
  "\n".match?("(?s:.)"),
  "a\nb".match?("(?m:^b)", 2),
  "ab".match?("\\bb", 1),
  "ab".match?("\\Bb", 1),
  "éa".match?("a", 1),
  "éa".match?("a", 2),
  "éa".match?("$", 3),
  Regex.match(".", "\xff").as(string).bytes,
  Regex.replace_all("a\xffb", ".", "$0").bytes,
  "a".match?("\\x{d800}")
]
"#,
        )
        .unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            true,
            false,
            false,
            false,
            true,
            false,
            true,
            true,
            true,
            false,
            true,
            true,
            false,
            false,
            [255],
            [97, 255, 98],
            false
        ])
    );
}

#[test]
fn regex_anchors_survive_non_capturing_groups() {
    let script = Engine::new()
        .compile(
            r#"
[
  Regex.match("(?:^a$)", "ab"),
  Regex.replace("ab", "(?:^a$)", "X"),
  Regex.replace_all("aba", "(?:^a$)", "X"),
  "ab".match?("(?:^a$)")
]
"#,
        )
        .unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([null, "ab", "aba", false])
    );
}

#[test]
fn short_matches_release_the_large_input_and_compiled_scratch() {
    let mut input = vec![b'x'; 1 << 20];
    input[0] = b'A';
    let input = Value::bytes(input);
    let script = Engine::new()
        .compile("def run(text: string) -> string?\nRegex.match(\"A\",text)\nend")
        .unwrap();
    let output = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_bytes().unwrap(), b"A");
    assert!(output.stats.retained_memory_bytes < 1024);
    assert_ne!(
        output.value.as_bytes().unwrap().as_ptr(),
        input.as_bytes().unwrap().as_ptr()
    );
    let script = Engine::new()
        .compile("def run(text: string) -> string?\nRegex.match(\"^Z\",text)\nend")
        .unwrap();
    let output = script
        .call(
            "run",
            &[Value::bytes(vec![b'x'; 4096])],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(json(&output.value), serde_json::Value::Null);
    assert_eq!(output.stats.retained_memory_bytes, 0);
}

#[test]
fn argument_pattern_and_expansion_failures_prevent_host_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "Regex.match(\"(\",\"\");effect()",
        "Regex.match(\"(?=a)\",\"a\");effect()",
        "\"\".match?(\"[\",999);effect()",
        "\"a\".match?(\"a\",-1);effect()",
        "Regex.replace_all(\"x\"*65536,\"(.*)\",\"$1\"*17);effect()",
    ] {
        let script = engine.compile(source).unwrap();
        let options = CallOptions {
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert!(script.run(options).is_err(), "{source}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // A block, an unknown keyword or a pattern of the wrong type is
    // refused before anything runs.
    for (source, code) in [
        ("Regex.match(\"a\",\"a\"){effect()}", "V0305"),
        ("Regex.replace(\"a\",\"a\",\"b\",extra:1);effect()", "V0302"),
        ("Regex.match(:a,\"a\");effect()", "V0101"),
        ("\"\".match?(\"[\",999){effect()}", "V0305"),
        ("\"a\".match?(\"a\",-1){effect()}", "V0305"),
        ("\"a\".match?(\"a\"){effect()}", "V0305"),
    ] {
        let mut checked = common::static_engine();
        checked.register("effect", |_, _| panic!("effect ran"));
        let error = checked.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
}

#[test]
fn execution_limits_stop_state_work_and_compilation() {
    for source in [
        "Regex.match(\"(a?){1000}b\",\"a\"*1000)",
        "Regex.match(\"a\"*16384,\"\")",
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: Some(1000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
    }
    let script = Engine::new()
        .compile("Regex.match(\"(?:\"+\"a\"*101+\"){1000}\",\"\")")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Memory
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let script = Engine::new().compile("Regex.match(\"\",\"\")").unwrap();
    assert_eq!(
        script
            .run(CallOptions {
                cancellation,
                ..CallOptions::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    // No literal prefix, so the matcher does real work at every position
    // instead of scanning ahead, and the deadline lands mid-match.
    let script = Engine::new()
        .compile("def run(text: string) -> string?\nRegex.match(\"(?:x?){200}y\",text)\nend")
        .unwrap();
    let error = script
        .call(
            "run",
            &[Value::bytes(vec![b'b'; 1 << 20])],
            CallOptions {
                deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(5)),
                limits: Limits {
                    steps: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Deadline);
}

#[test]
fn namespace_aliases_keywords_and_rebinding_use_ordinary_calls() {
    let script = common::runtime_engine()
        .compile(
            r#"
alias=Regex
args=["a", "ba"]
first=alias.match(*args)
Regex={match:7}
[first,Regex["match"],alias.replace_all("aba","a","X")]
"#,
        )
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(json(&result.value), serde_json::json!(["a", 7, "XbX"]));
}

#[test]
fn maximal_literal_patterns_match_within_the_default_step_quota() {
    // The reference matches a 16 KiB case-insensitive literal against 16 KiB
    // of uppercase text under its default quota.
    let pattern = "a".repeat(16 << 10);
    let script = Engine::new()
        .compile(&format!(
            "def match_op(t: string) -> int?\n  t =~ /{pattern}/i\nend\n\
             def string_match_q(t: string) -> bool\n  t.match?(/{pattern}/i)\nend\n\
             def string_sub(t: string) -> string\n  t.sub(/{pattern}/i, \"X\")\nend\n\
             def regex_match_q(t: string) -> bool\n  /{pattern}/i.match?(t)\nend\n\
             def near_misses(t: string) -> int?\n  (\"a\" * 16383 + \"c\") * 2 =~ /{near}b/\nend",
            near = &pattern[1..]
        ))
        .unwrap();
    let text = Value::bytes("A".repeat(16 << 10));
    let options = || CallOptions {
        limits: Limits {
            memory_bytes: Some(64 << 20),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    for (function, expected) in [
        ("match_op", serde_json::json!(0)),
        ("string_match_q", serde_json::json!(true)),
        ("string_sub", serde_json::json!("X")),
        ("regex_match_q", serde_json::json!(true)),
        ("near_misses", serde_json::Value::Null),
    ] {
        let outcome = script
            .call(function, std::slice::from_ref(&text), options())
            .unwrap();
        assert_eq!(json(&outcome.value), expected, "{function}");
    }
}

#[test]
fn regex_serves_the_regexp_constructors_under_their_canonical_names() {
    let script = Engine::new()
        .compile(
            r#"
[
  Regex.new("a+").source,
  Regex.escape("a.b*"),
  Regex.union("a", "b.").source,
  Regex.union.source,
  Regex.new("x") == Regex.new("x"),
  Regex.union("a").match?("a")
]
"#,
        )
        .unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!(["a+", "a\\.b\\*", "a|b\\.", "[^\\s\\S]", true, true])
    );
    // A constructor read as a value, or called with an argument of the
    // wrong type, a block or a keyword, is refused before anything runs.
    for (source, code) in [
        ("Regex.new", "V0301"),
        ("Regex.escape(1)", "V0101"),
        ("Regex.union { }", "V0305"),
        ("Regex.union(a: 1)", "V0302"),
    ] {
        let error = common::static_engine().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
}
