//! Type errors name the boundary that failed. Inside a program the static
//! checker reports them before it runs; values that arrive at runtime, from
//! a host call, `JSON.parse_as` or a cast, are checked when they arrive.

mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, Error, ErrorKind, Limits, Value, stringify_json};

fn fail(source: &str) -> Error {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .expect_err(source)
}

fn actual(value: Value) -> Error {
    Engine::new()
        .compile("def typed(payload:int)\ntrue\nend")
        .unwrap()
        .call("typed", &[value], CallOptions::default())
        .unwrap_err()
}

/// The code, the source text under the span, and the message of each
/// diagnostic that refuses `source`.
fn refused(source: &str) -> Vec<(String, &str, String)> {
    let error = vibescript::Engine::new()
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| {
            (
                d.code.to_string(),
                &source[d.span.start..d.span.end],
                d.message.clone(),
            )
        })
        .collect()
}

#[test]
fn parameter_defaults_captures_and_returns_name_the_failed_boundary() {
    // Host arguments are checked, and named, when the call starts.
    let script = Engine::new()
        .compile(
            "def one(payload:int)\ntrue\nend\n\
             def keyword(*, payload:int)\ntrue\nend\n\
             def rest(*payload:array<int>)\ntrue\nend\n\
             def options(**payload:hash<string,int>)\ntrue\nend",
        )
        .unwrap();
    let text = || Value::bytes("x");
    for (function, args, keywords, expected) in [
        (
            "one",
            vec![text()],
            vec![],
            "argument payload expected int, got string",
        ),
        (
            "keyword",
            vec![],
            vec![("payload".to_owned(), text())],
            "argument payload expected int, got string",
        ),
        (
            "rest",
            vec![Value::int(1), text()],
            vec![],
            "argument payload expected array<int>, got array<int | string>",
        ),
        (
            "options",
            vec![],
            vec![("b".to_owned(), text()), ("a".to_owned(), Value::int(1))],
            "argument payload expected hash<string, int>, got { a: int, b: string }",
        ),
    ] {
        let error = script
            .call_with_keywords(function, &args, &keywords, CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{function}");
        assert_eq!(error.message, expected, "{function}");
    }
    // A default is checked against its parameter before the program runs.
    assert_eq!(
        refused("def typed(payload:int=\"x\")\ntrue\nend\ntyped()"),
        [(
            "V0101".to_owned(),
            "\"x\"",
            "`payload` is int, found string".to_owned()
        )]
    );
    // Results are checked before the program runs.
    for (source, text, message) in [
        (
            "def typed->int\n\"x\"\nend\ntyped",
            "\"x\"",
            "`typed` returns int, found string",
        ),
        (
            "def typed->int\nreturn \"x\"\nend\ntyped",
            "\"x\"",
            "`typed` returns int, found string",
        ),
        (
            "class C\ndef typed->int\n\"x\"\nend\nend\nC.new.typed",
            "\"x\"",
            "`C#typed` returns int, found string",
        ),
        (
            "module M\ndef self.typed->int\n\"x\"\nend\nend\nM.typed",
            "\"x\"",
            "`M.typed` returns int, found string",
        ),
    ] {
        assert_eq!(
            refused(source),
            [("V0101".to_owned(), text, message.to_owned())],
            "{source}"
        );
    }
    let source = "def typed->int\n[1].each{return \"x\"}\nend\ntyped";
    assert_eq!(
        refused(source),
        [
            (
                "V0101".to_owned(),
                "[1].each{return \"x\"}",
                "`typed` returns int, found array<int>".to_owned()
            ),
            (
                "V0101".to_owned(),
                "\"x\"",
                "`typed` returns int, found string".to_owned()
            ),
        ]
    );
}

#[test]
fn block_patterns_json_and_property_writes_keep_their_subjects() {
    for (source, expected) in [
        (
            "[\"x\"].map{|payload:int|payload}",
            vec![("payload", "the annotation says int, found string")],
        ),
        (
            "[[1,\"x\"]].map{|(a:int,payload:int)|payload}",
            vec![
                ("a", "the annotation says int, found int | string | nil"),
                (
                    "payload",
                    "the annotation says int, found int | string | nil",
                ),
            ],
        ),
        (
            "[[1,\"x\"]].map{|(*payload:array<int>)|payload}",
            vec![(
                "payload",
                "the annotation says array<int>, found array<int | string>",
            )],
        ),
        (
            "[[[1,\"x\"]]].map{|((a,b): array<int>)|a}",
            vec![(
                "",
                "the annotation says array<int>, found array<int | string>?",
            )],
        ),
        (
            "class C\nproperty payload:int\nend\nC.new.payload=\"x\"",
            vec![(
                "\"x\"",
                "argument 1 (`value`) of `C#payload=` is int, found string",
            )],
        ),
        (
            "class C\nproperty payload:int\ndef typed\n@payload=\"x\"\nend\nend\nC.new.typed",
            vec![("\"x\"", "`@payload` is int, found string")],
        ),
        (
            "class C\nproperty payload:int\ndef typed(@payload: int)\nend\nend\nC.new.typed(\"x\")",
            vec![(
                "\"x\"",
                "argument 1 (`payload`) of `C#typed` is int, found string",
            )],
        ),
        (
            "class C\nproperty payload:array<int>\ndef initialize\n@payload=[]\nend\ndef typed\n@payload.push(\"x\")\nend\nend\nC.new.typed",
            vec![(
                "\"x\"",
                "argument 1 (`values`) of `push` is int, found string",
            )],
        ),
    ] {
        let expected: Vec<(String, &str, String)> = expected
            .into_iter()
            .map(|(text, message)| ("V0101".to_owned(), text, message.to_owned()))
            .collect();
        assert_eq!(refused(source), expected, "{source}");
    }
    assert_eq!(
        fail("JSON.parse_as(\"[1,\\\"x\\\"]\",array<int>)").message,
        "JSON.parse_as value expected array<int>, got array<int | string>"
    );
}

#[test]
fn expected_types_keep_shape_spelling_nullability_and_nominal_names() {
    // Removed type spellings such as `Int` and `object` are reported by the
    // surface tests; the canonical ones render as written.
    let script = Engine::new()
        .compile(
            "enum Status\nDraft\nend\nenum Review\nDraft\nend\n\
             def nested(x:array<array<int | string?>>)\ntrue\nend\n\
             def dictionary(x:hash<string,array<number>>)\ntrue\nend\n\
             def record(x:{z:int,a?:string,\"valid?\":bool,...})\ntrue\nend\n\
             def status(x:Status)\ntrue\nend\ndef count(x:int)\ntrue\nend\n\
             def review -> Review\nReview::Draft\nend\ndef kind -> any\nStatus\nend",
        )
        .unwrap();
    let value = |function: &str| {
        script
            .call(function, &[], CallOptions::default())
            .unwrap()
            .value
    };
    for (function, argument, expected) in [
        (
            "nested",
            Value::int(1),
            "argument x expected array<array<int | string?>>, got int",
        ),
        (
            "dictionary",
            Value::int(1),
            "argument x expected hash<string, array<number>>, got int",
        ),
        (
            "record",
            Value::int(1),
            "argument x expected { a?: string, \"valid?\": bool, z: int, ... }, got int",
        ),
        (
            "status",
            value("review"),
            "argument x expected Status, got Review",
        ),
        (
            "count",
            value("kind"),
            "argument x expected int, got enum Status",
        ),
    ] {
        let error = script
            .call(function, &[argument], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, expected, "{function}");
    }
    assert_eq!(
        refused("def typed->{}\n1\nend\ntyped"),
        [(
            "V0101".to_owned(),
            "1",
            "`typed` returns {}, found int".to_owned()
        )]
    );
}

#[test]
fn collection_summaries_sample_deterministically_and_deduplicate_types() {
    assert_eq!(
        actual(Value::array(vec![])).message,
        "argument payload expected int, got array<empty>"
    );
    assert_eq!(
        actual(Value::hash(vec![])).message,
        "argument payload expected int, got {}"
    );
    let mut items = vec![Value::int(1); 16];
    assert_eq!(
        actual(Value::array(items.clone())).message,
        "argument payload expected int, got array<int>"
    );
    items.push(Value::boolean(false));
    assert_eq!(
        actual(Value::array(items)).message,
        "argument payload expected int, got array<int | ...>"
    );
    let mixed = Value::array(vec![
        Value::bytes("x"),
        Value::nil(),
        Value::int(1),
        Value::boolean(false),
        Value::int(2),
    ]);
    assert_eq!(
        actual(mixed).message,
        "argument payload expected int, got array<bool | int | nil | string>"
    );
    for count in [6, 7, 16, 17] {
        let fields: Vec<_> = (0..count)
            .rev()
            .map(|index| {
                (
                    format!("k{index:02}").into_bytes(),
                    if index == 16 {
                        Value::boolean(true)
                    } else {
                        Value::int(index)
                    },
                )
            })
            .collect();
        let expected = match count {
            6 => {
                "argument payload expected int, got { k00: int, k01: int, k02: int, k03: int, k04: int, k05: int }"
            }
            7 | 16 => "argument payload expected int, got hash<string, int>",
            _ => "argument payload expected int, got hash<string, int | ...>",
        };
        assert_eq!(actual(Value::hash(fields.clone())).message, expected);
        let mut reversed = fields;
        reversed.reverse();
        assert_eq!(actual(Value::hash(reversed)).message, expected);
    }
}

#[test]
fn shared_collections_expand_by_path_and_type_summaries_stop_at_sixteen_levels() {
    let item = Value::array(vec![Value::int(1)]);
    assert_eq!(
        actual(Value::array(vec![item.clone(), item])).message,
        "argument payload expected int, got array<array<int>>"
    );
    let mut value = Value::int(1);
    for depth in 1..=18 {
        value = Value::array(vec![value]);
        let suffix = if depth > 16 { "array<...>" } else { "int" };
        let expected = format!(
            "argument payload expected int, got {}{}{}",
            "array<".repeat(depth.min(16)),
            suffix,
            ">".repeat(depth.min(16))
        );
        assert_eq!(actual(value.clone()).message, expected);
    }
    let mut value = Value::int(1);
    for _ in 0..17 {
        value = Value::hash(vec![(b"x".to_vec(), value)]);
    }
    let expected = format!(
        "argument payload expected int, got {}hash<string, ...>{}",
        "{ x: ".repeat(16),
        " }".repeat(16)
    );
    assert_eq!(actual(value).message, expected);
}

#[test]
fn invalid_utf8_in_field_names_remains_available_in_raw_error_messages() {
    let error = actual(Value::hash(vec![(vec![0xff], Value::int(1))]));
    assert_eq!(
        error.message_bytes(),
        b"argument payload expected int, got { \xff: int }"
    );
    assert_eq!(
        error.message,
        "argument payload expected int, got { \u{fffd}: int }"
    );
    let error = Engine::new()
        .compile("def typed(x:{\"\\xff?\":int})\ntrue\nend")
        .unwrap()
        .call("typed", &[Value::int(1)], CallOptions::default())
        .unwrap_err();
    assert_eq!(
        error.message,
        "argument x expected { \"\\xff?\": int }, got int"
    );
}

#[test]
fn failures_preserve_partial_output_and_skip_the_rejected_function_body() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let captured = bytes.clone();
    let mut engine = Engine::new();
    engine.set_output_writer(move |_, value| {
        captured.lock().unwrap().extend_from_slice(value);
        Ok(())
    });
    // A cast of a dynamic value fails before the call it feeds.
    let source = "def typed(value:int)\nprint(\"body\")\nend\nbegin\nprint(\"before\");v: any = \"x\";typed(v.as(int))\nrescue RuntimeError=>e\nprint(\"rescued\");e.message\nend";
    let result = engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        result.value.as_bytes(),
        Some(b"cast value expected int, got string".as_slice())
    );
    assert_eq!(*bytes.lock().unwrap(), b"beforerescued");
    bytes.lock().unwrap().clear();
    let result = engine.compile("class C\ndef to_s->string\nv: any = 7\nv.as(string)\nend\nend\nbegin\nputs(\"first\",C.new,\"never\")\nrescue RuntimeError=>e\ne.message\nend").unwrap().run(CallOptions::default()).unwrap();
    assert_eq!(
        result.value.as_bytes(),
        Some(b"cast value expected string, got int".as_slice())
    );
    assert_eq!(*bytes.lock().unwrap(), b"first\n");
}

#[test]
fn rescued_diagnostics_release_scratch_and_leave_later_calls_independent() {
    let engine = Engine::new();
    let script = engine.compile("def typed(payload:int)\ntrue\nend\ndef run(n: int) -> int\ni=0;while i<n\nbegin\nv: any = {a:[1,\"x\"],b:2}\ntyped(v.as(int))\nrescue RuntimeError=>e\ne.message\nend;i+=1\nend;7\nend").unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: Some(5_000_000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let first = script
        .call("run", &[Value::int(1)], options.clone())
        .unwrap();
    let many = script
        .call("run", &[Value::int(128)], options.clone())
        .unwrap();
    assert_eq!(first.stats.peak_memory_bytes, many.stats.peak_memory_bytes);
    assert_eq!(first.stats.retained_memory_bytes, 0);
    assert_eq!(many.stats.retained_memory_bytes, 0);
    let again = script.call("run", &[Value::int(1)], options).unwrap();
    assert_eq!(again.stats.steps, first.stats.steps);
    assert_eq!(again.stats.peak_memory_bytes, first.stats.peak_memory_bytes);
    assert_eq!(
        stringify_json(&again.value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes(),
        Some(b"7".as_slice())
    );
}
