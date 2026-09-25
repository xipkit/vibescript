mod common;

use vibescript::{CallOptions, CheckReport, ErrorKind, Script, Value};

const CONSUME: &str = "def consume(h:{x?:int})->int;if h.empty?;0;else;\"bad\";end;end";

fn compile(source: &str) -> Script {
    common::gradual_engine().compile(source).unwrap()
}

fn check(script: &Script, name: &str, source: &str) -> CheckReport {
    let report = script
        .check_function(name, &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

fn returns_bad_type(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.contains("Return value: expected int"))
}

fn rejects_argument(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.contains("argument \"h\": expected"))
}

fn run_accepts(script: &Script, argument: Value, source: &str) {
    let outcome = script
        .call("run", &[argument], CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(0), "{source}");
}

fn run_rejects(script: &Script, argument: Value, expected: &str, source: &str) {
    let error = script
        .call("run", &[argument], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
    assert!(error.message.contains(expected), "{source}: {error}");
}

#[test]
fn optional_fields_incompatible_with_generic_hash_values_disappear() {
    let source = format!("{CONSUME};def run(h:hash<string,string?>)->int;consume(h);end");
    let script = compile(&source);
    // Only the empty hash passes the closed optional-int contract, so the
    // string branch of consume is unreachable from run.
    let report = check(&script, "run", &source);
    assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), &source);
    run_accepts(&script, Value::object(Vec::new()), &source);
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::nil())]),
        "argument h expected",
        &source,
    );
    run_rejects(
        &script,
        Value::hash(vec![(b"y".to_vec(), Value::bytes(b"s".to_vec()))]),
        "argument h expected",
        &source,
    );
}

#[test]
fn required_fields_incompatible_with_generic_hash_values_reject() {
    let source =
        "def consume(h:{x:int})->int;0;end;def run(h:hash<string,string?>)->int;consume(h);end";
    let script = compile(source);
    let report = check(&script, "run", source);
    assert!(rejects_argument(&report), "{source}: {report:?}");
    assert!(!returns_bad_type(&report), "{source}: {report:?}");
    run_rejects(
        &script,
        Value::hash(Vec::new()),
        "argument h expected",
        source,
    );
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::bytes(b"s".to_vec()))]),
        "argument h expected",
        source,
    );
}

#[test]
fn overlapping_generic_hash_values_keep_real_return_contradictions() {
    let source = format!("{CONSUME};def run(h:hash<string,int?>)->int;consume(h);end");
    let script = compile(&source);
    let report = check(&script, "run", &source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), &source);
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::int(1))]),
        "return value for consume expected int",
        &source,
    );
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::nil())]),
        "argument h expected",
        &source,
    );
}

#[test]
fn direct_checks_of_shape_contracts_keep_the_invalid_branch() {
    let source = format!("{CONSUME};def run(h:hash<string,string?>)->int;consume(h);end");
    let script = compile(&source);
    let report = check(&script, "consume", &source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
}

#[test]
fn nested_generic_hashes_refine_inner_shapes() {
    let source = "def consume(h:{inner:{x?:int}})->int;if h.inner.empty?;0;else;\"bad\";end;end;def run(h:hash<string,hash<string,string?>>)->int;consume(h);end";
    let script = compile(source);
    let report = check(&script, "run", source);
    assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    run_accepts(
        &script,
        Value::hash(vec![(b"inner".to_vec(), Value::hash(Vec::new()))]),
        source,
    );
    run_rejects(
        &script,
        Value::hash(Vec::new()),
        "argument h expected",
        source,
    );
    run_rejects(
        &script,
        Value::hash(vec![(
            b"inner".to_vec(),
            Value::hash(vec![(b"x".to_vec(), Value::bytes(b"s".to_vec()))]),
        )]),
        "argument h expected",
        source,
    );
}

#[test]
fn nested_generic_hashes_keep_present_empty_inner_shapes() {
    let source = "def consume(h:{inner?:{x?:int}})->int;if h.empty?;0;else;\"bad\";end;end;def run(h:hash<string,hash<string,string?>>)->int;consume(h);end";
    let script = compile(source);
    // An empty inner hash is admitted, so consume can still return a string.
    let report = check(&script, "run", source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), source);
    run_rejects(
        &script,
        Value::hash(vec![(b"inner".to_vec(), Value::hash(Vec::new()))]),
        "return value for consume expected int",
        source,
    );
}

#[test]
fn generic_hashes_without_string_keys_only_reach_empty_shapes() {
    let source = format!("class Key;end;{CONSUME};def run(h:hash<Key,int>)->int;consume(h);end");
    let script = compile(&source);
    let report = check(&script, "run", &source);
    assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), &source);
}

#[test]
fn rejected_hash_key_domains_do_not_invent_successful_nonempty_values() {
    let source = "class Key;end;def consume(h:hash<Key,any>)->int;if h.empty?;0;else;\"bad\";end;end;def run(h:hash<string,int>)->int;consume(h);end";
    let script = compile(source);
    let report = check(&script, "run", source);
    assert!(rejects_argument(&report), "{source}: {report:?}");
    assert!(!returns_bad_type(&report), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), source);
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::int(1))]),
        "argument h expected",
        source,
    );
}

#[test]
fn string_and_symbol_hash_keys_keep_nonempty_successful_values() {
    let source = "def consume(h:hash<symbol,int>)->int;if h.empty?;0;else;\"bad\";end;end;def run(h:hash<string,int>)->int;consume(h);end";
    let script = compile(source);
    let report = check(&script, "run", source);
    assert!(!rejects_argument(&report), "{source}: {report:?}");
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), source);
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::int(1))]),
        "return value for consume expected int",
        source,
    );
}

#[test]
fn incompatible_hash_values_only_reach_empty_hash_contracts() {
    let source = "def consume(h:hash<string,int>)->int;if h.empty?;0;else;\"bad\";end;end;def run(h:hash<string,string>)->int;consume(h);end";
    let script = compile(source);
    let report = check(&script, "run", source);
    assert!(rejects_argument(&report), "{source}: {report:?}");
    assert!(!returns_bad_type(&report), "{source}: {report:?}");
    run_accepts(&script, Value::hash(Vec::new()), source);
    run_accepts(&script, Value::object(Vec::new()), source);
    run_rejects(
        &script,
        Value::hash(vec![(b"x".to_vec(), Value::bytes(b"s".to_vec()))]),
        "argument h expected",
        source,
    );
}

#[test]
fn generic_hash_values_still_coerce_symbols_at_shape_boundaries() {
    let source = "enum State;Ready;Done;end;def consume(h:{state:State})->string;h.state.name;end;def run(h:hash<string,symbol>)->string;consume(h);end";
    let script = compile(source);
    let report = check(&script, "run", source);
    assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    for input in [
        Value::hash(vec![(b"state".to_vec(), Value::symbol(b"ready".to_vec()))]),
        Value::object(vec![(b"state".to_vec(), Value::symbol(b"ready".to_vec()))]),
    ] {
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_bytes(), Some(&b"Ready"[..]));
        let original = &input.as_hash().unwrap()[0].1;
        assert_eq!(original.type_name(), "symbol");
        assert_eq!(original.as_bytes(), Some(&b"ready"[..]));
    }
    run_rejects(
        &script,
        Value::hash(vec![(
            b"state".to_vec(),
            Value::symbol(b"unknown".to_vec()),
        )]),
        "argument h expected",
        source,
    );
}
