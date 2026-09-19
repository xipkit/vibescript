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
fn block_mutations_publish_after_the_last_callback() {
    for (source, expected) in [
        (
            "a=[1,2,3];seen=[];a.fill {|i|seen.push(a);i+7};[seen,a]",
            "[[[1,2,3],[1,2,3],[1,2,3]],[7,8,9]]",
        ),
        (
            "a=[1,2,3];seen=[];a.delete_if {|v|seen.push(a);v==2};[seen,a]",
            "[[[1,2,3],[1,2,3],[1,2,3]],[1,3]]",
        ),
        (
            "a={a:1,b:2};seen=[];a.keep_if {|k,v|seen.push(a);v==2};[seen,a]",
            "[[{\"a\":1,\"b\":2},{\"a\":1,\"b\":2}],{\"b\":2}]",
        ),
    ] {
        let output = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), expected, "{source}");
    }
}

#[test]
fn hash_filter_commits_deletions_without_losing_callback_writes() {
    let output = Engine::new()
        .compile(
            "a={row:{a:1,b:2,c:3}};old=a;\n\
             r=a.row.delete_if {|k,v|a.row.b=9;a.row.d=4;a.row.delete(:c);k==\"a\"};\n\
             [a,old,r]",
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        "[{\"row\":{\"b\":9,\"d\":4}},{\"row\":{\"a\":1,\"b\":2,\"c\":3}},{\"b\":9,\"d\":4}]"
    );
}

#[test]
fn mutable_blocks_preserve_host_inputs_across_calls() {
    let input = Value::array(vec![Value::array(vec![Value::int(1), Value::int(2)])]);
    let script = Engine::new()
        .compile("def run(input)\ninput[0].fill {7}\ninput\nend")
        .unwrap();
    for _ in 0..2 {
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), "[[7,7]]");
        assert_eq!(json(&input), "[[1,2]]");
    }
}

#[test]
fn fill_growth_observes_steps_before_reserving_the_complete_gap() {
    for source in [
        "[1].fill(7,1000000000,0)",
        "[1].fill(1000000000,0) {7}",
        "[1].fill(0,1000000000) {7}",
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: Some(300),
                    memory_bytes: Some(64_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{source}");
    }
}

#[test]
fn staged_fill_results_are_charged_before_later_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("allocate", move |ctx, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        ctx.bytes(&[b'x'; 8192])
    });
    let error = engine
        .compile("[1].fill(0,100) {allocate()}")
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
    assert!(calls.load(Ordering::SeqCst) < 20);
}

#[test]
fn abandoned_mutations_release_addresses_and_staged_results() {
    let mut engine = Engine::new();
    engine.register("allocate", |ctx, _| ctx.bytes(&[b'x'; 8192]));
    for body in [
        "a=[1,2];a.fill {|i|return 7 if i==1;allocate()};7",
        "a=[1,2];a.push(a.fill {|i|break 7 if i==1;allocate()});7",
        "a=[1,2];a.keep_if {allocate();return 7};7",
        "a={a:1,b:2};a.delete_if {|k,v|return 7 if v==2;true};7",
        "a=[];a.push(a.delete(7){allocate();return 7});7",
        "a=[1];a.fill {next allocate()};7",
    ] {
        let source = format!("def work()\n{body}\nend\ndef run()\n200.times {{work()}}\n7\nend");
        let output = engine
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
        assert_eq!(output.value.as_int(), Some(7));
        assert_eq!(output.stats.retained_memory_bytes, 0, "{body}");
    }
}

#[test]
fn excessive_fill_depth_stops_before_another_callback() {
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
        .compile("[1,2].fill {deep()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_in_mutable_blocks_prevents_later_host_effects() {
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
    for source in [
        "[1,2].fill {cancel();effect()}",
        "[1,2].delete_if {cancel();effect()}",
        "{a:1,b:2}.keep_if {cancel();effect()}",
        "[].delete(7) {cancel();effect()}",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled, "{source}");
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}
