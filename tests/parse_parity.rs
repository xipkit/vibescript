//! Programs the Go reference accepts whose reading a mutation sweep against
//! it settled. Each expected value was checked with the Go `vibes run`.

mod common;

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
        ("if true then 5 else 4 end % 2", serde_json::json!(1)),
        ("def name=(v: int)\n  v\nend\n1", serde_json::json!(1)),
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
        // A compound statement continued after its end reads later lines too.
        (
            "def run -> int\n  if true then 5 else 0 end + 1\n  -1\nend\nrun",
            serde_json::json!(5),
        ),
        (
            "def run -> string?\n  if true then 1 else 2 end.to_s\n  [1, 2]\nend\nrun",
            serde_json::json!(""),
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn accepted_forms_fail_where_the_reference_fails_at_run_time() {
    let source = "def f -> int\n  def g -> int\n    1\n  end\n  2\nend\nf";
    assert_eq!(failure(source), "unsupported statement");
}

#[test]
fn forms_the_reference_rejects_at_run_time_are_refused_at_compile_time() {
    for (source, codes, at) in [
        // An instance variable outside any class.
        ("@1 = 2", &["V0204"][..], "@1"),
        // A block parameter's type continues after a line break.
        ("[1].map { |v: int|\n  v\n  |}", &["V0116"], "v"),
        // A statement or a call with arguments took a do block from the next
        // line; do blocks are removed.
        ("x = [1].map\n  do |v| v * 2 end\nx", &["V0406"], "do"),
        (
            "def c(x: int) -> int\n  x\nend\nif c(1)\n  do 2 end\n  3\nend",
            &["V0104", "V0406"],
            "do",
        ),
    ] {
        let error = common::static_engine().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), codes, "{source}");
        let last = error.diagnostics().last().unwrap();
        assert_eq!(last.span.start, source.find(at).unwrap(), "{source}");
    }
}
