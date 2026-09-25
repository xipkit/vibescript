mod common;

use vibescript::{CallOptions, CheckReport, ErrorKind, Script, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn compile(source: &str) -> Script {
    common::gradual_engine()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
}

fn check(script: &Script, source: &str) -> CheckReport {
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

fn returns_bad_type(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.contains("Return value: expected int"))
}

fn rejects_member(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.contains("does not accept receiver"))
}

#[test]
fn block_chunk_results_are_typed_rows() {
    for (source, expected) in [
        (
            "def run -> array; [1,1,2].chunk { |n| n }; end",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
        (
            "def run -> array; keys=[:a,nil,:a,:_alone]; [0,1,2,3].chunk { |n| keys[n] }; end",
            serde_json::json!([["a", [0]], ["a", [2]], ["_alone", [3]]]),
        ),
        (
            "def run -> array; [].chunk { |n| n }; end",
            serde_json::json!([]),
        ),
        (
            "def run(xs:array<int>) -> array; xs.chunk { |n| n % 2 == 0 }; end",
            serde_json::json!([]),
        ),
        (
            "def run -> array; a=[1,1,2]; a.send(:chunk) { |n| n }; end",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let args: Vec<Value> = if source.contains("xs:array") {
            vec![Value::array(vec![])]
        } else {
            vec![]
        };
        let value = script
            .call("run", &args, CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(json(&value), expected, "{source}");
    }
}

#[test]
fn rows_pair_the_key_with_an_array_of_items() {
    // The first row's key and group element types flow to the return check.
    for source in [
        "def run -> int; [1,1,2].chunk { |n| n }.first.first; end",
        "def run -> int; [1,1,2].chunk { |n| n }.first.last.first; end",
        "def run -> int; [1,1,2].chunk { |n| :k }.first.last.length; end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
    }
    let source = "def run -> string; [1,1,2].chunk { |n| :k }.first.last.first; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Return value: expected string")),
        "{source}: {report:?}"
    );
}

#[test]
fn optional_keys_preserve_the_empty_result_path() {
    for (source, key) in [
        (
            "def run(key: int | nil) -> array; [1].chunk { key }.first; end",
            Value::nil(),
        ),
        (
            "def run(key: symbol) -> array; [1].chunk { key }.first; end",
            Value::symbol(b"_separator".to_vec()),
        ),
        (
            "def run(key: int | nil) -> array; [1,2].chunk { |n| next key if n == 1; nil }.first; end",
            Value::nil(),
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Return value: expected array")),
            "{source}: {report:?}"
        );
        let error = script
            .call("run", &[key], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
    }
}

#[test]
fn argument_and_keyword_forms_end_the_path_before_the_return() {
    for (source, message) in [
        (
            "def run -> int; [1,2,3].chunk(2) { |x| x }; 'bad'; end",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "def run -> int; [1,2,3].chunk(k: 1) { |x| x }; 'bad'; end",
            "array.chunk does not take keyword arguments",
        ),
        (
            "def run -> int; [1].send(:chunk, 2) { |x| x }; 'bad'; end",
            "array.chunk does not take arguments when a block is supplied",
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(rejects_member(&report), "{source}: {report:?}");
        assert!(!returns_bad_type(&report), "{source}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}: {error}");
        assert_eq!(error.message, message, "{source}");
    }
}

#[test]
fn reserved_keys_keep_rescues_reachable() {
    // A literal reserved key always raises, so the rescue arm is the only way
    // to the bad return.
    for source in [
        "def run -> int; begin; [1].chunk { :_bad }; 0; rescue; 'bad'; end; end",
        "def run -> int; begin; [1].chunk(2) { |x| x }; 0; rescue; 'bad'; end; end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(returns_bad_type(&report), "{source}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
    }
    // An unknown symbol key may be reserved, so the rescue stays reachable
    // without being certain.
    let source = "def run(k:symbol) -> int; begin; [1].chunk { k }.length; rescue; 'bad'; end; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    // Ordinary keys never raise, so the rescue arm is unreachable and clean.
    let source = "def run -> int; begin; [1,2].chunk { |n| n }.length; rescue; 'bad'; end; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(report.is_clean(), "{source}: {report:?}");
    let value = script
        .call("run", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(2));
}

#[test]
fn block_exits_and_sized_forms_stay_clean() {
    for (source, expected) in [
        (
            "def run -> int; [1,2,3].chunk { |n| break 7 if n==2; n }; end",
            7,
        ),
        (
            "def run -> int; [1,2,3].chunk { |n| return 9 if n==2; n }; 4; end",
            9,
        ),
        ("def run -> int; [1,2,3].chunk(2).length; end", 2),
        (
            "def run -> int; a=[1,1,2]; a.chunk { |n| n }; a.length; end",
            3,
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let value = script
            .call("run", &[], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
}

#[test]
fn block_effects_are_observed_by_the_checker() {
    // The block runs once per item; a value pushed inside is visible after.
    let source = "def run -> int; seen=[]; [1,1,2].chunk { |n| seen.push(n); n }; seen.length; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(report.is_clean(), "{source}: {report:?}");
    // A wrong-typed block result on a typed key is still an ordinary value,
    // so no diagnostic; only the return check can fail.
    let source = "def run -> int; [1,1,2].chunk { |n| n }; 'bad'; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
}
