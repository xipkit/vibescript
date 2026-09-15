use vibescript::{CallOptions, Engine, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn global_mutations_preserve_local_bindings_and_pending_writes() {
    for (body, expected) in [
        (
            "Math.push(input.push(3).length)",
            serde_json::json!([[1, 2], [1, 3]]),
        ),
        (
            "Math.push(Math.push(2).length)",
            serde_json::json!([[1, 2, 2], [1]]),
        ),
        ("Math[0]=9", serde_json::json!([[9], [1]])),
        ("Math[0]+=4", serde_json::json!([[5], [1]])),
        ("Math.send(:push,2)", serde_json::json!([[1, 2], [1]])),
        ("Math.fill {|i| i+4}", serde_json::json!([[4], [1]])),
        (
            "begin;Math.insert();rescue;nil;end",
            serde_json::json!([[1], [1]]),
        ),
        (
            "begin;Math[9]=2;rescue;nil;end",
            serde_json::json!([[1], [1]]),
        ),
        (
            "begin;Math.fill {|i| raise \"stop\"};rescue;nil;end",
            serde_json::json!([[1], [1]]),
        ),
    ] {
        let source = format!("def run(input)\nMath=input\n{body}\n[Math,input]\nend");
        let script = Engine::new().compile(&source).unwrap();
        let input = Value::array(vec![Value::int(1)]);
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(json(&output.value), expected, "{body}");
        assert_eq!(json(&input), serde_json::json!([1]), "{body}");
    }
}

#[test]
fn namespace_global_writes_survive_unwind_and_reset_between_calls() {
    let script = Engine::new()
        .compile(
            r#"
module Reader
 def self.change
  Math[0].push(2)
  Math[0].fill {|i| i+7}
 end
end
def failing
 Reader.change
 raise "stop"
end
def run(input)
 Math=input
 begin
  failing
 rescue
  saved=Math
 ensure
  Math[0].push(9)
 end
 [input,saved,Math]
end
def discard(input)
 run(input)
 nil
end
def read
 Math.PI
end
"#,
        )
        .unwrap();
    let input = Value::array(vec![Value::array(vec![Value::int(1)])]);
    for _ in 0..3 {
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([[[1]], [[7, 8]], [[7, 8, 9]]])
        );
        let output = script
            .call(
                "discard",
                std::slice::from_ref(&input),
                CallOptions::default(),
            )
            .unwrap();
        assert_eq!(output.stats.retained_memory_bytes, 0);
        let output = script.call("read", &[], CallOptions::default()).unwrap();
        assert_eq!(output.value.as_float(), Some(std::f64::consts::PI));
        assert_eq!(output.stats.retained_memory_bytes, 0);
    }
    assert_eq!(json(&input), serde_json::json!([[1]]));
}

#[test]
fn type_annotations_resolve_the_current_global_binding() {
    let script = Engine::new()
        .compile(
            r#"
class A
end
class B
end
def typed(value:Math)->Math
 value
end
def run(first)
 if first
  Math=A
 else
  Math=B
 end
 value=Math.new
 accepted=typed(value).class==Math
 rejected=begin
  typed(7)
  false
 rescue
  true
 end
 [accepted,rejected]
end
"#,
        )
        .unwrap();
    for first in [true, false, true] {
        let output = script
            .call("run", &[Value::boolean(first)], CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), serde_json::json!([true, true]));
    }
}
