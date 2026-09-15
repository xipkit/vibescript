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
fn lifecycle_helpers_preserve_independent_collection_values() {
    for method in ["clone", "freeze"] {
        let source = format!(
            "a={{x:[\"a\"]}};b=a.{method};b.x[0]=b.x[0].replace(\"b\");b.x.push(2);[a,b,a.frozen?,b.frozen?]"
        );
        assert_eq!(
            result(&source),
            serde_json::json!([{"x":["a"]},{"x":["b",2]},true,true])
        );
        let source = format!("a=[1];a.freeze;a.push(2);b=a.{method};b.clear;[a,b]");
        assert_eq!(result(&source), serde_json::json!([[1, 2], []]));
    }
}

#[test]
fn cloned_and_frozen_match_data_keep_protection_and_rendering() {
    for expression in [
        "m.clone.clear",
        "m.freeze.captures.push(\"x\")",
        "m.clone.captures[0]=\"x\"",
        "m.freeze.captures[0].clear",
        "m.clone[:captures].map! {\"x\"}",
        "m.clone.clone[:captures][0].replace(\"x\")",
    ] {
        let source = format!(
            "m=\"a\".match(\"(a)\");begin\n{expression}\nrescue=>e\n[e.type,m.clone.to_s,m.freeze.captures]\nend"
        );
        assert_eq!(
            result(&source),
            serde_json::json!(["RuntimeError", "a", ["a"]]),
            "{expression}"
        );
    }
    assert_eq!(
        result(
            "m=\"a\".match(\"(a)\");c=m.clone.captures;c.push(\"x\");[m.clone.to_s,m.captures,c]"
        ),
        serde_json::json!(["a", ["a"], ["a", "x"]])
    );
}

#[test]
fn protected_copies_survive_host_transfer() {
    for (producer, mutation, rendering) in [
        ("\"a\".match(\"(a)\").clone", "x.freeze.captures.clear", "a"),
        (
            "begin\nraise \"bad\"\nrescue=>e\ne.clone\nend",
            "x.clone.message.clear",
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
        let source = format!("def run(x)\n{mutation}\nend");
        let error = engine
            .compile(&source)
            .unwrap()
            .call("run", std::slice::from_ref(&value), CallOptions::default())
            .unwrap_err();
        assert!(error.message.contains("cannot modify"), "{error}");
        let output = engine
            .compile("def run(x)\nx.clone.freeze.to_s\nend")
            .unwrap()
            .call("run", &[value], CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_bytes(), Some(rendering.as_bytes()));
    }
}

#[test]
fn builtin_copies_remain_callable_and_wrapped_helpers_use_call_time_receivers() {
    for method in ["clone", "freeze"] {
        assert_eq!(
            result(&format!("cb=JSON::parse.{method};cb(\"[8]\")")),
            serde_json::json!([8])
        );
        assert_eq!(
            result(&format!(
                "m=\"ab\".match(\"(b)\");cb=m[:begin].{method};cb(1)"
            )),
            serde_json::json!(1)
        );
        assert_eq!(
            result(&format!("(JSON::parse.{method} rescue missing)()")),
            serde_json::Value::Null
        );
    }
    assert_eq!(
        result("[JSON::parse.frozen?,(JSON::parse.frozen? rescue missing)()]"),
        serde_json::json!([true, true])
    );
}

#[test]
fn lifecycle_helpers_reuse_accounted_storage_and_preserve_exact_limits() {
    let input = Value::array(vec![Value::bytes(vec![b'x'; 8192])]);
    let engine = Engine::new();
    let baseline = engine
        .compile("def run(x)\nx.dup\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert!(baseline.stats.retained_memory_bytes >= 8192);
    for method in ["clone", "freeze"] {
        let script = engine
            .compile(&format!("def run(x)\nx.{method}\nend"))
            .unwrap();
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(output.stats.steps, baseline.stats.steps);
        assert_eq!(
            output.stats.peak_memory_bytes,
            baseline.stats.peak_memory_bytes
        );
        assert_eq!(
            output.stats.retained_memory_bytes,
            baseline.stats.retained_memory_bytes
        );
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
    }
    let script = engine
        .compile("def run(x,n)\ni=0;while i<n\na=x.clone.freeze;i+=1\nend;42\nend")
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
fn cancellation_during_arguments_prevents_fallback_and_following_effects() {
    for method in ["clone", "freeze", "frozen?"] {
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let mut engine = Engine::new();
        engine.register("stop", move |_, _| {
            cancellation.cancel();
            Ok(Value::nil())
        });
        engine.register("after", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        let source =
            format!("begin\n[1].{method}(stop());after()\nrescue\nafter()\nensure\nafter()\nend");
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                cancellation: token,
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
