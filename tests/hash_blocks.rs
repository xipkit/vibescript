use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let result = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(result.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn merge_folds_conflicts_in_argument_order_and_keeps_key_positions() {
    let result = Engine::new()
        .compile(
            "h={b:nil,a:1};seen=[];\n\
         r=h.merge({b:2,a:3,c:4},{a:5}) {|key,old,new|seen.push([key,old,new]);new};\n\
         [h,r,r.keys,seen]",
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&result.value),
        serde_json::json!([
            {"b":null,"a":1}, {"b":2,"a":5,"c":4}, ["b","a","c"],
            [["b",null,2],["a",1,3],["a",3,5]]
        ])
    );
}

#[test]
fn deep_keys_visit_each_occurrence_in_preorder_even_when_keys_collide() {
    for (source, expected) in [
        (
            "n=0;seen=[];child={a:1};h={x:child,y:[child]};\n\
             r=h.deep_transform_keys {|k|n+=1;seen.push(k);k+n.to_s};[r,seen]",
            serde_json::json!([{"x1":{"a2":1},"y3":[{"a4":1}]},["x","a","y","a"]]),
        ),
        (
            "seen=[];r={a:{b:1},c:{d:2}}.deep_transform_keys {|k|seen.push(k);:x};[r,seen]",
            serde_json::json!([{"x":{"x":2}},["a","b","c","d"]]),
        ),
    ] {
        let result = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(json(&result.value), expected, "{source}");
    }
}

#[test]
fn invalid_merge_arguments_and_keys_stop_before_later_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    for source in [
        "{a:1}.merge({a:2},9){effect()}",
        "{a:1}.merge({a:2},bad:9){effect()}",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let error = engine
        .compile("{a:{b:1},c:2}.deep_transform_keys {effect()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn retained_conflict_values_and_parent_keys_count_against_later_allocations() {
    for source in [
        "h=(1..100).map {|n|[n.to_s,n]}.to_h;h.merge(h){allocate()}",
        "h=(1..100).map {|n|[n.to_s,n]}.to_h;h.deep_transform_keys {allocate()}",
        "h={leaf:1};60.times {h={node:h}};h.deep_transform_keys {allocate()}",
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = Engine::new();
        engine.register("allocate", move |ctx, _| {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            let mut bytes = [b'x'; 8192];
            bytes[..size_of::<usize>()].copy_from_slice(&n.to_le_bytes());
            ctx.bytes(&bytes)
        });
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(96_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory, "{source}");
        let calls = calls.load(Ordering::SeqCst);
        assert!(calls > 0 && calls < 20, "{source}: {calls} allocations");
    }
}

#[test]
fn overwritten_conflict_results_are_released_between_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("allocate", move |ctx, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        ctx.bytes(&[b'x'; 8192])
    });
    let result = engine
        .compile("h={a:0};others=(1..100).map {|n|{a:n}};h.merge(*others){allocate()}.length")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(256_000),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    assert_eq!(calls.load(Ordering::SeqCst), 100);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn deep_shared_graph_expansion_observes_the_work_limit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("key", move |_, args| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(args[0].clone())
    });
    let error = engine.compile(
        "child={leaf:1};30.times {child=[child,child]};{root:child}.deep_transform_keys {|k|key(k)}"
    ).unwrap().run(CallOptions {
        limits: Limits {steps: Some(5000), memory_bytes: Some(1 << 20), ..Limits::default()},
        ..CallOptions::default()
    }).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    assert!(calls.load(Ordering::SeqCst) > 1);
}

#[test]
fn adjacent_group_depth_is_rejected_before_another_callback() {
    let mut deep = Value::int(0);
    for _ in 0..9_999 {
        deep = Value::array(vec![deep]);
    }
    let input = Value::array(vec![deep, Value::int(1), Value::int(2)]);
    for method in ["slice_when", "chunk_while"] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = Engine::new();
        engine.register("split", move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Value::boolean(method == "slice_when"))
        });
        let error = engine
            .compile(&format!("def run(input)\ninput.{method} {{split()}}\nend"))
            .unwrap()
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion, "{method}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn nested_key_walks_use_accounted_frames_without_consuming_script_recursion() {
    let mut input = Value::int(7);
    for _ in 0..10_000 {
        input = Value::hash(vec![(b"key".to_vec(), input)]);
    }
    let result = Engine::new()
        .compile("def run(input)\ninput.deep_transform_keys {|k|k};7\nend")
        .unwrap()
        .call(
            "run",
            &[input],
            CallOptions {
                limits: Limits {
                    recursion: 8,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn transformed_hashes_preserve_host_inputs_across_calls() {
    let input = Value::hash(vec![(b"a".to_vec(), Value::array(vec![Value::int(1)]))]);
    let script = Engine::new().compile(
        "def run(input)\nmerged=input.merge({new:[2]});deep=merged.deep_transform_keys {|k|k.upcase};\n\
         deep[\"A\"].push(7);[input,merged,deep]\nend"
    ).unwrap();
    for _ in 0..2 {
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(
            json(&result.value),
            serde_json::json!([
                {"a":[1]}, {"a":[1],"new":[2]}, {"A":[1,7],"NEW":[2]}
            ])
        );
        assert_eq!(json(&input), serde_json::json!({"a":[1]}));
    }
}

#[test]
fn abandoned_collection_drivers_release_outputs_keys_and_pending_receivers() {
    let mut engine = Engine::new();
    engine.register("allocate", |ctx, _| ctx.bytes(&[b'x'; 8192]));
    for body in [
        "{a:1,b:2}.merge({a:3,b:4}) {|k,o,n|return 7 if k==\"b\";allocate()}",
        "a=[];a.push({a:1,b:2}.merge({a:3,b:4}) {|k,o,n|break 7 if k==\"b\";allocate()});7",
        "{outer:{nested:1}}.deep_transform_keys {|k|return 7 if k==\"nested\";allocate()}",
        "a=[];a.push({outer:{nested:1}}.deep_transform_keys {|k|break 7 if k==\"nested\";allocate()});7",
        "a=[];a.push([1,2,3].slice_when {|a,b|break 7 if b==3;allocate()});7",
        "[1,2,3].chunk_while {|a,b|return 7 if b==3;false}",
    ] {
        let result = engine
            .compile(&format!(
                "def work()\n{body}\nend\ndef run()\n200.times {{work()}}\n7\nend"
            ))
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
            .unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
    }
}

#[test]
fn cancellation_in_collection_blocks_stops_later_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    for call in [
        "[1,2].slice_when",
        "[1,2].chunk_while",
        "{a:1}.merge({a:2})",
        "{a:1}.deep_transform_keys",
    ] {
        for block in ["cancel()", "cancel();effect()"] {
            let source = format!("{call} {{{block}}};effect()");
            let error = engine
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Cancelled, "{source}");
            assert_eq!(effects.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn hash_flatten_preserves_valid_depth_without_constructing_temporary_pairs() {
    let mut nested = Value::int(7);
    for _ in 0..9_999 {
        nested = Value::array(vec![nested]);
    }
    let input = Value::hash(vec![(b"key".to_vec(), nested)]);
    for (expression, expected) in [("input.flatten.length", 2), ("input.flatten(-1)[1]", 7)] {
        let result = Engine::new()
            .compile(&format!("def run(input)\n{expression}\nend"))
            .unwrap()
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(expected), "{expression}");
    }
    let error = Engine::new()
        .compile("def run(input)\ninput.flatten(0)\nend")
        .unwrap()
        .call("run", &[input], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
}

#[test]
fn hash_flatten_stops_shared_graph_expansion_at_the_step_limit() {
    let mut value = Value::int(7);
    for _ in 0..30 {
        value = Value::array(vec![value.clone(), value]);
    }
    let input = Value::hash(vec![(b"key".to_vec(), value)]);
    let error = Engine::new()
        .compile("def run(input)\ninput.flatten(-1)\nend")
        .unwrap()
        .call(
            "run",
            &[input],
            CallOptions {
                limits: Limits {
                    steps: Some(5000),
                    memory_bytes: Some(1 << 20),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}
