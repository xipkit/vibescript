mod common;

use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value};

#[test]
fn checker_tracks_offsets_and_does_not_drop_possible_misses() {
    for (expression, expected) in [
        ("[1,2,1].index(1,1)", "2"),
        ("[1,2,1].find_index(1,1.9)", "2"),
        ("[1,2,1].rindex(1,1)", "0"),
        ("[1,2,1].index(1,-0.5)", "0"),
        ("[1,2,1].index(1,9223372036854775807)", "nil"),
        ("[1,2,1].rindex(1,9223372036854775807)", "2"),
        ("[].rindex(1,0)", "nil"),
        ("[1,2,1].send(:index,1,1)", "2"),
        ("[1,2,1].public_send(:rindex,1,1)", "0"),
    ] {
        let source =
            format!("def run -> int; if {expression} == {expected}; 7; else; 'wrong'; end; end");
        let script = common::gradual_engine().compile(&source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
    }
    for method in ["index", "find_index", "rindex"] {
        for offset in ["-1", "-1.5", "'bad'", "true", "nil"] {
            let source = format!("def run; [].{method}(1,{offset}); end");
            let script = common::gradual_engine().compile(&source).unwrap();
            let report = script
                .check_call("run", &[], &CallOptions::default())
                .unwrap();
            assert!(!report.is_clean(), "{source}: {report:?}");
            assert_eq!(
                script
                    .call("run", &[], CallOptions::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::Argument,
                "{source}"
            );
        }
    }
    let source = "def run(offset:int) -> int; [1,1].index(1,offset); end";
    let script = common::gradual_engine().compile(source).unwrap();
    let report = script.check(&CallOptions::default()).unwrap();
    assert!(!report.is_clean(), "a dynamic offset can miss: {report:?}");
}

#[test]
fn searches_skip_ineligible_elements_and_stop_at_the_first_match() {
    let observed = Arc::new(AtomicU64::new(0));
    let checkpoint = observed.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |ctx, _| {
        checkpoint.store(ctx.stats().steps, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let run = |expression: &str| {
        let result = engine
            .compile(&format!("a=(1..1000).to_a;mark();{expression}"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.stats.retained_memory_bytes, 0);
        (
            result.stats.steps - observed.load(Ordering::SeqCst),
            result.value,
        )
    };
    let (full, _) = run("a.index(1000)");
    for expression in ["a.index(1000,999)", "a.rindex(1000)"] {
        let (short, value) = run(expression);
        assert_eq!(value.as_int(), Some(999), "{expression}");
        assert!(full >= short + 990, "{expression}: {full} versus {short}");
    }
    let (reverse, _) = run("a.rindex(1)");
    let (short, value) = run("a.rindex(1,0)");
    assert_eq!(value.as_int(), Some(0));
    assert!(reverse >= short + 990);
}

#[test]
fn composite_comparisons_consume_quota_and_cannot_be_rescued() {
    let checkpoint = Arc::new(AtomicU64::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let mark = checkpoint.clone();
    engine.register("mark", move |ctx, _| {
        mark.store(ctx.stats().steps, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let seen = effects.clone();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for method in ["index", "rindex"] {
        let source = format!(
            "a=(1..1000).to_a;b=a;b[999]=0;mark();begin;[a].{method}(b,0);rescue;effect();end;effect()"
        );
        let script = engine.compile(&source).unwrap();
        script.run(CallOptions::default()).unwrap();
        let steps = checkpoint.load(Ordering::SeqCst) + 100;
        effects.store(0, Ordering::SeqCst);
        let error = script
            .run(CallOptions {
                limits: Limits {
                    steps: Some(steps),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{method}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{method}");
    }
}

#[test]
fn include_string_searches_and_block_calls_keep_their_contracts() {
    let result = Engine::new().compile(
        "[[1,2,1].index{|n|n==1},[1,2,1].rindex{|n|n==1},'aba'.index('a',1),'aba'.rindex('a',1)]"
    ).unwrap().run(CallOptions::default()).unwrap();
    let values: Vec<_> = result
        .value
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_int)
        .collect();
    assert_eq!(values, [Some(0), Some(2), Some(2), Some(0)]);
    // No signature takes an offset beside a block, or `include?` an offset.
    for source in [
        "[1].include?(1,0)",
        "[1].index(1,0){true}",
        "[1].rindex(1,0){true}",
    ] {
        let error = common::static_engine().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0301"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, 4, "{source}");
    }
}
