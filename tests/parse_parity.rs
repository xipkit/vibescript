//! Programs the Go reference accepts whose reading a mutation sweep against
//! it settled. Each expected value was checked with the Go `vibes run`.

use vibescript::{CallOptions, Engine, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn failure(source: &str) -> String {
    Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .err()
        .unwrap_or_else(|| panic!("{source} ran"))
        .message
}

#[test]
fn lexical_forms_match_the_reference() {
    for (source, expected) in [
        ("=begin\nignored\n=end\n1", serde_json::json!(1)),
        (
            "[:1, :0xFF].map { |s| s.to_s }",
            serde_json::json!(["1", "0xFF"]),
        ),
        ("if true then 5 end % 2", serde_json::json!(1)),
        ("def name=(v)\n  v\nend\n1", serde_json::json!(1)),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn line_breaks_end_expressions_where_the_reference_ends_them() {
    for (source, expected) in [
        // A line expression stops at a suffix on a later line than its operand began.
        ("x = [\n  1,\n  2\n][0]\nx", serde_json::json!([1, 2])),
        ("y = {\n  a: 1\n}[:a]\ny", serde_json::json!({"a": 1})),
        (
            "a = \"z\"\nx = a + \"b\nc\"[0]\nx",
            serde_json::json!("zb\nc"),
        ),
        // A condition may start on the line after its keyword.
        (
            "x = 5\nif\n  x > 1\n  1\nelse\n  2\nend",
            serde_json::json!(1),
        ),
        // A when value list continues after a leading comma.
        (
            "x = case 2\nwhen 1\n  , 2\n  \"x\"\nend\nx",
            serde_json::json!("x"),
        ),
        // A statement or a call with arguments takes a do block from the next line.
        ("x = [1].map\n  do |v| v * 2 end\nx", serde_json::json!([2])),
        (
            "def c(x)\n  x\nend\nif c(1)\n  do 2 end\n  3\nend",
            serde_json::json!(3),
        ),
        // A compound statement continued after its end reads later lines too.
        (
            "def run\n  if true then 5 end + 1\n  -1\nend\nrun()",
            serde_json::json!(5),
        ),
        (
            "def run\n  if true then 1 end.to_s\n  [1, 2].size\nend\nrun()",
            serde_json::json!(0),
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn accepted_forms_fail_where_the_reference_fails_at_run_time() {
    for (source, expected) in [
        (
            "def f\n  def g\n    1\n  end\n  2\nend\nf()",
            "unsupported statement",
        ),
        ("@1 = 2", "no instance context for ivar"),
        (
            "[1].map do |v: int|\n  v\n  |end",
            "argument v type check failed: unknown type v",
        ),
    ] {
        assert_eq!(failure(source), expected, "{source}");
    }
}
