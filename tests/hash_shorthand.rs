mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn omitted_values_use_ordinary_lookup_and_implicit_block_parameters() {
    assert_eq!(
        evaluate(
            "def name -> int\n7\nend\ndef with_parameter(value: int) -> { value: int }\n{value:}\nend\nname=3;[name,{name:},with_parameter(4),[5].map{{it:}},[[6,7]].map{{_1:,_2:}}]"
        ),
        serde_json::json!([3, {"name":3}, {"value":4}, [{"it":5}], [{"_1":[6,7],"_2":null}]])
    );
    assert_eq!(
        evaluate("def name -> int\n7\nend\n{name:}"),
        serde_json::json!({"name":7})
    );
    assert_eq!(
        evaluate("a=[1];{a:,changed:a.push(2),nested:{a:}}"),
        serde_json::json!({"a":[1],"changed":[1,2],"nested":{"a":[1,2]}})
    );
}

#[test]
fn labels_allow_physical_newlines_but_require_values_for_quoted_keys() {
    assert_eq!(
        evaluate("a=3;b=4;{a\n:\n# comment\n,b\n:\n,\"c\"\n:\n5}"),
        serde_json::json!({"a":3,"b":4,"c":5})
    );
    for source in [
        "a=1;{\"a\":}",
        "a=1;{\"a\":,b:2}",
        "a=1;{a:;}",
        "a=1;{a;:}",
        "a=1;{a:1;}",
        "a=1;{;a:}",
        "a=1;{a:,;}",
        "a=1;{a:",
    ] {
        assert_eq!(
            Engine::new().compile(source).err().unwrap().kind,
            ErrorKind::Syntax,
            "{source}"
        );
    }
    // An omitted value names a local, function or builtin, so a missing
    // name or a keyword is refused before anything runs.
    for source in ["{missing:}", "{nil:}", "{true:}", "{false:}", "{end:}"] {
        let error = common::static_engine().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0201"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, 1, "{source}");
    }
}

#[test]
fn shorthand_preserves_snapshots_nested_writes_and_host_input_isolation() {
    assert_eq!(
        evaluate("a=[1];h={a:,updated:a.push(2)};h[\"a\"].push(3);[a,h]"),
        serde_json::json!([[1,2],{"a":[1,3],"updated":[1,2]}])
    );
    let script = Engine::new()
        .compile("def run(input: array<int>) -> array<array<int>>\nh={input:};h[\"input\"].push(2);[input,h[\"input\"]]\nend")
        .unwrap();
    let input = Value::array(vec![Value::int(1)]);
    let output = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert_eq!(input.as_array().unwrap().len(), 1);
    let values = output.value.as_array().unwrap();
    assert_eq!(values[0].as_array().unwrap().len(), 1);
    assert_eq!(values[1].as_array().unwrap().len(), 2);
}

#[test]
fn temporary_hash_storage_is_reclaimed_and_failures_stop_before_host_effects() {
    let script = Engine::new()
        .compile("s=\"x\"*4096;i=0;h: hash<string, string | int> = {};while i<500;h={s:,i:};i+=1;end;h.keys")
        .unwrap();
    let output = script
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(16384),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert!(output.stats.peak_memory_bytes < 16384);
    assert!(output.stats.retained_memory_bytes < 1024);

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let mut refusing = common::static_engine();
    refusing.register("mark", |_, _| panic!("mark ran"));
    let error = refusing.compile("{missing:};mark()").err().unwrap();
    assert_eq!(common::codes(&error), ["V0201"]);
    let fields = (0..128)
        .map(|i| format!("field{i}:input"))
        .collect::<Vec<_>>()
        .join(",");
    let script = engine
        .compile(&format!(
            "def run(input: int) -> any\nh={{{fields}}};{{input:,h:}};mark()\nend"
        ))
        .unwrap();
    for kind in [
        ErrorKind::Memory,
        ErrorKind::Steps,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let mut options = CallOptions::default();
        match kind {
            ErrorKind::Memory => options.limits.memory_bytes = Some(4096),
            ErrorKind::Steps => options.limits.steps = Some(128),
            ErrorKind::Cancelled => options.cancellation.cancel(),
            ErrorKind::Deadline => options.deadline = Some(std::time::Instant::now()),
            _ => unreachable!(),
        }
        assert_eq!(
            script
                .call("run", &[Value::int(1)], options)
                .unwrap_err()
                .kind,
            kind
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}
