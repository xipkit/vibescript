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
            "a=[1];begin\na.insert()\nrescue\na\nend",
            serde_json::json!([1]),
        ),
        (
            "a=[1];begin\na[9]=2\nrescue\na\nend",
            serde_json::json!([1]),
        ),
        (
            "a=[[1]];begin\na[0][9]=2\nrescue\na\nend",
            serde_json::json!([[1]]),
        ),
        (
            "h={a:[1]};begin\nh.a.insert()\nrescue\nh\nend",
            serde_json::json!({"a":[1]}),
        ),
        (
            "h={a:[1]};begin\nh.a[9]=2\nrescue\nh\nend",
            serde_json::json!({"a":[1]}),
        ),
        (
            "a=[[1],[2]];a[1]=begin\na[0].insert()\nrescue\na[0]\nend;a",
            serde_json::json!([[1], [1]]),
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn a_callers_handler_preserves_class_and_instance_fields() {
    let source = "class C\n@@items=[1]\ndef self.fail\n@@items.insert()\nend\ndef self.go\nbegin\nfail\nrescue\n@@items\nend\nend\nend\nC.go";
    assert_eq!(result(source), serde_json::json!([1]));
    let source = "class C\ngetter items\ndef initialize\n@items=[1]\nend\ndef fail\n@items.insert()\nend\nend\nc=C.new;begin\nc.fail\nrescue\nc.items\nend";
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
        .compile("a=[1];begin\na.insert()\nensure\nrecord(a)\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("insert"), "{error}");
    assert_eq!(*recorded.lock().unwrap(), vec![1]);
}

#[test]
fn failed_block_updates_do_not_publish_partial_results() {
    let source = "a=[1,2,3];begin\na.fill {|i| raise \"stop\" if i==1;9}\nrescue\na\nend";
    assert_eq!(result(source), serde_json::json!([1, 2, 3]));
}

#[test]
fn prior_writes_and_explicit_callback_writes_survive_later_failures() {
    assert_eq!(
        result("a=[1];begin\na.push(2);a.insert()\nrescue\na\nend"),
        serde_json::json!([1, 2])
    );
    assert_eq!(
        result("a=[1,2,3];begin\na.fill {|i| a[2]=7;raise \"stop\" if i==1;9}\nrescue\na\nend"),
        serde_json::json!([1, 2, 7])
    );
    assert_eq!(
        result("s=\"ab\";begin\ns.insert(:x,\"c\")\nrescue\ns\nend"),
        serde_json::json!("ab")
    );
    assert_eq!(
        result("h={a:1};begin\nh.replace([1])\nrescue\nh\nend"),
        serde_json::json!({"a":1})
    );
}
