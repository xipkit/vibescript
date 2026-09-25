//! The block form of `chunk` groups consecutive elements by the key the
//! block returns. The builtin signature table declares only `chunk(size)`,
//! so these programs cannot type check yet; until the block form is typed
//! or removed they check the runtime without static types.
mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, Capability, Engine, ErrorClass, ErrorKind, HostMethod, Limits, Value,
    stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn run(source: &str) -> Value {
    common::gradual_engine()
        .compile(&format!("def run\n{source}\nend"))
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .value
}

fn effect_engine() -> (Engine, Arc<AtomicUsize>) {
    let mut engine = common::gradual_engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    (engine, effects)
}

#[test]
fn chunk_groups_consecutive_equal_keys_in_order() {
    for (source, expected) in [
        ("[].chunk { |n| n }", serde_json::json!([])),
        ("[0].chunk { |n| n }", serde_json::json!([[0, [0]]])),
        (
            "keys=[0, 0, 1]; [0,1,2].chunk { |n| keys[n] }",
            serde_json::json!([[0, [0, 1]], [1, [2]]]),
        ),
        (
            "keys=[0, 1, 0]; [0,1,2].chunk { |n| keys[n] }",
            serde_json::json!([[0, [0]], [1, [1]], [0, [2]]]),
        ),
        (
            "keys=[0, 0, 1, 1]; [0,1,2,3].chunk { |n| keys[n] }",
            serde_json::json!([[0, [0, 1]], [1, [2, 3]]]),
        ),
        (
            "[1,1,2].chunk { |n| n }",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
        // Keys may be any ordinary value: booleans, arrays and hashes compare
        // structurally.
        (
            "keys=[true,true,false,false,true]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([[true, [0, 1]], [false, [2, 3]], [true, [4]]]),
        ),
        (
            "keys=[[1],[1],[2],[2],[1]]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([[[1], [0, 1]], [[2], [2, 3]], [[1], [4]]]),
        ),
        (
            "keys=[{k:1},{k:1},{k:2},{k:2},{k:1}]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([[{"k": 1}, [0, 1]], [{"k": 2}, [2, 3]], [{"k": 1}, [4]]]),
        ),
        // Strings beginning with an underscore are ordinary keys.
        (
            "keys=[\"_separator\",\"_separator\",\"_alone\",\"_alone\",\"_bad\"]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([["_separator", [0, 1]], ["_alone", [2, 3]], ["_bad", [4]]]),
        ),
        // Forwarded and expanded spellings reach the same overload.
        (
            "a=[1,1,2]; a.send(:chunk) { |n| n }",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
        (
            "a=[1,1,2]; a.public_send(:chunk) { |n| n }",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
        (
            "a=[1,1,2]; a.chunk(*[]) { |n| n }",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
        (
            "a=[1,1,2]; a.chunk(**{}) { |n| n }",
            serde_json::json!([[1, [1, 1]], [2, [2]]]),
        ),
        // The receiver is untouched.
        (
            "keys=[0, 0]; a=[0, 1]; result=a.chunk { |n| keys[n] }; [result,a]",
            serde_json::json!([[[0, [0, 1]]], [0, 1]]),
        ),
    ] {
        assert_eq!(json(&run(source)), expected, "{source}");
    }
}

#[test]
fn nil_and_control_symbols_split_groups() {
    for (source, expected) in [
        (
            "keys=[1,nil,1,1,nil]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([[1, [0]], [1, [2, 3]]]),
        ),
        (
            "keys=[:a,:_separator,:a,:a,:_separator]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([["a", [0]], ["a", [2, 3]]]),
        ),
        (
            "keys=[:a,:_alone,:a,:_alone,:_alone]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([
                ["a", [0]],
                ["_alone", [1]],
                ["a", [2]],
                ["_alone", [3]],
                ["_alone", [4]]
            ]),
        ),
        (
            "keys=[nil,nil,nil,nil,nil]; [0,1,2,3,4].chunk { |n| keys[n] }",
            serde_json::json!([]),
        ),
        // A separator after an alone row leaves nothing pending.
        (
            "keys=[:_alone,nil,:b]; [0,1,2].chunk { |n| keys[n] }",
            serde_json::json!([["_alone", [0]], ["b", [2]]]),
        ),
    ] {
        assert_eq!(json(&run(source)), expected, "{source}");
    }
}

#[test]
fn reserved_symbol_keys_are_runtime_errors() {
    for (source, message) in [
        ("[1].chunk { :_bad }", "array.chunk reserved key :_bad"),
        ("[1].chunk { :_ }", "array.chunk reserved key :_"),
        ("[1].chunk { :__x }", "array.chunk reserved key :__x"),
    ] {
        let error = common::gradual_engine()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}: {error:?}");
        assert_eq!(error.class(), Some(ErrorClass::Runtime), "{source}");
        assert_eq!(error.message, message, "{source}");
    }
    // Rescuable, and earlier rows are discarded with the interrupted run.
    let value = run(
        "begin; [1,2].chunk { |n| if n == 1; :a; else; :_bad; end }; rescue => e; [e.type, e.message]; end",
    );
    assert_eq!(
        json(&value),
        serde_json::json!(["RuntimeError", "array.chunk reserved key :_bad"])
    );
}

#[test]
fn arguments_and_keywords_are_rejected_before_the_block_runs() {
    let (engine, effects) = effect_engine();
    for (expression, message) in [
        (
            "[1].chunk(2) { effect() }",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "[1].chunk(2, k: 1) { effect() }",
            "array.chunk does not take arguments when a block is supplied",
        ),
        (
            "[1].chunk(k: 1) { effect() }",
            "array.chunk does not take keyword arguments",
        ),
        (
            "[].chunk(k: 1) { effect() }",
            "array.chunk does not take keyword arguments",
        ),
        (
            "[1].send(:chunk, 2) { effect() }",
            "array.chunk does not take arguments when a block is supplied",
        ),
    ] {
        let script = engine
            .compile(&format!("{expression};effect()"))
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        let error = script.run(CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{expression}: {error:?}");
        assert_eq!(error.class(), Some(ErrorClass::Runtime), "{expression}");
        assert_eq!(error.message, message, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
    // The sized form is unchanged.
    assert_eq!(
        json(&run("[1,2,3].chunk(2)")),
        serde_json::json!([[1, 2], [3]])
    );
    let error = common::gradual_engine()
        .compile("[1,2,3].chunk")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
}

#[test]
fn empty_receivers_never_run_the_block() {
    let (engine, effects) = effect_engine();
    let value = engine
        .compile("[].chunk { effect(); raise \"should not run\" }")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(json(&value), serde_json::json!([]));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    // A non-empty receiver runs the block once per item, including nil items.
    assert_eq!(
        json(&run("hit=0; [1,nil,2].chunk { hit+=1; 9 }; hit")),
        serde_json::json!(3)
    );
}

#[test]
fn block_control_flow_follows_collection_rules() {
    for (source, expected) in [
        (
            "[1,2,3].chunk { |n| break 7 if n==2; n }",
            serde_json::json!(7),
        ),
        (
            "[1,2,3].chunk { |n| next :even if n%2==0; :odd }",
            serde_json::json!([["odd", [1]], ["even", [2]], ["odd", [3]]]),
        ),
        (
            "[1,2,3].chunk { |n| return 9 if n==2; n }; 4",
            serde_json::json!(9),
        ),
    ] {
        assert_eq!(json(&run(source)), expected, "{source}");
    }
}

#[test]
fn results_and_keys_do_not_alias_later_mutations() {
    for (source, expected) in [
        (
            "a=[[1],[1]]; groups=a.chunk { |v| :same }; groups[0][1][0].push(2); [groups,a]",
            serde_json::json!([[["same", [[1, 2], [1]]]], [[1], [1]]]),
        ),
        (
            "key=[1]; groups=[1,2].chunk { key }; key.push(2); groups",
            serde_json::json!([[[1], [1, 2]]]),
        ),
        // The block sees the receiver snapshot even if it reassigns the source.
        (
            "a=[1,1,2]; seen=[]; r=a.chunk { |n| seen.push(a.length); a=[]; n }; [r,seen,a]",
            serde_json::json!([[[1, [1, 1]], [2, [2]]], [3, 0, 0], []]),
        ),
    ] {
        assert_eq!(json(&run(source)), expected, "{source}");
    }
}

#[test]
fn errors_inside_the_block_are_rescuable_but_cancellation_is_not() {
    let value =
        run("begin; [1,2].chunk { |n| raise \"boom\" if n==2; n }; rescue => e; e.message; end");
    assert_eq!(json(&value), serde_json::json!("boom"));
    let mut engine = common::gradual_engine();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    let error = engine
        .compile("begin; [1,2,3].chunk { |n| cancel(); n }; rescue; 1; end")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    let error = common::gradual_engine()
        .compile("begin; [1,2,3].chunk { |n| n }; rescue; 1; end")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                steps: Some(5),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}

#[test]
fn excessive_row_depth_stops_before_another_callback() {
    let mut value = Value::int(1);
    for _ in 0..10_000 {
        value = Value::array(vec![value]);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = common::gradual_engine();
    engine.register("deep", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(value.clone())
    });
    let error = engine
        .compile("[1,2].chunk {deep()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn chunk_checks_the_complete_result_depth_before_another_callback() {
    for (key_depth, item_depth, succeeds) in [
        (9998, 0, true),
        (9999, 0, false),
        (0, 9997, true),
        (0, 9998, false),
    ] {
        let nest = |depth| {
            let mut value = Value::int(1);
            for _ in 0..depth {
                value = Value::array(vec![value]);
            }
            value
        };
        let key = nest(key_depth);
        let item = nest(item_depth);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = common::gradual_engine();
        engine.register("items", move |_, _| {
            Ok(Value::array(vec![item.clone(), item.clone()]))
        });
        engine.register("key", move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(key.clone())
        });
        let result = engine
            .compile("items().chunk { key() }")
            .unwrap()
            .run(CallOptions::default());
        if succeeds {
            assert!(
                result.is_ok(),
                "key depth {key_depth}, item depth {item_depth}: {result:?}"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        } else {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Recursion);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "key depth {key_depth}, item depth {item_depth}"
            );
        }
    }
}

#[test]
fn pending_groups_and_rows_are_accounted_and_released() {
    let mut engine = common::gradual_engine();
    engine.register("allocate", |ctx, _| ctx.bytes(&[b'x'; 8192]));
    // Each row retains an 8 KiB key; the memory limit stops the run.
    let error = engine
        .compile("(1..1000).to_a.chunk { |n| allocate() + n.to_s }")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(96_000),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    // Discarded results, breaks and rescued errors release the partial state.
    for body in [
        "[1,2,3].chunk { |n| allocate(); n }",
        "[1,2,3].chunk { |n| allocate(); break if n==2; n }",
        "begin; [1,2,3].chunk { |n| allocate(); raise \"x\" if n==2; n }; rescue; nil; end",
        "begin; [1,2,3].chunk { |n| allocate(); if n==2; :_bad; else; n; end }; rescue; nil; end",
    ] {
        let source = format!("def work()\n{body}\nend\ndef run()\n200.times {{work()}}\n7\nend");
        let result = engine
            .compile(&source)
            .unwrap()
            .call(
                "run",
                &[],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(96_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.as_int(), Some(7), "{body}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
        assert!(result.stats.peak_memory_bytes < 96_000, "{body}");
    }
}

fn host() -> Capability {
    Capability::from_value(
        "host",
        Value::object(vec![(
            b"chunk".to_vec(),
            HostMethod::new_with_block("host.chunk", |call, args, _| call.call_block(args)).value(),
        )]),
    )
}

#[test]
fn callable_host_members_named_chunk_still_take_blocks() {
    let mut engine = Engine::new();
    engine.declare_capability(&host()).unwrap();
    for body in ["host.chunk(3) { |n| n.as(int) + 1 }", "host.chunk { 3 }"] {
        let options = CallOptions {
            capabilities: vec![host()],
            ..CallOptions::default()
        };
        let outcome = engine
            .compile(&format!("def run -> any\n{body}\nend"))
            .unwrap_or_else(|error| panic!("{body}: {error}"))
            .call("run", &[], options)
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        let expected = if body.contains("(3)") { 4 } else { 3 };
        assert_eq!(outcome.value.as_int(), Some(expected), "{body}");
    }
}
