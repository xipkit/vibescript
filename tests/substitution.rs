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
    let output = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
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

fn unmetered_steps() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

#[test]
fn literal_and_regex_templates_keep_their_distinct_reference_rules() {
    assert_eq!(
        evaluate(
            r#"
[
 "bananas".sub("na","NA"),
 "bananas".gsub("na","NA"),
 "aba".gsub("a","$0\\1"),
 "aba".gsub(/(a)(b)?/,"<\\0|\\1|\\2|\\+>"),
 "aba".gsub(/(?<x>a)(b)?/,"<\\0|\\1|\\2|\\+>"),
 "aba".gsub(/(?<x>a)|(?<x>b)/,"<\\k<x>>"),
 "b".sub(/(?<x>a)?(b)/,"<\\k<x>|\\+>"),
 "ab".sub(/a/,"\\\\"),
 "ab".sub(/a/,"\\q"),
 "ab".sub(/z/,"\\k<missing>"),
 "a".sub(/(a)/,"\\10"),
 "A\nB".sub(/a.b/im,"x")
]
"#
        ),
        serde_json::json!([
            "baNAnas",
            "baNANAs",
            "$0\\1b$0\\1",
            "<ab|a|b|b><a|a||a>",
            "<ab|||a><a|||a>",
            "<a><b><a>",
            "<|>",
            "\\b",
            "\\qb",
            "ab",
            "a0",
            "x"
        ])
    );
}

#[test]
fn substitution_preserves_unicode_byte_windows_and_assertion_context() {
    assert_eq!(
        evaluate(
            r#"
[
 "éa".gsub("","-"),
 "éa".gsub(//,"-"),
 "a\xffb".gsub(/./,"\\0").bytes,
 "a\xffb".gsub("\xff","!").bytes,
 "é".gsub("\xa9","x").bytes,
 "abc".gsub(/a*/,"X"),
 "ababa".gsub(/\B/,"X"),
 "a b aa".gsub(/\b/,"X"),
 "ab".gsub(/^|$/,"X"),
 "ab".gsub(/(?:^a$)/,"X"),
 "a b".gsub(/\b/) {"X"}
]
"#
        ),
        serde_json::json!([
            "-é-a-",
            "-é-a-",
            [97, 255, 98],
            [97, 33, 98],
            [195, 120],
            "XbXcX",
            "aXbXaXbXa",
            "XaX XbX XaaX",
            "XabX",
            "ab",
            "XaX XbX"
        ])
    );
}

#[test]
fn bang_results_follow_match_presence_without_changing_receiver_bindings() {
    assert_eq!(
        evaluate(
            r#"
s="aba";alias=s;out=s.gsub!("a","x")
h={text:s};nested=h.text.sub!("a") {"q"}
[s,alias,out,h,nested,s.sub!("a","a"),s.gsub!("a") {"a"},
 s.sub!("z","x"),s.gsub!("z") {"x"},"".sub!("","")]
"#
        ),
        serde_json::json!([
            "aba","aba","xbx",{"text":"aba"},"qba","aba","aba",null,null,""
        ])
    );
}

#[test]
fn blocks_convert_values_and_preserve_parameters_and_control_flow() {
    assert_eq!(
        evaluate(
            r##"
enum Status
Open
end
def early()
 "aba".gsub(/a/) {return 41}
 0
end
seen=[]
result="aba".gsub(/(a)(b)?/) {|whole,extra|seen.push([whole,extra]);next 7}
[
 result,seen,early(),"aba".gsub(/a/) {break 9},"aba".gsub(/a/) {break},
 "a".sub(/a/) {nil},"a".sub(/a/) {[1,nil,:x]},"a".sub(/a/) {{z:2,a:[true]}},
 "a".sub(/a/) {Status},"a".sub(/a/) {Status::Open},"a".sub(/a/) {{a:int}},
 "a".sub(/a/) {/x/i},"a".sub(/a/) {1..3},"a".sub(/a/) {Regexp},
 "aa".gsub(/a/) {"a".match(/a/)},"a".sub(/a/) {"a".match(/a/)[:begin]}
]
"##
        ),
        serde_json::json!([
            "77",
            [["ab", null], ["a", null]],
            41,
            9,
            null,
            "",
            "[1, , x]",
            "{z: 2, a: [true]}",
            "<Enum Status>",
            "Status::Open",
            "<Shape { a: int }>",
            "/x/i",
            "1..3",
            "<object>",
            "aa",
            "<builtin>"
        ])
    );
}

#[test]
fn invalid_signatures_and_references_stop_later_host_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for operation in [
        r#""a".sub("a")"#,
        r#""a".sub("a",nil)"#,
        r#""a".gsub(:a,"x")"#,
        r#""a".gsub(/a/,"x",regex:false)"#,
        r#""a".sub("a","x",regex:1)"#,
        r#""a".gsub("a","x",extra:true)"#,
        r#""a".sub("a","x") {effect()}"#,
        r#""a".gsub!("[",regex:true) {effect()}"#,
        r#""a".sub(/a/,"\\k<missing>")"#,
        r#""a".gsub(/a/,"\\k<name")"#,
    ] {
        assert!(
            engine
                .compile(&format!("{operation};effect()"))
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{operation}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let script = engine
        .compile(
            r#"def argument();effect();"a";end;"a".sub(argument(),argument(),regex:1);effect()"#,
        )
        .unwrap();
    assert!(script.run(CallOptions::default()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn block_cancellation_and_deadlines_prevent_subsequent_callbacks() {
    for method in ["sub", "sub!", "gsub", "gsub!"] {
        for pattern in [r#""a""#, "/a/"] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let token = CancellationToken::new();
            let cancel = token.clone();
            let mut engine = Engine::new();
            engine.register("stop", move |_, _| {
                count.fetch_add(1, Ordering::SeqCst);
                cancel.cancel();
                Ok(Value::bytes(b"x"))
            });
            let script = engine
                .compile(&format!(r#""aaa".{method}({pattern}) {{stop()}};stop()"#))
                .unwrap();
            let error = script
                .run(CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                })
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Cancelled);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let error = script
                .run(CallOptions {
                    deadline: Some(Instant::now()),
                    ..CallOptions::default()
                })
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Deadline);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn fixed_limits_allow_large_literal_shrinking_and_bound_block_conversion() {
    let large = Value::bytes(vec![b'a'; (1 << 20) + 1]);
    for body in [
        r#"text.sub(text,"x")"#,
        r#"text.gsub(text,"x")"#,
        r#"text.sub(text) {"x"}"#,
        r#"text.gsub(text) {"x"}"#,
    ] {
        let script = Engine::new()
            .compile(&format!("def run(text)\n{body}\nend"))
            .unwrap();
        let output = script
            .call("run", std::slice::from_ref(&large), unmetered_steps())
            .unwrap();
        assert_eq!(output.value.as_bytes(), Some(b"x".as_slice()), "{body}");
        assert!(output.stats.retained_memory_bytes < 1024);
    }
    for (body, kind) in [
        (r#"text.sub("a","x",regex:true)"#, ErrorKind::Memory),
        (r#"text.gsub("z") {"x"}"#, ErrorKind::OutputLimit),
        (r#"text.gsub!("z") {"x"}"#, ErrorKind::OutputLimit),
        (r#""a".sub(/a/) {text}"#, ErrorKind::OutputLimit),
        (r#""a".sub(/z/,text)"#, ErrorKind::Memory),
    ] {
        let script = Engine::new()
            .compile(&format!("def run(text)\n{body}\nend"))
            .unwrap();
        assert_eq!(
            script
                .call("run", std::slice::from_ref(&large), unmetered_steps())
                .unwrap_err()
                .kind,
            kind,
            "{body}"
        );
    }
    for body in [
        r#"text.gsub("z",text).bytesize"#,
        r#"text.gsub(text,text).bytesize"#,
    ] {
        let script = Engine::new()
            .compile(&format!("def run(text)\n{body}\nend"))
            .unwrap();
        assert_eq!(
            script
                .call("run", std::slice::from_ref(&large), unmetered_steps())
                .unwrap()
                .value
                .as_int(),
            Some((1 << 20) + 1)
        );
    }
    let script = Engine::new()
        .compile(r#"leaf="x"*65536;items=[leaf];5.times {items=items+items};"a".gsub(/a/) {items}"#)
        .unwrap();
    assert_eq!(
        script.run(unmetered_steps()).unwrap_err().kind,
        ErrorKind::OutputLimit
    );
    for length in [(1 << 20), (1 << 20) + 1] {
        let script = Engine::new()
            .compile(r#"def run(text);("a"+text).sub("a","x").bytesize;end"#)
            .unwrap();
        let input = Value::bytes(vec![b'b'; length - 1]);
        let output = script.call("run", &[input], unmetered_steps());
        if length == 1 << 20 {
            assert_eq!(output.unwrap().value.as_int(), Some(length as i64));
        } else {
            assert_eq!(output.unwrap_err().kind, ErrorKind::OutputLimit);
        }
    }
}

#[test]
fn outputs_and_nonlocal_exits_release_receivers_and_previous_block_values() {
    let subject = Value::bytes(b"a!".repeat(32768));
    for body in [
        r#"text.sub(/a/) {return 9}"#,
        r#"sink(text,text.gsub(/a/) {return 9})"#,
        r#"text.gsub(/a/) {break 9}"#,
    ] {
        let source = format!("def sink(a,b);7;end\ndef run(text)\n{body}\nend");
        let output = Engine::new()
            .compile(&source)
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&subject),
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(output.value.as_int(), Some(9), "{body}");
        assert_eq!(output.stats.retained_memory_bytes, 0, "{body}");
    }
    let output = Engine::new()
        .compile(r#"("a"*64).gsub(/a/) {["x"*1024]}"#)
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(256 << 10),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(
        output.value.as_bytes().unwrap(),
        format!("[{}]", "x".repeat(1024)).repeat(64).as_bytes()
    );
    assert!(output.stats.peak_memory_bytes < 256 << 10);
    let consumer = Engine::new()
        .compile("def run(text);text.bytesize;end")
        .unwrap();
    let imported = consumer
        .call("run", &[output.value], CallOptions::default())
        .unwrap();
    assert_eq!(imported.value.as_int(), Some(1026 * 64));
    assert_eq!(imported.stats.retained_memory_bytes, 0);
}
