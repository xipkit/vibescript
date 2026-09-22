mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorClass, ErrorKind, Limits, Value, stringify_json};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    json(&result.value)
}

fn fixed_entropy_engine(byte: u8) -> (Engine, Arc<AtomicUsize>) {
    let mut engine = Engine::new();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    engine.set_random_source(move |_, output| {
        count.fetch_add(1, Ordering::SeqCst);
        output.fill(byte);
        Ok(output.len())
    });
    (engine, reads)
}

fn ints(values: &[i64]) -> Value {
    Value::array(values.iter().copied().map(Value::int).collect())
}

#[test]
fn rotate_and_product_match_the_reference_outputs() {
    for (source, expected) in [
        ("[1,2,3].rotate", serde_json::json!([2, 3, 1])),
        ("[1,2,3].rotate(2)", serde_json::json!([3, 1, 2])),
        ("[1,2,3].rotate(-1)", serde_json::json!([3, 1, 2])),
        ("[1,2,3].rotate(4)", serde_json::json!([2, 3, 1])),
        ("[1,2,3].rotate(0)", serde_json::json!([1, 2, 3])),
        ("[1,2,3].rotate(1.9)", serde_json::json!([2, 3, 1])),
        (
            "[1,2,3].rotate(-9223372036854775808)",
            serde_json::json!([2, 3, 1]),
        ),
        (
            "[1,2,3].rotate(9223372036854775807)",
            serde_json::json!([2, 3, 1]),
        ),
        ("[].rotate", serde_json::json!([])),
        ("[].rotate(5)", serde_json::json!([])),
        ("[7].rotate(-3)", serde_json::json!([7])),
        (
            "[1,2].product(['a','b'])",
            serde_json::json!([[1, "a"], [1, "b"], [2, "a"], [2, "b"]]),
        ),
        (
            "[1,2].product([3],[4,5])",
            serde_json::json!([[1, 3, 4], [1, 3, 5], [2, 3, 4], [2, 3, 5]]),
        ),
        ("[1,2].product", serde_json::json!([[1], [2]])),
        ("[1,2].product([])", serde_json::json!([])),
        ("[].product([1])", serde_json::json!([])),
        ("[].product", serde_json::json!([])),
        ("[1].product([],[2,3])", serde_json::json!([])),
        (
            "[[1],nil].product([{a:1}])",
            serde_json::json!([[[1], {"a": 1}], [null, {"a": 1}]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn tuple_generators_preserve_order_duplicates_and_default_lengths() {
    for (source, expected) in [
        (
            "[1,2,3].combination(2)",
            serde_json::json!([[1, 2], [1, 3], [2, 3]]),
        ),
        ("[1,2,3].combination(0)", serde_json::json!([[]])),
        ("[1,2,3].combination(3)", serde_json::json!([[1, 2, 3]])),
        ("[1,2,3].combination(4)", serde_json::json!([])),
        ("[1,2,3].combination(-1)", serde_json::json!([])),
        (
            "[1,2,3].combination(2.7)",
            serde_json::json!([[1, 2], [1, 3], [2, 3]]),
        ),
        ("[].combination(0)", serde_json::json!([[]])),
        ("[].combination(1)", serde_json::json!([])),
        ("[1,1].combination(2)", serde_json::json!([[1, 1]])),
        (
            "[1,2,3,4].combination(3)",
            serde_json::json!([[1, 2, 3], [1, 2, 4], [1, 3, 4], [2, 3, 4]]),
        ),
        (
            "[1,2,3].permutation(2)",
            serde_json::json!([[1, 2], [1, 3], [2, 1], [2, 3], [3, 1], [3, 2]]),
        ),
        (
            "[1,2,3].permutation",
            serde_json::json!([
                [1, 2, 3],
                [1, 3, 2],
                [2, 1, 3],
                [2, 3, 1],
                [3, 1, 2],
                [3, 2, 1]
            ]),
        ),
        ("[1,2].permutation(0)", serde_json::json!([[]])),
        ("[1,2].permutation(3)", serde_json::json!([])),
        ("[1,2].permutation(-1)", serde_json::json!([])),
        ("[].permutation", serde_json::json!([[]])),
        ("[].permutation(0)", serde_json::json!([[]])),
        ("[].permutation(1)", serde_json::json!([])),
        ("[1,1].permutation(2)", serde_json::json!([[1, 1], [1, 1]])),
        ("[1,2,3].permutation(1)", serde_json::json!([[1], [2], [3]])),
        (
            "[1,2].repeated_combination(2)",
            serde_json::json!([[1, 1], [1, 2], [2, 2]]),
        ),
        (
            "[1,2,3].repeated_combination(2)",
            serde_json::json!([[1, 1], [1, 2], [1, 3], [2, 2], [2, 3], [3, 3]]),
        ),
        ("[1,2].repeated_combination(0)", serde_json::json!([[]])),
        ("[].repeated_combination(0)", serde_json::json!([[]])),
        ("[].repeated_combination(2)", serde_json::json!([])),
        (
            "[1].repeated_combination(3)",
            serde_json::json!([[1, 1, 1]]),
        ),
        ("[1,2].repeated_combination(-2)", serde_json::json!([])),
        (
            "[1,2].repeated_combination(3)",
            serde_json::json!([[1, 1, 1], [1, 1, 2], [1, 2, 2], [2, 2, 2]]),
        ),
        (
            "[1,2].repeated_permutation(2)",
            serde_json::json!([[1, 1], [1, 2], [2, 1], [2, 2]]),
        ),
        (
            "[1].repeated_permutation(3)",
            serde_json::json!([[1, 1, 1]]),
        ),
        ("[].repeated_permutation(0)", serde_json::json!([[]])),
        ("[].repeated_permutation(1)", serde_json::json!([])),
        ("[1,2].repeated_permutation(0)", serde_json::json!([[]])),
        ("[1,2].repeated_permutation(-1)", serde_json::json!([])),
        (
            "[1,2,3].repeated_permutation(1)",
            serde_json::json!([[1], [2], [3]]),
        ),
        (
            "['a',:b].repeated_permutation(2)",
            serde_json::json!([["a", "a"], ["a", "b"], ["b", "a"], ["b", "b"]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn fixed_entropy_yields_reference_selections_without_extra_reads() {
    let (engine, reads) = fixed_entropy_engine(0);
    let output = engine
        .compile(
            "[[1,2,3,4].sample, [1,2,3].sample(2), [1,2,3].shuffle, [1,2].sample(5), \
             [].sample, [].sample(3), [1,2].sample(0), [].shuffle, [9].shuffle, [5].sample(1), [6].sample]",
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([1, [1, 2], [2, 3, 1], [1, 2], null, [], [], [], [9], [5], 6])
    );
    // One eight-byte read per selection: one for sample, two for sample(2),
    // two swaps for the three-element shuffle, two for sample(5), one each for
    // the single-element sample(1) and sample; empty and no-op cases read none.
    assert_eq!(reads.load(Ordering::SeqCst), 9);
    assert!(output.stats.retained_memory_bytes > 0);

    let (engine, reads) = fixed_entropy_engine(0);
    let output = engine
        .compile("[[].sample, [].sample(2), [1,2].sample(0), [].shuffle, [1].shuffle, [1,2].sample(-0.5)]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([null, [], [], [], [1], []])
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn seeded_sampling_is_deterministic_and_call_local() {
    let (engine, reads) = fixed_entropy_engine(0);
    let script = engine
        .compile(
            r#"
def run()
 srand(1234)
 single_a = [1, 2, 3, 4].sample
 srand(1234)
 single_b = [1, 2, 3, 4].sample
 srand(5678)
 sample_a = [1, 2, 3, 4].sample(2)
 srand(5678)
 sample_b = [1, 2, 3, 4].sample(2)
 srand(9012)
 shuffle_a = [1, 2, 3, 4].shuffle
 srand(9012)
 shuffle_b = [1, 2, 3, 4].shuffle
 srand(42)
 mixed_a = [rand(100), (1..10).to_a.shuffle, (1..10).to_a.sample(4), rand(100)]
 srand(42)
 mixed_b = [rand(100), (1..10).to_a.shuffle, (1..10).to_a.sample(4), rand(100)]
 [
  single_a == single_b,
  single_a >= 1 && single_a <= 4,
  sample_a == sample_b,
  sample_a.length == 2,
  sample_a.uniq.length == sample_a.length,
  shuffle_a == shuffle_b,
  shuffle_a.sort == [1, 2, 3, 4],
  mixed_a == mixed_b,
  mixed_a[1].sort == (1..10).to_a,
  mixed_a[2].uniq.length == 4,
  [].sample,
  [1, 2].sample(0),
  [1, 2].sample(5).length,
  [single_a, sample_a, shuffle_a, mixed_a]
 ]
end
def seeded
 srand(7)
 [(1..6).to_a.shuffle, (1..6).to_a.sample(3), (1..6).to_a.sample]
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    let values = json(&output.value);
    assert_eq!(
        values.as_array().unwrap()[..13],
        serde_json::json!([
            true,
            true,
            true,
            true,
            true,
            true,
            true,
            true,
            true,
            true,
            null,
            [],
            2
        ])
        .as_array()
        .unwrap()[..]
    );
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert!(output.stats.retained_memory_bytes > 0);
    let again = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(json(&again.value), values);

    let expected = json(
        &script
            .call("seeded", &[], CallOptions::default())
            .unwrap()
            .value,
    );
    common::scope(|scope| {
        let jobs = (0..4)
            .map(|_| scope.spawn(|| script.call("seeded", &[], CallOptions::default()).unwrap()))
            .collect::<Vec<_>>();
        for job in jobs {
            let output = job.join().unwrap();
            assert_eq!(json(&output.value), expected);
            assert!(output.stats.retained_memory_bytes > 0);
        }
    });
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn invalid_calls_fail_before_entropy_block_or_host_effects() {
    let (mut engine, reads) = fixed_entropy_engine(0);
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for (expression, message) in [
        (
            "[1].sample(k:1)",
            "array.sample does not take keyword arguments",
        ),
        (
            "[1].sample{effect()}",
            "array.sample does not accept a block",
        ),
        (
            "[1].sample(1){effect()}",
            "array.sample does not accept a block",
        ),
        ("[1].sample(1,2)", "array.sample accepts at most one count"),
        ("[1].sample(-1)", "array.sample count must be non-negative"),
        (
            "[1].sample(-1.5)",
            "array.sample count must be non-negative",
        ),
        (
            "[1].sample(-9223372036854775809)",
            "array.sample count must be non-negative",
        ),
        ("[1].sample('x')", "array.sample count must be integer"),
        ("[1].sample(nil)", "array.sample count must be integer"),
        (
            "[1].sample(9223372036854775808)",
            "array.sample count must be integer",
        ),
        ("[].sample('x')", "array.sample count must be integer"),
        ("[1].shuffle(1)", "array.shuffle does not take arguments"),
        (
            "[1].shuffle(k:1)",
            "array.shuffle does not take keyword arguments",
        ),
        (
            "[1].shuffle{effect()}",
            "array.shuffle does not accept a block",
        ),
        (
            "[1].rotate(k:1)",
            "array.rotate does not take keyword arguments",
        ),
        (
            "[1].rotate{effect()}",
            "array.rotate does not accept a block",
        ),
        ("[1].rotate(1,2)", "array.rotate accepts at most one count"),
        ("[1].rotate('x')", "array.rotate count must be integer"),
        ("[].rotate(nil)", "array.rotate count must be integer"),
        (
            "[1].rotate(9223372036854775808)",
            "array.rotate count must be integer",
        ),
        (
            "[1].product(k:1)",
            "array.product does not take keyword arguments",
        ),
        (
            "[1].product([1]){effect()}",
            "array.product does not accept a block",
        ),
        (
            "[1].product([],'x')",
            "array.product arguments must be arrays",
        ),
        ("[].product(1)", "array.product arguments must be arrays"),
        (
            "[1].product([2],nil)",
            "array.product arguments must be arrays",
        ),
        (
            "[1].combination(k:1)",
            "array.combination does not take keyword arguments",
        ),
        (
            "[1].combination(1){effect()}",
            "array.combination does not accept a block",
        ),
        (
            "[1].combination",
            "array.combination expects exactly one length",
        ),
        (
            "[1].combination(1,2)",
            "array.combination expects exactly one length",
        ),
        (
            "[1].combination('x')",
            "array.combination length must be integer",
        ),
        (
            "[1].combination(nil)",
            "array.combination length must be integer",
        ),
        (
            "[1].combination(9223372036854775808)",
            "array.combination length must be integer",
        ),
        (
            "[].combination(nil)",
            "array.combination length must be integer",
        ),
        (
            "[1].permutation(k:1)",
            "array.permutation does not take keyword arguments",
        ),
        (
            "[1].permutation{effect()}",
            "array.permutation does not accept a block",
        ),
        (
            "[1].permutation(1){effect()}",
            "array.permutation does not accept a block",
        ),
        (
            "[1].permutation(1,2)",
            "array.permutation expects exactly one length",
        ),
        (
            "[1].permutation('x')",
            "array.permutation length must be integer",
        ),
        (
            "[1].repeated_combination(k:1)",
            "array.repeated_combination does not take keyword arguments",
        ),
        (
            "[1].repeated_combination(1){effect()}",
            "array.repeated_combination does not accept a block",
        ),
        (
            "[1].repeated_combination",
            "array.repeated_combination expects exactly one length",
        ),
        (
            "[1].repeated_combination(1.5,2)",
            "array.repeated_combination expects exactly one length",
        ),
        (
            "[1].repeated_combination(:x)",
            "array.repeated_combination length must be integer",
        ),
        (
            "[1].repeated_permutation(k:1)",
            "array.repeated_permutation does not take keyword arguments",
        ),
        (
            "[1].repeated_permutation(1){effect()}",
            "array.repeated_permutation does not accept a block",
        ),
        (
            "[1].repeated_permutation",
            "array.repeated_permutation expects exactly one length",
        ),
        (
            "[1].repeated_permutation([1])",
            "array.repeated_permutation length must be integer",
        ),
    ] {
        let script = engine
            .compile(&format!("{expression};effect()"))
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        let error = script.run(CallOptions::default()).unwrap_err();
        assert!(
            matches!(error.kind, ErrorKind::Argument | ErrorKind::Type),
            "{expression}: {error:?}"
        );
        assert_eq!(error.message, message, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
        assert_eq!(reads.load(Ordering::SeqCst), 0, "{expression}");
        let rescued = engine
            .compile(&format!(
                "begin;{expression};rescue StandardError => e;e.message;end"
            ))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        assert_eq!(
            rescued.value.as_bytes(),
            Some(message.as_bytes()),
            "{expression}"
        );
        assert!(rescued.stats.retained_memory_bytes > 0, "{expression}");
    }
    // Arguments are still evaluated before the call rejects them.
    let error = engine
        .compile("[1].sample(effect(), effect());effect()")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, "array.sample accepts at most one count");
    assert_eq!(effects.load(Ordering::SeqCst), 2);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
}

#[test]
fn entropy_callbacks_cannot_swallow_cancellation_or_quota_exhaustion() {
    for cancelled in [false, true] {
        for expression in ["[1,2].sample", "[1,2].sample(2)", "[1,2].shuffle"] {
            let reads = Arc::new(AtomicUsize::new(0));
            let effects = Arc::new(AtomicUsize::new(0));
            let mut engine = Engine::new();
            let read_count = reads.clone();
            engine.set_random_source(move |ctx, output| {
                read_count.fetch_add(1, Ordering::SeqCst);
                if cancelled {
                    ctx.cancellation().cancel();
                } else {
                    let _ = ctx.charge(u64::MAX);
                }
                output.fill(0);
                Ok(output.len())
            });
            let effect_count = effects.clone();
            engine.register("effect", move |_, _| {
                effect_count.fetch_add(1, Ordering::SeqCst);
                Ok(Value::nil())
            });
            let script = engine
                .compile(&format!(
                    "begin;{expression};rescue RuntimeError;effect();end;effect()"
                ))
                .unwrap();
            let error = script.run(CallOptions::default()).unwrap_err();
            assert_eq!(
                error.kind,
                if cancelled {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::Steps
                }
            );
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            assert_eq!(effects.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn oversized_results_raise_recoverable_limit_errors_before_allocating() {
    for (expression, message) in [
        (
            "(1..200).to_a.combination(100)",
            "array.combination result too large",
        ),
        (
            "(1..25).to_a.permutation",
            "array.permutation result too large",
        ),
        (
            "(1..70).to_a.permutation(20)",
            "array.permutation result too large",
        ),
        (
            "[1,2].repeated_combination(9223372036854775807)",
            "array.repeated_combination result too large",
        ),
        (
            "[1].repeated_combination(9223372036854775807)",
            "array.repeated_combination result too large",
        ),
        (
            "[1].repeated_permutation(9223372036854775807)",
            "array.repeated_permutation result too large",
        ),
        (
            "(1..100).to_a.repeated_combination(60)",
            "array.repeated_combination result too large",
        ),
        (
            "[1,2].repeated_permutation(64)",
            "array.repeated_permutation result too large",
        ),
        (
            "[1,2].repeated_permutation(63)",
            "array.repeated_permutation result too large",
        ),
        (
            "[1,2,3].repeated_permutation(9223372036854775807)",
            "array.repeated_permutation result too large",
        ),
        (
            "a=(1..100).to_a;a.product(a,a,a,a,a,a,a,a,a)",
            "array.product result too large",
        ),
    ] {
        let source = format!(
            "begin\n{expression}\nrescue LimitError => e\n[e.type, e.message, [1,2].combination(1)]\nend"
        );
        let output = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        assert_eq!(
            json(&output.value),
            serde_json::json!(["LimitError", message, [[1], [2]]]),
            "{expression}"
        );
        assert!(output.stats.retained_memory_bytes > 0, "{expression}");
        assert!(output.stats.peak_memory_bytes < 64 << 10, "{expression}");
        let error = Engine::new()
            .compile(&format!(
                "begin\n{expression}\nrescue StandardError\n42\nend"
            ))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Arithmetic, "{expression}");
        assert_eq!(error.class(), Some(ErrorClass::Limit), "{expression}");
    }
}

#[test]
fn huge_requested_results_exhaust_quotas_before_building_and_cannot_be_rescued() {
    let script = Engine::new()
        .compile(
            "def run(a, n)\nbegin\n[a.combination(n), a.permutation(n), a.product(a, a), \
             a.repeated_combination(n), a.repeated_permutation(n)]\nrescue\n42\nend\nend\n\
             def wide(n)\nbegin\n[[1].repeated_permutation(n), [1].repeated_combination(n)]\nrescue\n42\nend\nend",
        )
        .unwrap();
    let steps = CallOptions {
        limits: Limits {
            steps: Some(5_000),
            memory_bytes: Some(1 << 30),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    for (values, length) in [(20, 10), (8, 8), (100, 2), (10, 6), (2, 20)] {
        let array = ints(&(1..=values).collect::<Vec<_>>());
        let error = script
            .call("run", &[array, Value::int(length)], steps.clone())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{values} choose {length}");
    }
    for length in [100_000_000_i64, i64::MAX - 1] {
        let error = script
            .call("wide", &[Value::int(length)], steps.clone())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{length}");
    }
    let memory = CallOptions {
        limits: Limits {
            steps: None,
            memory_bytes: Some(1 << 20),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let error = script
        .call("wide", &[Value::int(100_000)], memory.clone())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    let array = ints(&(1..=40).collect::<Vec<_>>());
    let error = script
        .call("run", &[array, Value::int(5)], memory)
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
}

#[test]
fn exhausted_work_stops_before_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "a.permutation(4)",
        "a.combination(5)",
        "a.product(a,a)",
        "a.repeated_combination(5)",
        "a.repeated_permutation(3)",
        "a.shuffle",
        "a.sample(9)",
        "a.rotate(3)",
    ] {
        let script = engine
            .compile(&format!("a=(0..9).to_a;{expression};effect()"))
            .unwrap();
        let error = script
            .run(CallOptions {
                limits: Limits {
                    steps: Some(40),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps, "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn repeated_results_release_storage() {
    for expression in [
        "a.combination(3)",
        "a.permutation(3)",
        "a.product(a)",
        "a.repeated_combination(3)",
        "a.repeated_permutation(2)",
        "a.shuffle",
        "a.sample(5)",
        "a.rotate(4)",
    ] {
        let script = Engine::new()
            .compile(&format!(
                "a=(0..9).to_a;i=0;while i<24;r={expression};i+=1;end;r.length"
            ))
            .unwrap();
        let result = script
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(512 << 10),
                    steps: None,
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        assert!(result.value.as_int().unwrap() > 0, "{expression}");
        assert!(result.stats.peak_memory_bytes < 512 << 10, "{expression}");
        assert_eq!(result.stats.retained_memory_bytes, 0, "{expression}");
    }
}

#[test]
fn results_are_independent_of_the_receiver_and_of_each_other() {
    for (source, expected) in [
        (
            "a=[[1],[2]];r=a.combination(1);r[0][0].push(9);r[1].push(8);[a,r]",
            serde_json::json!([[[1], [2]], [[[1, 9]], [[2], 8]]]),
        ),
        (
            "a=[[1],[2]];b=[[3]];r=a.product(b);r[0][0].push(9);r[1][1].push(7);[a,b,r]",
            serde_json::json!([[[1], [2]], [[3]], [[[1, 9], [3]], [[2], [3, 7]]]]),
        ),
        (
            "a=[[1]];r=a.repeated_permutation(2);r[0][0].push(5);[a,r]",
            serde_json::json!([[[1]], [[[1, 5], [1]]]]),
        ),
        (
            "a=[[1]];r=a.repeated_combination(2);r[0][1].push(5);[a,r]",
            serde_json::json!([[[1]], [[[1], [1, 5]]]]),
        ),
        (
            "r=[1,2].permutation(2);r[0].push(0);r",
            serde_json::json!([[1, 2, 0], [2, 1]]),
        ),
        (
            "a=[1,2];old=a;r=a.rotate;r.push(3);a.push(4);[a,old,r]",
            serde_json::json!([[1, 2, 4], [1, 2], [2, 1, 3]]),
        ),
        (
            "a=[[1],[2]];r=a.rotate;r[0].push(9);[a,r]",
            serde_json::json!([[[1], [2]], [[2, 9], [1]]]),
        ),
        (
            "a=[1,2,3];r=a.shuffle;a.push(4);r.push(5);[a.length,r.length,r.sort[0..2]]",
            serde_json::json!([4, 4, [1, 2, 3]]),
        ),
        (
            "a=[[1],[2]];r=a.sample(2);r[0].push(9);r[1].push(9);[a,r.length]",
            serde_json::json!([[[1], [2]], 2]),
        ),
        (
            "a=[[1]];r=a.sample;r.push(9);[a,r]",
            serde_json::json!([[[1]], [1, 9]]),
        ),
        (
            "a=[1];r=a.product(a.push(2));[a,r]",
            serde_json::json!([[1, 2], [[1, 1], [1, 2]]]),
        ),
        (
            "a=[1,2];r=a.combination(a.push(3).length - 2);[a,r]",
            serde_json::json!([[1, 2, 3], [[1], [2]]]),
        ),
    ] {
        assert_eq!(evaluate(source), expected, "{source}");
    }
}

#[test]
fn checker_facts_follow_runtime_outcomes() {
    for (source, expected) in [
        ("def run -> array; [1,2,3].rotate(1); end", 0),
        ("def run -> array; [1,2].product([3], [4,5]); end", 0),
        ("def run -> array; [1,2,3].combination(2); end", 0),
        ("def run -> array; [1,2,3].permutation; end", 0),
        ("def run -> array; [1,2].repeated_combination(2); end", 0),
        ("def run -> array; [1,2].repeated_permutation(2); end", 0),
        ("def run -> array; [3,1,2].shuffle; end", 0),
        ("def run -> array; [3,1,2].sample(2); end", 0),
        ("def run -> int; [3,1,2].sample; end", 0),
        ("def run -> int; [1,2].sample(0).length; end", 0),
        ("def run -> int; [1,2].combination(2).length; end", 0),
        (
            "def run -> int; if [].sample == nil; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [1,2].sample(0) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [1,2].permutation(3) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [1,2].combination(-1) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [].combination(0) == [[]]; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [].permutation == [[]]; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [].repeated_permutation(2) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [].product([1]) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [].rotate(2) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [].shuffle == []; 7; else; 'wrong'; end; end",
            7,
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        if expected != 0 {
            assert_eq!(result.value.as_int(), Some(expected), "{source}");
        }
    }
    for expression in [
        "[1].shuffle(1)",
        "[1].rotate('x')",
        "[1].rotate(1,2)",
        "[1].sample(-1)",
        "[1].sample(nil)",
        "[1].sample(1,2)",
        "[1].product(1)",
        "[1].product([1],'x')",
        "[1].combination",
        "[1].combination('x')",
        "[1].permutation(1,2)",
        "[1].repeated_combination(:x)",
        "[1].repeated_permutation",
    ] {
        let script = Engine::new()
            .compile(&format!("def run; {expression}; end"))
            .unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(!report.is_clean(), "{expression}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert!(
            matches!(error.kind, ErrorKind::Argument | ErrorKind::Type),
            "{expression}: {error:?}"
        );
    }
}
