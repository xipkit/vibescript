mod common;

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
            "a=[1,2,3];seen: array<array<int>> = [];a.delete_if {|v|seen.push(a);v==2};[seen,a]",
            "[[[1,2,3],[1,2,3],[1,2,3]],[1,3]]",
        ),
        (
            "a: hash<string, int> = {a:1,b:2};seen: array<hash<string, int>> = [];\
             a.keep_if {|k,v|seen.push(a);v==2};[seen,a]",
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
    // `fill` takes no block now.
    let source = "a=[1,2,3];seen: array<array<int>> = [];a.fill {|i|seen.push(a);i+7};[seen,a]";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0301", "V0305"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("fill").unwrap()
    );
}

#[test]
fn hash_filter_commits_deletions_without_losing_callback_writes() {
    let output = Engine::new()
        .compile(
            "a: { row: hash<string, int> } = {row:{a:1,b:2,c:3}};old=a;\n\
             r=a[\"row\"].delete_if {|k,v|a[\"row\"][\"b\"]=9;a[\"row\"][\"d\"]=4;a[\"row\"].delete(\"c\");k==\"a\"};\n\
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
        .compile(
            "def run(input: [array<int>]) -> [array<int>]\ninput[0].delete_if {|v| v == 1}\ninput\nend",
        )
        .unwrap();
    for _ in 0..2 {
        let output = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), "[[2]]");
        assert_eq!(json(&input), "[[1,2]]");
    }
}

#[test]
fn fill_past_the_end_raises_before_reserving_a_gap() {
    // Filling past the end raises instead of padding the gap with nil
    // (ADR-008), so nothing is reserved or counted for the gap.
    let error = Engine::new()
        .compile("[1].fill(7,1000000000,0)")
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
    assert_eq!(error.kind, ErrorKind::Argument);
    assert_eq!(
        error.message,
        "array.fill window 1000000000...1000000000 is past the end of the array (length 1)"
    );
    // `fill` takes no block now.
    for source in ["[1].fill(1000000000,0) {7}", "[1].fill(0,1000000000) {7}"] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0305"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, source.find('{').unwrap());
    }
}

#[test]
fn staged_fill_results_are_charged_before_later_callbacks() {
    // `fill` takes no block now, so nothing stages its results.
    let mut engine = vibescript::Engine::new();
    engine.register("allocate", |_, _| panic!("allocate ran"));
    let source = "[1].fill(0,100) {allocate()}";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0305"]);
    assert_eq!(error.diagnostics()[0].span.start, source.find('{').unwrap());
}

#[test]
fn abandoned_mutations_release_addresses_and_staged_results() {
    let mut engine = Engine::new();
    engine.register("allocate", |ctx, _| ctx.bytes(&[b'x'; 8192]));
    for body in [
        "a=[1,2];a.keep_if {allocate();return 7};7",
        "a: hash<string, int> = {a:1,b:2};a.delete_if {|k,v|return 7 if v==2;true};7",
        "a: array<int?> = [];a.push(a.delete(7){allocate();return 7});7",
    ] {
        let source =
            format!("def work -> int\n{body}\nend\ndef run -> int\n200.times {{work}}\n7\nend");
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
    // `fill` takes no block now.
    let mut checked = vibescript::Engine::new();
    checked.register("allocate", |_, _| panic!("allocate ran"));
    for body in [
        "a=[1,2];a.fill {|i|return 7 if i==1;allocate()};7",
        "a=[1];a.fill {next allocate()};7",
    ] {
        let source = format!("def work -> int\n{body}\nend");
        let error = checked.compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0301", "V0305"], "{body}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find("fill").unwrap()
        );
    }
}

#[test]
fn excessive_fill_depth_stops_before_another_callback() {
    // `fill` takes no block now, so no callback can stage a deep value.
    let mut engine = vibescript::Engine::new();
    engine.register("deep", |_, _| panic!("deep ran"));
    let source = "[1,2].fill {deep()}";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0301", "V0305"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("fill").unwrap()
    );
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
        "[1,2].delete_if {cancel();effect().as(bool)}",
        "{a:1,b:2}.keep_if {cancel();effect().as(bool)}",
        "[].delete(7) {cancel();effect();7}",
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
