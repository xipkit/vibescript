//! `==` compares values. The removed `eql?` and `equal?`, the strict and
//! identity comparisons they made, and builtins read as values are reported
//! by the surface and builtin tests.

mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn integer_float_equality_does_not_round_away_distinct_values() {
    for (integer, float, expected) in [
        (9_007_199_254_740_993, 9_007_199_254_740_992.0, false),
        (-9_007_199_254_740_993, -9_007_199_254_740_992.0, false),
        (i64::MAX, 9_223_372_036_854_775_808.0, false),
        (i64::MIN, -9_223_372_036_854_775_808.0, true),
        (9_007_199_254_740_992, 9_007_199_254_740_992.0, true),
        (0, -0.0, true),
        (0, f64::from_bits(1), false),
        (0, f64::NAN, false),
        (i64::MAX, f64::INFINITY, false),
        (i64::MIN, f64::NEG_INFINITY, false),
    ] {
        let output = Engine::new()
            .compile("def run(a: int, b: float) -> array<bool>\n[a == b,b == a,a != b,[a] == [b],{x:a} == {x:b}]\nend")
            .unwrap()
            .call("run", &[Value::int(integer),Value::float(float)], CallOptions::default())
            .unwrap();
        let values: Vec<_> = output
            .value
            .as_array()
            .unwrap()
            .iter()
            .map(Value::truthy)
            .collect();
        assert_eq!(
            values,
            vec![expected, expected, !expected, expected, expected],
            "{integer} / {float}"
        );
    }
}

#[test]
fn comparisons_capture_the_receiver_before_argument_mutation() {
    assert_eq!(
        result("a=[1];r=(a == a.push(2));[r,a]"),
        serde_json::json!([false, [1, 2]])
    );
    assert_eq!(
        result("a=[[1]];r=(a == begin\na[0]&.push(2);a\nend);[r,a]"),
        serde_json::json!([false, [[1, 2]]])
    );
    assert_eq!(
        result("class C\nproperty link: C?\nend\na=C.new;a.link=a;[a == a.link]"),
        serde_json::json!([true])
    );
}

#[test]
fn hash_fields_named_like_removed_predicates_stay_data() {
    assert_eq!(
        result("h={\"eql?\":3,\"equal?\":4};[h == h,h[\"eql?\"],h[\"equal?\"]]"),
        serde_json::json!([true, 3, 4])
    );
    // A method named like a removed predicate cannot be called by it.
    let source = "class C\ndef eql?(x: int) -> int\nx+3\nend\ndef equal?(x: int) -> int\nx+4\nend\nend\na=C.new;[a.eql?(2),a.equal?(2)]";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0403", "V0403"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("eql?(2)").unwrap()
    );
}

#[test]
fn comparisons_meter_raw_bytes_and_release_captured_values() {
    let script = Engine::new()
        .compile("def run(a: string, b: string) -> array<bool>\n[a == b,b == a]\nend")
        .unwrap();
    let mut data = vec![0xff; 64 << 10];
    data[0] = 0;
    let args = [Value::bytes(data.clone()), Value::bytes(data)];
    let output = script.call("run", &args, CallOptions::default()).unwrap();
    assert_eq!(output.value.to_string(), "[true, true]");
    assert!(output.stats.steps >= 2 * (64 << 10) / 64);
    assert!(output.stats.retained_memory_bytes < 1024);
    let mut options = CallOptions::default();
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
    script.call("run", &args, options.clone()).unwrap();
    options.limits.steps = Some(output.stats.steps - 1);
    assert_eq!(
        script.call("run", &args, options.clone()).unwrap_err().kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("run", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn cancellation_during_arguments_prevents_comparison_and_later_effects() {
    let token = CancellationToken::new();
    let cancellation = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancellation.cancel();
        Ok(Value::bytes(vec![1; 8192]))
    });
    engine.register("after", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let error = engine.compile("begin\n(\"x\"*8192) == stop();after()\nrescue LimitError | RuntimeError\nafter()\nensure\nafter()\nend").unwrap().run(CallOptions { cancellation: token, ..CallOptions::default() }).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn enum_imports_preserve_aliases_and_rebind_owned_arguments() {
    let script = Engine::new().compile("enum E\nA\nB\nend\ndef make -> array<any>\n[E,E::A,E::A,E::B]\nend\ndef check(x: array<any>) -> array<bool>\n[x.fetch(0) == E,x.fetch(1) == E::A,x.fetch(1) == x.fetch(2),x.fetch(3).as(E).enum == x.fetch(0),x.fetch(1) == x.fetch(3)]\nend").unwrap();
    let output = script.call("make", &[], CallOptions::default()).unwrap();
    let args = [output.value.clone()];
    let checked = script.call("check", &args, CallOptions::default()).unwrap();
    assert_eq!(checked.value.to_string(), "[true, true, true, true, false]");

    // Another script holds the foreign members as `any`.
    let foreign = Engine::new()
        .compile(
            "def check(x: array<any>) -> array<bool>\n[x.fetch(1) == x.fetch(2),x.fetch(1) == x.fetch(3)]\nend",
        )
        .unwrap();
    assert_eq!(
        foreign
            .call("check", &args, CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "[true, false]"
    );
    let mut options = CallOptions::default();
    options.limits.steps = Some(checked.stats.steps);
    options.limits.memory_bytes = Some(checked.stats.peak_memory_bytes);
    script.call("check", &args, options.clone()).unwrap();
    options.limits.memory_bytes = Some(checked.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("check", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn enum_rebinding_reaches_instance_fields_and_keyword_arguments() {
    let script = Engine::new().compile("enum E\nA\nend\nclass Box\nproperty items: array<E>\nend\ndef make -> Box\nb=Box.new;b.items=[E::A,E::A];b\nend\ndef check(*, box: Box, item: E) -> array<bool>\n[box.items.fetch(0) == E::A,box.items.fetch(0) == box.items.fetch(1),item == E::A]\nend\ndef member -> E\nE::A\nend").unwrap();
    let value = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let item = script
        .call("member", &[], CallOptions::default())
        .unwrap()
        .value;
    let output = script
        .call_with_keywords(
            "check",
            &[],
            &[("box".into(), value), ("item".into(), item)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(output.value.to_string(), "[true, true, true]");
    assert!(output.stats.retained_memory_bytes < 1024);
}

const SHARED_DAG: &str = "def build(d: int) -> array<any>\n  cur: array<any> = [1]\n  i = 0\n  while i < d\n    cur = [cur, cur]\n    i = i + 1\n  end\n  cur\nend\n";

fn run_with(source: &str, limits: vibescript::Limits) -> Result<(String, u64), ErrorKind> {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions {
            limits,
            ..CallOptions::default()
        })
        .map_err(|error| error.kind)?;
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    Ok((
        String::from_utf8(encoded.value.as_bytes().unwrap().to_vec()).unwrap(),
        output.stats.steps,
    ))
}

#[test]
fn shared_structures_compare_each_pair_of_containers_once() {
    // The reference runs this with a 5,000,000-step and 64 MiB quota.
    let source = format!(
        "{SHARED_DAG}s = \"ab\" * 2048\na = [build(24), s]\nb = [build(24), s]\n(a == b).to_s"
    );
    let limits = vibescript::Limits {
        steps: Some(5_000_000),
        memory_bytes: Some(64 << 20),
        ..vibescript::Limits::default()
    };
    assert_eq!(run_with(&source, limits).unwrap().0, "\"true\"");
    // Every walk that compares elements shares the memo. Arrays are not
    // ordered, so `<=>` on them is refused (see the ordering tests).
    for expression in [
        "a == b",
        "[a].include?(b)",
        "[a].index(b)",
        "[a].count(b)",
        "[a, b].uniq.length",
        "([a] - [b]).length",
        "{x: a} == {x: b}",
        "case a\nwhen b\n  1\nend",
    ] {
        let unlimited = vibescript::Limits {
            steps: None,
            ..vibescript::Limits::default()
        };
        let cost = |depth: usize| {
            let prefix = format!("{SHARED_DAG}a = build({depth})\nb = build({depth})\n");
            let base = run_with(&format!("{prefix}nil"), unlimited.clone())
                .unwrap()
                .1;
            let (value, steps) =
                run_with(&format!("{prefix}{expression}"), unlimited.clone()).unwrap();
            (value, steps - base)
        };
        let (small, large) = (cost(20), cost(40));
        assert_eq!(small.0, large.0, "{expression}");
        // Twenty more levels cost a few steps each, not 2^20 times more.
        assert!(
            large.1 - small.1 < 20 * 8,
            "{expression}: {small:?} {large:?}"
        );
        let source = format!("{SHARED_DAG}a = build(24)\nb = build(24)\n{expression}");
        assert!(
            run_with(&source, vibescript::Limits::default()).is_ok(),
            "{expression}"
        );
    }
}

#[test]
fn equality_remembers_shared_pairs_beyond_any_fixed_window() {
    // Each level separates the two references to its child with 300 distinct
    // completed pairs, more than a bounded window of recent pairs retains.
    let source = |depth: usize| {
        format!(
            "def build(d: int) -> array<any>\n  cur: array<any> = [1]\n  i = 0\n  while i < d\n    fill = (0..300).to_a.map {{ |k| [k] }}\n    cur = [cur] + fill + [cur]\n    i = i + 1\n  end\n  cur\nend\na = build({depth})\nb = build({depth})\n[a == b]"
        )
    };
    let unlimited = vibescript::Limits {
        steps: None,
        ..vibescript::Limits::default()
    };
    let base = |depth: usize| {
        let source = source(depth).replace("[a == b]", "nil");
        run_with(&source, unlimited.clone()).unwrap().1
    };
    let cost = |depth: usize| {
        let (value, steps) = run_with(&source(depth), unlimited.clone()).unwrap();
        assert_eq!(value, "[true]");
        steps - base(depth)
    };
    let (small, large) = (cost(8), cost(16));
    assert!(large < 3 * small, "{small} then {large}");
}
