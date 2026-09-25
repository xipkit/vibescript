mod common;

use vibescript::{CallOptions, CheckReport, ErrorKind, Script, Value};

// Runtime hash contracts fall into three key families. Builtin keys other than
// string, symbol and any, such as hash<int,int>, reject every hash before any
// stored key is inspected, so even {} fails. Nominal keys validate each stored
// key, so {} passes while string-keyed entries fail. String, symbol and any keys
// admit stored keys. These tests compare checker reports with ordinary execution
// for each family.

fn compile(source: &str) -> Script {
    common::gradual_engine().compile(source).unwrap()
}

fn check_function(script: &Script, name: &str, source: &str) -> CheckReport {
    let report = script
        .check_function(name, &CallOptions::default())
        .unwrap();
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

fn check_main(script: &Script, source: &str) -> CheckReport {
    let report = script
        .check_call("main", &[], &CallOptions::default())
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

fn assert_clean(report: &CheckReport, source: &str) {
    assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
}

fn assert_argument_failure_only(report: &CheckReport, source: &str) {
    assert!(rejects_argument(report), "{source}: {report:?}");
    assert!(!returns_bad_type(report), "{source}: {report:?}");
}

fn main_returns(script: &Script, expected: i64, source: &str) {
    let outcome = script.call("main", &[], CallOptions::default()).unwrap();
    assert_eq!(outcome.value.as_int(), Some(expected), "{source}");
}

fn main_rejects(script: &Script, expected: &str, source: &str) {
    let error = script
        .call("main", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
    assert!(error.message.contains(expected), "{source}: {error}");
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

fn entry(key: &str, value: Value) -> Value {
    Value::hash(vec![(key.as_bytes().to_vec(), value)])
}

#[test]
fn builtin_key_contracts_reject_the_empty_hash_literal() {
    for key in ["int", "bool", "nil", "number", "int?", "int|float"] {
        let source = format!("def run(h:hash<{key},int>)->int;0;end;def main;run({{}});end");
        let script = compile(&source);
        assert_argument_failure_only(&check_main(&script, &source), &source);
        main_rejects(&script, "argument h expected", &source);
        run_rejects(
            &script,
            Value::hash(Vec::new()),
            "argument h expected",
            &source,
        );
        run_rejects(
            &script,
            entry("x", Value::int(1)),
            "argument h expected",
            &source,
        );
    }
}

#[test]
fn nominal_key_contracts_admit_only_the_empty_hash() {
    for prefix in ["class Key;end", "enum Key;Draft;end"] {
        let source = format!("{prefix};def run(h:hash<Key,int>)->int;0;end;def main;run({{}});end");
        let script = compile(&source);
        assert_clean(&check_main(&script, &source), &source);
        main_returns(&script, 0, &source);
        run_accepts(&script, Value::hash(Vec::new()), &source);
        run_rejects(
            &script,
            entry("draft", Value::int(1)),
            "argument h expected",
            &source,
        );
        let source =
            format!("{prefix};def run(h:hash<Key,int>)->int;0;end;def main;run({{draft:1}});end");
        let script = compile(&source);
        assert_argument_failure_only(&check_main(&script, &source), &source);
        main_rejects(&script, "argument h expected", &source);
    }
}

#[test]
fn unreachable_callee_bodies_keep_only_the_argument_failure() {
    let source = "def consume(h:hash<int,int>)->int;\"bad\";end;def run(h:hash<string,int>)->int;consume(h);end;def main;run({});end";
    let script = compile(source);
    // A concrete empty hash and the declared hash<string,int> domain both fail
    // consume's key contract at runtime, so its string return is never reached.
    assert_argument_failure_only(&check_main(&script, source), source);
    assert_argument_failure_only(&check_function(&script, "run", source), source);
    // No supplied value can enter consume's body through this contract.
    let direct = check_function(&script, "consume", source);
    assert_clean(&direct, source);
    main_rejects(&script, "argument h expected hash<int, int>", source);
    run_rejects(
        &script,
        Value::hash(Vec::new()),
        "argument h expected hash<int, int>",
        source,
    );
    run_rejects(
        &script,
        entry("x", Value::int(1)),
        "argument h expected hash<int, int>",
        source,
    );
}

#[test]
fn gradual_inputs_stop_at_impossible_contracts_and_keep_rescue_paths() {
    for (consume, call) in [
        (
            "def consume(h:hash<int,int>)->int;\"bad\";end",
            "consume(h)",
        ),
        (
            "def consume(seed,h:hash<int,int> = seed)->int;\"bad\";end",
            "consume(h)",
        ),
        ("def consume(h)->hash<int,int>;h;end", "consume(h)"),
        (
            "class Box;property data:hash<int,int>;end;def consume(h);b=Box.new;b.data=h;end",
            "consume(h)",
        ),
        (
            "def consume(h);[h].each {|item:hash<int,int>| \"unreachable\"};end",
            "consume(h)",
        ),
    ] {
        let source = format!(
            "{consume};def run(h)->int;begin;{call};\"unreachable\";rescue RuntimeError;0;end;end"
        );
        let script = compile(&source);
        assert_clean(&check_function(&script, "run", &source), &source);
        for argument in [
            Value::nil(),
            Value::int(1),
            Value::hash(Vec::new()),
            entry("x", Value::int(1)),
        ] {
            run_accepts(&script, argument, &source);
        }
    }
}

#[test]
fn empty_arrays_satisfy_element_contracts_with_builtin_hash_keys() {
    let source = "def run(h:array<hash<int,int>>)->int;0;end;def main;run([]);end";
    let script = compile(source);
    assert_clean(&check_main(&script, source), source);
    main_returns(&script, 0, source);
    run_accepts(&script, Value::array(Vec::new()), source);
    run_rejects(
        &script,
        Value::array(vec![Value::hash(Vec::new())]),
        "argument h expected",
        source,
    );
    let source = "def run(h:array<hash<int,int>>)->int;0;end;def main;run([{}]);end";
    let script = compile(source);
    assert_argument_failure_only(&check_main(&script, source), source);
    main_rejects(&script, "argument h expected", source);
}

#[test]
fn impossible_host_results_keep_failure_without_running_callbacks_during_analysis() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vibescript::{HostMethod, Signature};

    let effects = Arc::new(AtomicUsize::new(0));
    let callback = effects.clone();
    let method = HostMethod::new("read", move |_, _, _| {
        callback.fetch_add(1, Ordering::Relaxed);
        Ok(Value::hash(Vec::new()))
    })
    .with_signature(Signature {
        result: "hash<int,int>".into(),
        ..Signature::default()
    })
    .unwrap();
    let mut engine = common::gradual_engine();
    engine.register_method("read", method);
    let source = "def run()->int;begin;read();\"unreachable\";rescue RuntimeError;0;end;end";
    let script = engine.compile(source).unwrap();
    assert_clean(&check_function(&script, "run", source), source);
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(0)
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn empty_hashes_satisfy_value_contracts_with_builtin_hash_keys() {
    let source = "def run(h:hash<string,hash<int,int>>)->int;0;end;def main;run({});end";
    let script = compile(source);
    assert_clean(&check_main(&script, source), source);
    main_returns(&script, 0, source);
    run_accepts(&script, Value::hash(Vec::new()), source);
    run_rejects(
        &script,
        entry("x", Value::hash(Vec::new())),
        "argument h expected",
        source,
    );
    let source = "def run(h:hash<string,hash<int,int>>)->int;0;end;def main;run({x:{}});end";
    let script = compile(source);
    assert_argument_failure_only(&check_main(&script, source), source);
    main_rejects(&script, "argument h expected", source);
}

#[test]
fn ordered_union_fallbacks_skip_impossible_key_contracts() {
    for (prefix, ty) in [
        ("", "hash<int,int>|hash<string,int>"),
        ("", "hash<string,int>|hash<int,int>"),
        ("", "hash<int,int>|any"),
        ("class Key;end;", "hash<int,int>|hash<Key,int>"),
        ("class Key;end;", "hash<Key,int>|hash<int,int>"),
    ] {
        let source = format!("{prefix}def run(h:{ty})->int;0;end;def main;run({{}});end");
        let script = compile(&source);
        assert_clean(&check_main(&script, &source), &source);
        main_returns(&script, 0, &source);
        run_accepts(&script, Value::hash(Vec::new()), &source);
    }
    let source = "def run(h:hash<int,int>|hash<string,int>)->int;0;end;def main;run({x:1});end";
    let script = compile(source);
    assert_clean(&check_main(&script, source), source);
    main_returns(&script, 0, source);
    run_rejects(
        &script,
        entry("x", Value::bytes(b"s".to_vec())),
        "argument h expected",
        source,
    );
    for ty in ["hash<int,int>|hash<Key,int>", "hash<Key,int>|hash<int,int>"] {
        let source = format!("class Key;end;def run(h:{ty})->int;0;end;def main;run({{x:1}});end");
        let script = compile(&source);
        assert_argument_failure_only(&check_main(&script, &source), &source);
        main_rejects(&script, "argument h expected", &source);
    }
}

#[test]
fn union_fallbacks_after_impossible_key_contracts_still_convert_enum_values() {
    let source = "enum Status;Draft;end;def run(h:hash<int,int>|hash<string,Status>)->string;h.x.name;end;def main;run({x: :draft});end";
    let script = compile(source);
    assert_clean(&check_main(&script, source), source);
    let outcome = script.call("main", &[], CallOptions::default()).unwrap();
    assert_eq!(outcome.value.as_bytes(), Some(&b"Draft"[..]), "{source}");
    let outcome = script
        .call(
            "run",
            &[entry("x", Value::symbol(b"draft".to_vec()))],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(outcome.value.as_bytes(), Some(&b"Draft"[..]), "{source}");
}

#[test]
fn string_symbol_and_any_key_contracts_keep_nonempty_hashes() {
    for key in [
        "string",
        "symbol",
        "any",
        "string|int",
        "int|symbol",
        "string|Status",
    ] {
        for literal in ["{}", "{x:1}"] {
            let source = format!(
                "enum Status;Draft;end;def run(h:hash<{key},int>)->int;0;end;def main;run({literal});end"
            );
            let script = compile(&source);
            assert_clean(&check_main(&script, &source), &source);
            main_returns(&script, 0, &source);
        }
    }
    // Symbol arms only accept stored keys through the dedicated whole-contract
    // rule; alongside a nominal arm every stored string key is matched and fails.
    for key in ["symbol|Status", "Status|symbol"] {
        let source = format!(
            "enum Status;Draft;end;def run(h:hash<{key},int>)->int;0;end;def main;run({{draft:1}});end"
        );
        let script = compile(&source);
        assert_argument_failure_only(&check_main(&script, &source), &source);
        main_rejects(&script, "argument h expected", &source);
        run_accepts(&script, Value::hash(Vec::new()), &source);
    }
}
