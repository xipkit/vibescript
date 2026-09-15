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

#[test]
fn parameter_defaults_captures_and_returns_name_the_failed_boundary() {
    for (source, expected) in [
        (
            "def typed(payload:int)\ntrue\nend\ntyped(\"x\")",
            "argument payload expected int, got string",
        ),
        (
            "def typed(payload:int=\"x\")\ntrue\nend\ntyped()",
            "argument payload expected int, got string",
        ),
        (
            "def typed(payload:int:)\ntrue\nend\ntyped(payload:\"x\")",
            "argument payload expected int, got string",
        ),
        (
            "def typed(*payload:array<int>)\ntrue\nend\ntyped(1,\"x\")",
            "argument payload expected array<int>, got array<int | string>",
        ),
        (
            "def typed(**payload:hash<string,int>)\ntrue\nend\ntyped(b:\"x\",a:1)",
            "argument payload expected hash<string, int>, got { a: int, b: string }",
        ),
        (
            "def typed->int\n\"x\"\nend\ntyped()",
            "return value for typed expected int, got string",
        ),
        (
            "def typed->int\nreturn \"x\"\nend\ntyped()",
            "return value for typed expected int, got string",
        ),
        (
            "def typed->int\n[1].each{return \"x\"}\nend\ntyped()",
            "return value for typed expected int, got string",
        ),
        (
            "class C\ndef typed->int\n\"x\"\nend\nend\nC.new.typed()",
            "return value for typed expected int, got string",
        ),
        (
            "module M\ndef self.typed->int\n\"x\"\nend\nend\nM.typed()",
            "return value for typed expected int, got string",
        ),
    ] {
        let error = fail(source);
        assert_eq!(error.kind, ErrorKind::Type, "{source}");
        assert_eq!(error.message, expected, "{source}");
    }
}

#[test]
fn block_patterns_json_and_property_writes_keep_their_subjects() {
    for (source, expected) in [
        (
            "[\"x\"].map{|payload:int|payload}",
            "argument payload expected int, got string",
        ),
        (
            "[[1,\"x\"]].map{|(a:int,payload:int)|payload}",
            "argument payload expected int, got string",
        ),
        (
            "[[1,\"x\"]].map{|(*payload:array<int>)|payload}",
            "argument payload expected array<int>, got array<int | string>",
        ),
        (
            "[[[1,\"x\"]]].map{|((a,b): array<int>)|a}",
            "argument (a, b) expected array<int>, got array<int | string>",
        ),
        (
            "JSON.parse_as(\"[1,\\\"x\\\"]\",array<int>)",
            "JSON.parse_as value expected array<int>, got array<int | string>",
        ),
        (
            "class C\nproperty payload:int\nend\nC.new.payload=\"x\"",
            "argument value expected int, got string",
        ),
        (
            "class C\nproperty payload:int\ndef typed\n@payload=\"x\"\nend\nend\nC.new.typed()",
            "instance variable @payload expected int, got string",
        ),
        (
            "class C\nproperty payload:int\ndef typed(@payload)\nend\nend\nC.new.typed(\"x\")",
            "instance variable @payload expected int, got string",
        ),
        (
            "class C\nproperty payload:array<int>\ndef initialize\n@payload=[]\nend\ndef typed\n@payload.push(\"x\")\nend\nend\nC.new.typed()",
            "instance variable @payload expected array<int>, got array<string>",
        ),
    ] {
        assert_eq!(fail(source).message, expected, "{source}");
    }
}

#[test]
fn expected_types_keep_shape_spelling_nullability_and_nominal_names() {
    for (source, expected) in [
        (
            "def typed(x:array<array<Int | STRING?>>)\ntrue\nend\ntyped(1)",
            "argument x expected array<array<int | string?>>, got int",
        ),
        (
            "def typed(x:object<symbol,array<number>>)\ntrue\nend\ntyped(1)",
            "argument x expected object<symbol, array<number>>, got int",
        ),
        (
            "def typed(x:{z:int,a?:string,\"valid?\":bool,...})\ntrue\nend\ntyped(1)",
            "argument x expected { a?: string, \"valid?\": bool, z: int, ... }, got int",
        ),
        (
            "def typed->{}\n1\nend\ntyped()",
            "return value for typed expected {}, got int",
        ),
        (
            "enum Status\nDraft\nend\nenum Review\nDraft\nend\ndef typed(x:Status)\ntrue\nend\ntyped(Review::Draft)",
            "argument x expected Status, got Review",
        ),
        (
            "enum Status\nDraft\nend\ndef typed(x:int)\ntrue\nend\ntyped(Status)",
            "argument x expected int, got enum Status",
        ),
    ] {
        assert_eq!(fail(source).message, expected, "{source}");
    }
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
    let error = fail("def typed(x:{\"\\xff?\":int})\ntrue\nend\ntyped(1)");
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
    let source = "class C\ndef to_s->string\n7\nend\nend\ndef typed(value:int)\nprint(\"body\")\nend\nbegin\nprint(\"before\");typed(C.new)\nrescue RuntimeError=>e\nprint(\"rescued\");e.message\nend";
    let result = engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        result.value.as_bytes(),
        Some(b"argument value expected int, got instance".as_slice())
    );
    assert_eq!(*bytes.lock().unwrap(), b"beforerescued");
    bytes.lock().unwrap().clear();
    let result = engine.compile("class C\ndef to_s->string\n7\nend\nend\nbegin\nputs(\"first\",C.new,\"never\")\nrescue RuntimeError=>e\ne.message\nend").unwrap().run(CallOptions::default()).unwrap();
    assert_eq!(
        result.value.as_bytes(),
        Some(b"return value for to_s expected string, got int".as_slice())
    );
    assert_eq!(*bytes.lock().unwrap(), b"first\n");
}

#[test]
fn rescued_diagnostics_release_scratch_and_leave_later_calls_independent() {
    let engine = Engine::new();
    let script = engine.compile("def typed(payload:int)\ntrue\nend\ndef run(n)\ni=0;while i<n\nbegin\ntyped({a:[1,\"x\"],b:2})\nrescue RuntimeError=>e\ne.message\nend;i+=1\nend;7\nend").unwrap();
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
