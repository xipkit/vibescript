mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, Value, stringify_json};

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
fn rejected_updates_preserve_local_and_nested_bindings() {
    for (source, expected) in [
        (
            "a=[1];begin\na.insert(-9, 2)\nrescue\na\nend",
            serde_json::json!([1]),
        ),
        (
            "a=[1];begin\na[9]=2\nrescue\na\nend",
            serde_json::json!([1]),
        ),
        (
            "a: [array<int>] = [[1]];begin\na[0][9]=2\nrescue\na\nend",
            serde_json::json!([[1]]),
        ),
        (
            "h={a:[1]};begin\nh[\"a\"].insert(-9, 2)\nrescue\nh\nend",
            serde_json::json!({"a":[1]}),
        ),
        (
            "h={a:[1]};begin\nh[\"a\"][9]=2\nrescue\nh\nend",
            serde_json::json!({"a":[1]}),
        ),
        (
            "a: [array<int>, array<int>] = [[1],[2]];a[1]=begin\na[0].insert(-9, 2)\nrescue\na[0]\nend;a",
            serde_json::json!([[1], [1]]),
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn a_callers_handler_preserves_class_and_instance_fields() {
    let source = "class C\n@@items: array<int> = [1]\ndef self.fail -> array<int>\n@@items.insert(-9, 2)\nend\ndef self.go -> array<int>\nbegin\nfail\nrescue\n@@items\nend\nend\nend\nC.go";
    assert_eq!(result(source), serde_json::json!([1]));
    let source = "class C\ngetter items: array<int>\ndef initialize\n@items=[1]\nend\ndef fail -> array<int>\n@items.insert(-9, 2)\nend\nend\nc=C.new;begin\nc.fail\nrescue\nc.items\nend";
    assert_eq!(result(source), serde_json::json!([1]));
}

#[test]
fn ensure_observes_the_binding_when_an_error_propagates() {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let captured = recorded.clone();
    let mut engine = Engine::new();
    engine.register("record", move |_, args| {
        captured
            .lock()
            .unwrap()
            .push(args[0].as_array().unwrap()[0].as_int().unwrap());
        Ok(Value::nil())
    });
    let error = engine
        .compile("a=[1];begin\na.insert(-9, 2)\nensure\nrecord(a)\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("insert"), "{error}");
    assert_eq!(*recorded.lock().unwrap(), vec![1]);
}

#[test]
fn failed_block_updates_do_not_publish_partial_results() {
    let source = "a=[1,2,3];begin\na.delete_if {|v| raise \"stop\" if v==2;v==1}\nrescue\na\nend";
    assert_eq!(result(source), serde_json::json!([1, 2, 3]));
    // `fill` takes no block now.
    let source = "a=[1,2,3];begin\na.fill {|i| raise \"stop\" if i==1;9}\nrescue\na\nend";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0301", "V0305"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("fill").unwrap()
    );
}

#[test]
fn prior_writes_and_explicit_callback_writes_survive_later_failures() {
    assert_eq!(
        result("a=[1];begin\na.push(2);a.insert(-9, 2)\nrescue\na\nend"),
        serde_json::json!([1, 2])
    );
    assert_eq!(
        result(
            "a=[1,2,3];begin\na.delete_if {|v| a[2]=7;raise \"stop\" if v==2;v==1}\nrescue\na\nend"
        ),
        serde_json::json!([1, 2, 7])
    );
    // Updates with arguments of the wrong type are refused before they run.
    for (source, at) in [
        ("s=\"ab\";begin\ns.insert(:x,\"c\")\nrescue\ns\nend", ":x"),
        ("h={a:1};begin\nh.replace([1])\nrescue\nh\nend", "[1]"),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0101"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, source.find(at).unwrap());
    }
}
