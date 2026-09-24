use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn invalid_call_members_stop_before_arguments_and_blocks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, args| {
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(args[0].clone())
    });
    let prefix = "class P\nprivate\ndef call(value=7)\nvalue\nend\nend\n";
    for receiver in [
        "{}",
        "[]",
        "7",
        "P.new",
        "Math[\"sqrt\"]",
        "Regexp[\"last_match\"]",
    ] {
        for suffix in [
            ".call(mark(1))",
            "&.call(mark(1))",
            ".call(*[mark(1)])",
            ".call(flag:mark(1)){mark(2)}",
            ".call{mark(1)}",
        ] {
            events.lock().unwrap().clear();
            let source = format!("{prefix}\n{receiver}{suffix}");
            engine
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert!(events.lock().unwrap().is_empty(), "{source}");
        }
    }
    let output = engine
        .compile("nil&.call(mark(1))")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(json(&output.value).is_null());
    assert!(events.lock().unwrap().is_empty());
    engine
        .compile("{call:7}.call(mark(1))")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(*events.lock().unwrap(), [1]);
}

#[test]
fn valid_call_methods_preserve_binding_blocks_and_control_flow() {
    let script = Engine::new().compile(r#"
class C
 def call(value=7,flag:0)
  out=[value,flag]
  if block_given?
   out.push(yield(value))
  end
  out
 end
end
class D
 def call(options={})
  options
 end
end
def escape
 C.new.call{return 13}
 99
end
def run
 c=C.new
 [c.call(2,flag:3){|v|v+1},c.call{break 12},escape(),c.call{|v|next v+1},D.new.call({flag:3}),D.new.send(:call,flag:3)]
end
"#).unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([[2,3,3],12,13,[7,0,8],{"flag":3},{"flag":3}])
    );
    let error = Engine::new()
        .compile("class D\ndef call(options={})\noptions\nend\nend\nD.new.call(flag:3)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
}

#[test]
fn call_targets_are_selected_before_arguments_mutate_callable_fields() {
    let result = Engine::new()
        .compile("h={call:Math::sqrt};value=h.call(h.clear.length);[value,h]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&result.value), serde_json::json!([0, {}]));
}

#[test]
fn rejected_call_arguments_release_storage_and_cancellation_still_wins() {
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
    let script = engine.compile("def reject\nbegin\n{}.call(mark(1))\nrescue RuntimeError\nnil\nend\nend\ndef run(n)\nfor i in 1..n\nreject()\nend\nnil\nend").unwrap();
    let small = script
        .call("run", &[Value::int(32)], CallOptions::default())
        .unwrap();
    let large = script
        .call("run", &[Value::int(256)], CallOptions::default())
        .unwrap();
    assert_eq!(small.stats.peak_memory_bytes, large.stats.peak_memory_bytes);
    assert_eq!(large.stats.retained_memory_bytes, 0);
    assert!(events.lock().unwrap().is_empty());
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(small.stats.peak_memory_bytes - 1),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call("run", &[Value::int(32)], options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let fresh = script
        .call("run", &[Value::int(32)], CallOptions::default())
        .unwrap();
    assert_eq!(fresh.stats.peak_memory_bytes, small.stats.peak_memory_bytes);
    assert_eq!(fresh.stats.retained_memory_bytes, 0);
    let cancelled = engine.compile("class C\ndef call(value=nil)\nyield\nend\nend\nbegin\nC.new.call(cancel()){mark(2)}\nrescue RuntimeError\nmark(3)\nensure\nmark(4)\nend").unwrap().run(CallOptions::default()).unwrap_err();
    assert_eq!(cancelled.kind, ErrorKind::Cancelled);
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn unknown_call_members_use_the_reference_wording() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("language-errors.json")).unwrap();
    let mut checked = 0;
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if !name.starts_with("call_member_unknown_") {
            continue;
        }
        let source = case["source"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("def run(input)\n{}\nend", case["body"].as_str().unwrap()));
        let error = Engine::new()
            .compile(&source)
            .unwrap()
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, case["go_error"].as_str().unwrap(), "{name}");
        checked += 1;
    }
    assert_eq!(checked, 15);
}
