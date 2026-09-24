//! The declarations ADR-007 adds, enforced at runtime like parameter
//! annotations: typed locals, tuple types and the newer type names.

use vibescript::{CallOptions, Engine, ErrorKind, stringify_json};

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

/// The runtime failure of `source`, with its one-based line and column.
fn failure(source: &str) -> (ErrorKind, String, (usize, usize)) {
    let error = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .expect_err(source);
    let position = error.diagnostic.as_ref().unwrap().position;
    (error.kind, error.message, (position.line, position.column))
}

/// A compile error as `vibes` prints it, with its one-based position.
fn compile_error(source: &str) -> String {
    let Err(error) = Engine::new().compile(source) else {
        panic!("{source} compiled");
    };
    let position = error.diagnostic.as_ref().unwrap().position;
    format!(
        "parse error at {}:{}: {}",
        position.line, position.column, error.message
    )
}

#[test]
fn typed_locals_take_their_declared_type() {
    for (source, expected) in [
        ("x: int = 1\nx = x + 2\nx", serde_json::json!(3)),
        (
            "label: string? = nil\nlabel = \"ready\"\nlabel",
            serde_json::json!("ready"),
        ),
        (
            "names: array<string> = []\nnames << \"Ada\"\nnames",
            serde_json::json!(["Ada"]),
        ),
        (
            "counts: hash<string, int> = {}\ncounts[\"a\"] = 1\ncounts",
            serde_json::json!({"a": 1}),
        ),
        (
            "total: number = 1\ntotal = 2.5\ntotal",
            serde_json::json!(2.5),
        ),
        ("x: int = 1\nx += 2\nx *= 3\nx", serde_json::json!(9)),
        ("x: int? = nil\nx ||= 4\nx", serde_json::json!(4)),
        (
            "def sum(items: array<int>) -> int\n  total: int = 0\n  items.each { |item| total += item }\n  for item in items\n    total = total + item\n  end\n  total\nend\nsum([1, 2])",
            serde_json::json!(6),
        ),
        (
            "a: int = 1\nb = 2\na, b = b, a\n[a, b]",
            serde_json::json!([2, 1]),
        ),
        (
            "x: int = 1\n[1].map { |x| x = \"shadowed\" }",
            serde_json::json!(["shadowed"]),
        ),
        ("x: int = 1 if true\nx", serde_json::json!(1)),
        ("type = 3\ntype + 1", serde_json::json!(4)),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn typed_locals_check_every_assignment() {
    for (source, message, at) in [
        (
            "x: int = \"a\"",
            "local variable x expected int, got string",
            (1, 1),
        ),
        (
            "x: int = 1\nx = \"a\"",
            "local variable x expected int, got string",
            (2, 1),
        ),
        (
            "x: int = 1\nx += 1.5",
            "local variable x expected int, got float",
            (2, 1),
        ),
        (
            "x: int = 1\nx = x + \"\".length.to_f",
            "local variable x expected int, got float",
            (2, 1),
        ),
        (
            "x: int? = nil\nx ||= \"a\"",
            "local variable x expected int?, got string",
            (2, 1),
        ),
        (
            "x: int = 1\n[1, 2].each { |i| x = \"s\" }",
            "local variable x expected int, got string",
            (2, 19),
        ),
        (
            "x: int = 1\n[[1]].each { |pair| [2].each { |i| x = nil } }",
            "local variable x expected int, got nil",
            (2, 36),
        ),
        (
            "x: int = 1\nfor x in [\"a\"]\nend",
            "local variable x expected int, got string",
            (2, 5),
        ),
        (
            "x: int = 1\ny = 2\nx, y = \"a\", 1",
            "local variable x expected int, got string",
            (3, 1),
        ),
        (
            "names: array<string> = [1]",
            "local variable names expected array<string>, got array<int>",
            (1, 1),
        ),
        (
            "x: Missing = 1",
            "local variable x type check failed: unknown type Missing",
            (1, 1),
        ),
    ] {
        let (kind, actual, position) = failure(source);
        assert_eq!(kind, ErrorKind::Type, "{source}");
        assert_eq!((actual.as_str(), position), (message, at), "{source}");
    }
}

#[test]
fn malformed_typed_locals_are_positioned_parse_errors() {
    for (source, message) in [
        (
            "x: int\n",
            "parse error at 1:7: typed local x needs a value; write x: T = value",
        ),
        (
            "def f\n  total: array<int>\nend",
            "parse error at 2:20: typed local total needs a value; write total: T = value",
        ),
        (
            "x: [] = []",
            "parse error at 1:4: a tuple type needs at least one element type, as in [int, string]",
        ),
        (
            "x: array<int, string> = []",
            "parse error at 1:4: array type expects exactly 1 type argument",
        ),
        (
            "x: type<int, string> = int",
            "parse error at 1:4: type expects exactly 1 type argument",
        ),
        (
            "x: [int string] = []",
            "parse error at 1:9: expected \"]\", got identifier",
        ),
        // What only resembles a declaration keeps the error it always had.
        ("x: Missing", "parse error at 1:2: unexpected token \":\""),
        ("x:int = 1", "parse error at 1:2: unexpected token \":\""),
        ("id : a", "parse error at 1:4: unexpected token \":\""),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn tuple_types_are_arrays_of_exactly_their_elements() {
    let enums = "enum Status\n  Draft\n  Done\nend\n";
    for (source, expected) in [
        (
            "def f(x: [int, string]) -> [int, string]\n  x\nend\nf([1, \"a\"])".to_owned(),
            serde_json::json!([1, "a"]),
        ),
        (
            "def f(x: [int, string]?) -> [int, string]?\n  x\nend\nf(nil)".to_owned(),
            serde_json::json!(null),
        ),
        (
            "def f(pair: [int, int] = [1, 2]) -> array<int>\n  pair\nend\nf()".to_owned(),
            serde_json::json!([1, 2]),
        ),
        // A bracket whose leaves are not all types stays a default.
        (
            "def f(a, b, pair: [a, b])\n  pair\nend\nf(1, 2)".to_owned(),
            serde_json::json!([1, 2]),
        ),
        (
            "def f(x: [])\n  x\nend\nf()".to_owned(),
            serde_json::json!([]),
        ),
        (
            format!("{enums}def f(x: [Status, int]) -> string\n  x[0].name\nend\nf([:done, 1])"),
            serde_json::json!("Done"),
        ),
        (
            "[[1, \"a\"]].map { |p: [int, string]| p[1] }".to_owned(),
            serde_json::json!(["a"]),
        ),
    ] {
        assert_eq!(evaluate(&source), expected, "{source}");
    }
    for (source, message) in [
        (
            "def f(pair: [int, string])\nend\nf([1, 2])",
            "argument pair expected [int, string], got array<int>",
        ),
        (
            "def f(pair: [int, string])\nend\nf([1, \"a\", 3])",
            "argument pair expected [int, string], got array<int | string>",
        ),
        (
            "def f(pair: [int, string])\nend\nf({ a: 1 })",
            "argument pair expected [int, string], got { a: int }",
        ),
        (
            "def f -> [int, [string, bool]]\n  [1, [\"a\", 2]]\nend\nf",
            "return value for f expected [int, [string, bool]], got array<array<int | string> | int>",
        ),
    ] {
        let (kind, actual, _) = failure(source);
        assert_eq!(
            (kind, actual.as_str()),
            (ErrorKind::Type, message),
            "{source}"
        );
    }
    for (source, message) in [
        (
            "def f -> []\nend",
            "parse error at 1:10: a tuple type needs at least one element type, as in [int, string]",
        ),
        (
            "def f -> [int string]\nend",
            "parse error at 1:15: expected \"]\", got identifier",
        ),
        (
            "def f(x: array<int, string>)\nend",
            "parse error at 1:10: array type expects exactly 1 type argument",
        ),
    ] {
        assert_eq!(compile_error(source), message, "{source}");
    }
}

#[test]
fn newer_type_names_validate_their_values() {
    let error = "e = begin\n  raise \"bad\"\nrescue => err\n  err\nend\n";
    assert_eq!(
        evaluate(&format!(
            "{error}def f(r: regex, m: match_data, e: error, t: type<array<int>>)\n  [r.source, m[0], e.message]\nend\nf(/a/, \"abc\".match(\"b\"), e, array<int>)"
        )),
        serde_json::json!(["a", "b", "bad"])
    );
    for (source, message) in [
        (
            "def f(r: regex)\nend\nf(\"a\")",
            "argument r expected regex, got string",
        ),
        (
            "def f(m: match_data)\nend\nf({ captures: [] })",
            "argument m expected match_data, got { captures: array<empty> }",
        ),
        (
            "def f(e: error)\nend\nf(\"x\")",
            "argument e expected error, got string",
        ),
        (
            "def f(t: type<int>)\nend\nf(1)",
            "argument t expected type<int>, got int",
        ),
        (
            "def f(m: match_data?) -> match_data\n  m\nend\nf(\"a\".match(\"z\"))",
            "return value for f expected match_data, got nil",
        ),
    ] {
        let (kind, actual, _) = failure(source);
        assert_eq!(
            (kind, actual.as_str()),
            (ErrorKind::Type, message),
            "{source}"
        );
    }
    assert_eq!(
        compile_error("def f(t: type<int, string>)\nend"),
        "parse error at 1:10: type expects exactly 1 type argument"
    );
    // The newer names are lowercase only, so a class named Error keeps naming itself.
    assert_eq!(
        evaluate("class Error\nend\ndef f(e: Error) -> bool\n  true\nend\nf(Error.new)"),
        serde_json::json!(true)
    );
    // A rescued error bound to `error` is still a value where a type could be meant.
    assert_eq!(
        evaluate(
            "def show(x)\n  x.message\nend\nbegin\n  raise \"bad\"\nrescue => error\n  show(error)\nend"
        ),
        serde_json::json!("bad")
    );
}

#[test]
fn the_gradual_checker_accepts_the_new_declarations() {
    let source = "def run -> int
  pair: [int, string] = [1, \"a\"]
  total: int = pair[0]
  [1, 2].each { |i| total += i }
  total
end
";
    let script = Engine::new().compile(source).unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(4)
    );
}
