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
fn hash_constructors_and_namespace_aliases_produce_independent_values() {
    let script = Engine::new()
        .compile(
            r#"
def fresh
 Hash.new
end
def run
 ns=Hash
 values=[Hash.new,Hash::new(),Hash["new"](),ns.send(:new),fresh]
 old=values
 values[0][:x]=1
 values[1][:x]=2
 [values,old,Hash.new.fetch(:missing,7),Hash.new[:missing],Hash.new.tap{|h|h[:x]=1}]
end
"#,
        )
        .unwrap();
    for _ in 0..3 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([[{"x":1},{"x":2},{},{},{}],[{},{},{},{},{}],7,null,{}])
        );
    }
}

#[test]
fn hash_constructor_validation_preserves_argument_order_and_skips_blocks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |ctx, args| {
        ctx.charge(1)?;
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(args[0].clone())
    });
    for (call, expected, message) in [
        (
            "Hash.new(mark(1),flag:mark(2)){mark(3)}",
            vec![1, 2],
            "Hash.new does not accept keyword arguments",
        ),
        (
            "Hash.public_send(:new,mark(1)){mark(2)}",
            vec![1],
            "Hash.new takes no default",
        ),
        ("Hash.new{mark(1)}", vec![], "Hash.new takes no default"),
        ("Hash.new.call(mark(1))", vec![], "unknown hash method call"),
    ] {
        events.lock().unwrap().clear();
        let error = engine
            .compile(call)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert!(error.message.starts_with(message), "{call}: {error}");
        assert_eq!(*events.lock().unwrap(), expected, "{call}");
    }
    assert_eq!(
        json(
            &engine
                .compile("Hash.new(*[],**{})")
                .unwrap()
                .run(CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!({})
    );
}

#[test]
fn retained_empty_hashes_are_charged_and_discarded_constructors_stay_bounded() {
    let script = Engine::new()
        .compile(
            "def hold(n)\nout=[]\nfor i in 1..n\nout.push(Hash.new)\nend\nout\nend\n\
         def discard(n)\nfor i in 1..n\nHash.new\nend\nnil\nend",
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
        .compile("def run\na=Hash.new\nb=Hash::new()\na[:x]=[b,Hash.send(:new)]\na\nend")
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
            "def run(stop)\nbegin\nif stop\nHash.new(cancel(),mark(1))\nelse\nHash.new\nend\n\
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
