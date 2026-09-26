//! `dup` copies a value. The removed `clone`, `freeze` and `frozen?` are
//! reported with their rewrites by the surface tests.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn copies_preserve_independent_collection_values() {
    let source = "a: { x: array<string | int> } = {x:[\"a\"]};b=a.dup;b[\"x\"][0]=\"b\";b[\"x\"].push(2);[a,b]";
    assert_eq!(
        result(source),
        serde_json::json!([{"x":["a"]},{"x":["b",2]}])
    );
    let source = "a=[1];a.push(2);b=a.dup;b.clear;[a,b]";
    assert_eq!(result(source), serde_json::json!([[1, 2], []]));
}

#[test]
fn copied_match_data_keeps_protection_and_rendering() {
    for expression in ["m.captures.push(\"x\")", "m.dup.captures[0]=\"x\""] {
        let source = format!(
            "m=\"a\".match(\"(a)\").as(match_data);begin\n{expression}\nrescue=>e\n[e.class,m.dup.to_s,m.captures]\nend"
        );
        assert_eq!(
            result(&source),
            serde_json::json!(["RuntimeError", "a", ["a"]]),
            "{expression}"
        );
    }
    assert_eq!(
        result(
            "m=\"a\".match(\"(a)\").as(match_data);c=m.dup.captures;c.push(\"x\");[m.dup.to_s,m.captures,c]"
        ),
        serde_json::json!(["a", ["a"], ["a", "x"]])
    );
}

#[test]
fn protected_copies_survive_host_transfer() {
    for (producer, ty, mutation, render, rendering) in [
        (
            "\"a\".match(\"(a)\").as(match_data).dup",
            "match_data",
            "x.captures.clear",
            "to_s",
            "a",
        ),
        (
            "begin\nraise \"bad\"\nrescue=>e\ne.dup\nend",
            "error",
            "x.dup.backtrace.push(\"x\")",
            "message",
            "bad",
        ),
    ] {
        let value = Engine::new()
            .compile(producer)
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        let engine = Engine::new();
        let source = format!("def run(x: {ty})\n{mutation}\nend");
        let error = engine
            .compile(&source)
            .unwrap()
            .call("run", std::slice::from_ref(&value), CallOptions::default())
            .unwrap_err();
        assert!(error.message.contains("cannot modify"), "{error}");
        let output = engine
            .compile(&format!("def run(x: {ty}) -> string\nx.dup.{render}\nend"))
            .unwrap()
            .call("run", &[value], CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_bytes(), Some(rendering.as_bytes()));
    }
}

#[test]
fn builtins_are_not_values_to_copy() {
    // A builtin names a call; it is not a value that could be copied and
    // called later.
    let source = "cb=JSON::parse.dup;cb(\"[8]\")";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0416", "V0301", "V0106", "V0310"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("::").unwrap()
    );
    assert_eq!(
        error.diagnostics()[3].span.start,
        source.find("cb(").unwrap()
    );
}

#[test]
fn copies_reuse_accounted_storage_and_preserve_exact_limits() {
    let input = Value::array(vec![Value::bytes(vec![b'x'; 8192])]);
    let engine = Engine::new();
    let script = engine
        .compile("def run(x: array<string>) -> array<string>\nx.dup\nend")
        .unwrap();
    let output = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert!(output.stats.retained_memory_bytes >= 8192);
    for memory in [false, true] {
        let mut options = CallOptions::default();
        if memory {
            options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
        } else {
            options.limits.steps = Some(output.stats.steps);
        }
        script
            .call("run", std::slice::from_ref(&input), options.clone())
            .unwrap();
        if memory {
            options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
        } else {
            options.limits.steps = Some(output.stats.steps - 1);
        }
        let error = script
            .call("run", std::slice::from_ref(&input), options)
            .unwrap_err();
        assert_eq!(
            error.kind,
            if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            }
        );
    }
    let script = engine
        .compile(
            "def run(x: array<string>,n: int) -> int\ni=0;while i<n\na=x.dup;i+=1\nend;42\nend",
        )
        .unwrap();
    let first = script
        .call(
            "run",
            &[input.clone(), Value::int(1)],
            CallOptions::default(),
        )
        .unwrap();
    let repeated = script
        .call("run", &[input, Value::int(64)], CallOptions::default())
        .unwrap();
    assert_eq!(repeated.stats.retained_memory_bytes, 0);
    assert_eq!(
        first.stats.peak_memory_bytes,
        repeated.stats.peak_memory_bytes
    );
}

#[test]
fn arguments_to_dup_are_refused_before_running() {
    let token = CancellationToken::new();
    let cancellation = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = vibescript::Engine::new();
    engine.register("stop", move |_, _| {
        cancellation.cancel();
        Ok(Value::nil())
    });
    engine.register("after", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let source = "begin\n[1].dup(stop());after()\nrescue\nafter()\nensure\nafter()\nend";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0301"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("dup").unwrap()
    );
    assert!(!token.is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
