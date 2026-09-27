use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> String {
    String::from_utf8(
        stringify_json(value, CallOptions::default())
            .unwrap()
            .value
            .as_bytes()
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn hash_yield_shapes_and_iteration_order_are_explicit() {
    let script = Engine::new()
        .compile(
            "h={b:2,a:1}\n\
             [h.map {_1}, h.map {_2}, h.map {|(key,value)|[key,value]},\n\
              h.map_with_index {|(key,value),index|[key,value,index]},\n\
              h.select {|key|key==\"a\"}]",
        )
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&result.value),
        "[[[\"b\",2],[\"a\",1]],[2,1],[[\"b\",2],[\"a\",1]],[[\"b\",2,0],[\"a\",1,1]],{\"a\":1}]"
    );
}

#[test]
fn sparse_numeric_iteration_reaches_integer_boundaries_in_bounded_work() {
    for (source, expected) in [
        (
            "a: array<int> = [];(-9223372036854775808..9223372036854775807).step(9223372036854775807) {|n|a.push(n)};a",
            "[-9223372036854775808,-1,9223372036854775806]",
        ),
        (
            "a: array<int> = [];9223372036854775807.step(-9223372036854775808,-9223372036854775808) {|n|a.push(n)};a",
            "[9223372036854775807,-1]",
        ),
        (
            "(-9223372036854775808..-9223372036854775808).map {|n|n}",
            "[-9223372036854775808]",
        ),
    ] {
        let result = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: Some(300),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        assert_eq!(json(&result.value), expected);
        assert!(result.stats.steps < 300);
    }
}

#[test]
fn retained_block_results_count_against_later_allocations() {
    for expression in [
        "(1..100).map {allocate()}",
        "(1..100).to_a.filter_map {allocate()}",
        "(1..100).to_a.to_h {|n|[n.to_s,allocate()]}",
        "(1..100).to_a.map_with_index {allocate()}",
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = Engine::new();
        engine.register("allocate", move |ctx, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            ctx.bytes(&[b'x'; 8192])
        });
        let error = engine
            .compile(expression)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(96_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory, "{expression}");
        assert!(calls.load(Ordering::SeqCst) < 20, "{expression}");
    }
}

#[test]
fn discarded_results_and_nonlocal_exits_release_iteration_roots() {
    let mut engine = Engine::new();
    engine.register("allocate", |ctx, _| ctx.bytes(&[b'x'; 8192]));
    for body in [
        "[1].each {allocate()};7",
        "[1,2].map {allocate();return 7}",
        "[1,2].reduce(allocate()) {return 7}",
        "[1,2].partition {allocate();break 7}",
        "{a:1}.transform_values {allocate();return 7}",
        "[1].map {next allocate()};7",
        "[1,2].each { [3,4].each { [5,6].map {allocate();return 7} } };7",
        "[1,2].each { [3,4].map {allocate();break 7} };7",
        "[1,2].each { begin; [3,4].map {allocate();raise \"stop\"}; rescue; nil; end };7",
    ] {
        let source =
            format!("def work() -> any\n{body}\nend\ndef run() -> int\n200.times {{work}}\n7\nend");
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
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
        assert!(result.stats.peak_memory_bytes < 96_000, "{body}");
    }
}

#[test]
fn cycles_observe_limits_and_cancellation_before_later_host_effects() {
    let mut engine = Engine::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    let error = engine
        .compile("[1].cycle {}\neffect()")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                steps: Some(300),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    let error = engine
        .compile("[1,2].map {cancel();effect()}\neffect()")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn nested_builtin_blocks_use_the_vm_recursion_limit() {
    let script = Engine::new()
        .compile("def recurse() -> array<any>\n[1].map {recurse}\nend")
        .unwrap();
    let error = script
        .call(
            "recurse",
            &[],
            CallOptions {
                limits: Limits {
                    recursion: 16,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
}

#[test]
fn an_unrepresentable_output_stops_before_the_next_block() {
    let mut value = Value::int(1);
    for _ in 0..10_000 {
        value = Value::array(vec![value]);
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("deep", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(value.clone())
    });
    let error = engine
        .compile("[1,2].map {deep()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn flat_map_checks_unflattened_hash_results_before_the_next_block() {
    let mut value = Value::int(1);
    for _ in 0..9_999 {
        value = Value::array(vec![value]);
    }
    let value = Value::hash(vec![(b"deep".to_vec(), value)]);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("deep", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(value.clone())
    });
    let error = engine
        .compile("[1,2].flat_map {deep()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn grouping_checks_every_result_wrapper_before_another_block() {
    for (method, depth, key) in [
        ("partition", 9_999, "true"),
        ("group_by", 9_999, ":key"),
        ("group_by_stable", 9_998, ":key"),
    ] {
        let mut value = Value::int(1);
        for _ in 0..depth {
            value = Value::array(vec![value]);
        }
        let input = Value::array(vec![value, Value::int(2)]);
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let mut engine = Engine::new();
        engine.register("entered", move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        let source =
            format!("def run(input: array<any>) -> any\ninput.{method} {{entered();{key}}}\nend");
        let error = engine
            .compile(&source)
            .unwrap()
            .call("run", &[input], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{method}");
    }
}
