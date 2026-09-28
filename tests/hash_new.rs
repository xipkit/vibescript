mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn counters(stats: vibescript::Stats) -> (u64, usize, usize) {
    (
        stats.steps,
        stats.peak_memory_bytes,
        stats.retained_memory_bytes,
    )
}

#[test]
fn hash_constructors_are_refused_in_favour_of_literals() {
    // `Hash.new` was removed for `{}` with a declared type, in every call
    // form, so none of its arguments or blocks run.
    for source in [
        "Hash::new()",
        "Hash.new(mark(1),flag:mark(2)){mark(3)}",
        "Hash.new{mark(1)}",
        "Hash.new.call(mark(1))",
        "Hash.new(*[],**{})",
    ] {
        let mut engine = vibescript::Engine::new();
        engine.register("mark", |_, _| panic!("mark ran"));
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0411"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, 0, "{source}");
    }
}

#[test]
fn retained_empty_hashes_are_charged_and_discarded_constructors_stay_bounded() {
    let script = Engine::new()
        .compile(
            "def hold(n: int) -> array<hash<string, int>>\nout: array<hash<string, int>> = []\nfor i in 1..n\nout.push({})\nend\nout\nend\n\
         def discard(n: int)\nfor i in 1..n\n{}\nend\nnil\nend",
        )
        .unwrap();
    let small = script
        .call("hold", &[Value::int(32)], CallOptions::default())
        .unwrap();
    let large = script
        .call("hold", &[Value::int(128)], CallOptions::default())
        .unwrap();
    assert_eq!(large.value.as_array().unwrap().len(), 128);
    assert!(large.stats.retained_memory_bytes > 3 * small.stats.retained_memory_bytes);
    let limited = CallOptions {
        limits: Limits {
            memory_bytes: Some(small.stats.peak_memory_bytes),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call("hold", &[Value::int(128)], limited)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let short = script
        .call("discard", &[Value::int(32)], CallOptions::default())
        .unwrap();
    let long = script
        .call("discard", &[Value::int(512)], CallOptions::default())
        .unwrap();
    assert_eq!(short.stats.peak_memory_bytes, long.stats.peak_memory_bytes);
    assert_eq!(long.stats.retained_memory_bytes, 0);
    let fresh = script
        .call("hold", &[Value::int(32)], CallOptions::default())
        .unwrap();
    assert_eq!(counters(small.stats), counters(fresh.stats));
}

#[test]
fn hash_constructor_work_and_memory_limits_release_call_storage() {
    let script = Engine::new()
        .compile("def run -> hash<string, array<hash<string, int>>>\na: hash<string, array<hash<string, int>>> = {}\nb: hash<string, int> = {}\na[\"x\"]=[b,{}]\na\nend")
        .unwrap();
    let baseline = script.call("run", &[], CallOptions::default()).unwrap();
    for steps in 0..baseline.stats.steps {
        let options = CallOptions {
            limits: Limits {
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Steps,
            "steps={steps}"
        );
    }
    let peak = baseline.stats.peak_memory_bytes;
    for memory in (0..peak).step_by((peak / 37).max(1)).chain([peak - 1]) {
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.call("run", &[], options).unwrap_err().kind,
            ErrorKind::Memory,
            "memory={memory}"
        );
    }
    let fresh = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&fresh.value), serde_json::json!({"x":[{},{}]}));
    assert_eq!(counters(baseline.stats), counters(fresh.stats));
}

#[test]
fn cancelled_constructor_arguments_cannot_be_rescued_and_fresh_calls_succeed() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, args| {
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            "def run(stop: bool) -> any\nbegin\nif stop\n{a: cancel(), b: mark(1)}\nelse\n{}\nend\n\
         rescue RuntimeError\nmark(2)\nensure\nmark(3)\nend\nend",
        )
        .unwrap();
    assert_eq!(
        script
            .call("run", &[Value::boolean(true)], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert!(events.lock().unwrap().is_empty());
    let fresh = script
        .call("run", &[Value::boolean(false)], CallOptions::default())
        .unwrap();
    assert_eq!(json(&fresh.value), serde_json::json!({}));
    assert_eq!(*events.lock().unwrap(), [3]);
}
