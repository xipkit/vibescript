use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, Capability, Engine, ErrorClass, ErrorKind, HostMethod, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn effect_engine() -> (Engine, Arc<AtomicUsize>) {
    let mut engine = Engine::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    (engine, effects)
}

#[test]
fn blocks_on_non_iterating_collection_methods_fail_before_any_block_effect() {
    let (engine, effects) = effect_engine();
    for (expression, message) in [
        ("[1].to_s {effect()}", "array.to_s does not take a block"),
        ("[].string {effect()}", "array.string does not take a block"),
        (
            "[1].send(:to_s) {effect()}",
            "array.to_s does not take a block",
        ),
        (
            "[1].public_send(:string) {effect()}",
            "array.string does not take a block",
        ),
        (
            "[1,2,3].clear{effect()}",
            "array.clear does not accept a block",
        ),
        ("[].clear{effect()}", "array.clear does not accept a block"),
        (
            "[1,nil,2].compact{effect()}",
            "array.compact does not accept a block",
        ),
        (
            "[].compact{effect()}",
            "array.compact does not accept a block",
        ),
        (
            "[1,2,3].chunk(2){effect()}",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "[].chunk(2){effect()}",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "[1,2].chunk(*[2]){effect()}",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "[1,2,3].reverse{effect()}",
            "array.reverse does not accept a block",
        ),
        (
            "[].reverse{effect()}",
            "array.reverse does not accept a block",
        ),
        (
            "{a: 1}.clear{effect()}",
            "hash.clear does not accept a block",
        ),
        ("{}.clear{effect()}", "hash.clear does not accept a block"),
        // A plain hash key named clear never overrides the builtin.
        (
            "{clear: 1}.clear{effect()}",
            "hash.clear does not accept a block",
        ),
        // Forwarded spellings reach the same guard.
        (
            "[1].send(:clear){effect()}",
            "array.clear does not accept a block",
        ),
        (
            "[1].public_send(:reverse){effect()}",
            "array.reverse does not accept a block",
        ),
        (
            "[1].send(:compact){effect()}",
            "array.compact does not accept a block",
        ),
        (
            "[1].send(:chunk, 1){effect()}",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "{a: 1}.send(:clear){effect()}",
            "hash.clear does not accept a block",
        ),
        // In-place mutator sites on locals reject before writing back.
        (
            "a=[1]; a.clear{effect()}",
            "array.clear does not accept a block",
        ),
        (
            "h={a: 1}; h.clear{effect()}",
            "hash.clear does not accept a block",
        ),
        (
            "a=[1]; a.send(:clear){effect()}",
            "array.clear does not accept a block",
        ),
    ] {
        let script = engine
            .compile(&format!("{expression};effect()"))
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        let error = script.run(CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{expression}: {error:?}");
        assert_eq!(
            error.class(),
            Some(ErrorClass::Runtime),
            "{expression}: {error:?}"
        );
        assert_eq!(error.message, message, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn argument_and_keyword_errors_take_precedence_over_the_block() {
    let (engine, effects) = effect_engine();
    // Keyword rejection precedes the block check, as in the reference.
    for (expression, message) in [
        (
            "[1].clear(k:1){effect()}",
            "array.clear does not take keyword arguments",
        ),
        (
            "[1].compact(k:1){effect()}",
            "array.compact does not take keyword arguments",
        ),
        (
            "[1].reverse(k:1){effect()}",
            "array.reverse does not take keyword arguments",
        ),
        (
            "{a: 1}.clear(k:1){effect()}",
            "hash.clear does not take keyword arguments",
        ),
        // chunk validates its positional arguments before keywords when a
        // block is attached.
        (
            "[1].chunk(2,k:1){effect()}",
            "array.chunk does not take arguments when a block is supplied",
        ),
    ] {
        let script = engine
            .compile(&format!("{expression};effect()"))
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        let error = script.run(CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{expression}: {error:?}");
        assert_eq!(error.message, message, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
    // Positional-argument errors win over the block message.
    for expression in [
        "[1].clear(1){effect()}",
        "[1].compact(1){effect()}",
        "[1].reverse(1){effect()}",
        "{a: 1}.clear(1){effect()}",
    ] {
        let script = engine
            .compile(&format!("{expression};effect()"))
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        let error = script.run(CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{expression}: {error:?}");
        assert!(
            !error.message.contains("block"),
            "{expression}: {}",
            error.message
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn rejected_block_calls_leave_receivers_untouched_and_are_rescuable() {
    let source = r#"
def run
  a = [1, 2, 3]
  h = {a: 1}
  seen = false
  kinds = []
  begin
    a.clear { seen = true }
  rescue => e
    kinds.push(e.type)
  end
  begin
    a.reverse { seen = true }
  rescue => e
    kinds.push(e.type)
  end
  begin
    a.compact { seen = true }
  rescue => e
    kinds.push(e.type)
  end
  begin
    a.chunk(2) { seen = true }
  rescue => e
    kinds.push(e.type)
  end
  begin
    h.clear { seen = true }
  rescue => e
    kinds.push(e.type)
  end
  [a, h, seen, kinds]
end
"#;
    let script = Engine::new().compile(source).unwrap();
    let outcome = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&outcome.value),
        serde_json::json!([
            [1, 2, 3],
            {"a": 1},
            false,
            ["RuntimeError", "RuntimeError", "RuntimeError", "RuntimeError", "RuntimeError"]
        ])
    );
}

#[test]
fn blockless_forms_still_work() {
    for (source, expected) in [
        ("[1,2,3].reverse", serde_json::json!([3, 2, 1])),
        ("[1,nil,2].compact", serde_json::json!([1, 2])),
        ("[1,2].to_s", serde_json::json!("[1, 2]")),
        ("[].string", serde_json::json!("[]")),
        ("[1,2,3].chunk(2)", serde_json::json!([[1, 2], [3]])),
        ("a=[1,2]; a.clear; a", serde_json::json!([])),
        ("h={a: 1}; h.clear; h", serde_json::json!({})),
    ] {
        let value = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(json(&value), expected, "{source}");
    }
}

fn host_options() -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::new("host", |_| {
            let member = |name: &'static str| {
                HostMethod::new_with_block(name, |call, args, _| call.call_block(args)).value()
            };
            Ok(Value::object(vec![
                (b"clear".to_vec(), member("host.clear")),
                (b"compact".to_vec(), member("host.compact")),
                (b"chunk".to_vec(), member("host.chunk")),
                (b"reverse".to_vec(), member("host.reverse")),
                (b"to_s".to_vec(), member("host.to_s")),
                (b"string".to_vec(), member("host.string")),
            ]))
        })],
        ..CallOptions::default()
    }
}

#[test]
fn callable_object_members_with_collection_names_still_accept_blocks() {
    for body in [
        "host.clear(3) { |n| n + 1 }",
        "host.compact(3) { |n| n + 1 }",
        "host.chunk(3) { |n| n + 1 }",
        "host.reverse(3) { |n| n + 1 }",
        "host.to_s(3) { |n| n + 1 }",
        "host.send(:string, 3) { |n| n + 1 }",
        "host.send(:clear, 3) { |n| n + 1 }",
        "host.public_send(:reverse, 3) { |n| n + 1 }",
        "local = host; local.clear(3) { |n| n + 1 }",
        "local = host; local.reverse(3) { |n| n + 1 }",
    ] {
        let outcome = Engine::new()
            .compile(&format!("def run\n{body}\nend"))
            .unwrap()
            .call("run", &[], host_options())
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(outcome.value.as_int(), Some(4), "{body}");
    }
}
