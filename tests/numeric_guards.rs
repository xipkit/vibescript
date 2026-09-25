mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value};

const GUARDS: &[(&str, &str)] = &[
    (
        "9223372036854775808.times {entered()}",
        "int.times count must fit in a 64-bit integer",
    ),
    (
        "(-9223372036854775809).times {entered()}",
        "int.times count must fit in a 64-bit integer",
    ),
    (
        "9223372036854775808.upto(0) {entered()}",
        "int.upto bounds must fit in a 64-bit integer",
    ),
    (
        "0.upto(9223372036854775808) {entered()}",
        "int.upto bounds must fit in a 64-bit integer",
    ),
    (
        "(-9223372036854775809).downto(0) {entered()}",
        "int.downto bounds must fit in a 64-bit integer",
    ),
    (
        "0.downto(-9223372036854775809) {entered()}",
        "int.downto bounds must fit in a 64-bit integer",
    ),
    (
        "9223372036854775808.step(0) {entered()}",
        "int.step bounds must fit in a 64-bit integer",
    ),
    (
        "0.step(9223372036854775808,0) {entered()}",
        "int.step bounds must fit in a 64-bit integer",
    ),
    (
        "0.step(1,9223372036854775808) {entered()}",
        "int.step bounds must fit in a 64-bit integer",
    ),
    (
        "(1..3).step(9223372036854775808) {entered()}",
        "range.step step must fit in a 64-bit integer",
    ),
    (
        "(..3).step(-9223372036854775809) {entered()}",
        "range.step step must fit in a 64-bit integer",
    ),
    (
        "(0..9223372036854775807).to_a",
        "range.to_a result too large",
    ),
    (
        "(-9223372036854775808..-1).to_a",
        "range.to_a result too large",
    ),
    (
        "a.fill(0,9223372036854775807,1)",
        "array.fill window is too large",
    ),
    (
        "a.fill(0,0..9223372036854775807)",
        "array.fill window is too large",
    ),
];

#[test]
fn representational_guards_recover_without_callbacks_or_partial_mutation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(0))
    });
    for (expression, message) in GUARDS {
        let source = format!(
            "a=[1,2];begin\n{expression}\nrescue LimitError=>e\n[e.class,e.message,a,a.fill(0,1,1)]\nend"
        );
        let mut options = CallOptions::default();
        options.limits.steps = Some(10_000);
        options.limits.memory_bytes = Some(128 << 10);
        let output = engine
            .compile(&source)
            .unwrap()
            .run(options)
            .unwrap_or_else(|error| panic!("{expression}: {error}"));
        let values = output.value.as_array().unwrap();
        assert_eq!(
            values[0].as_bytes(),
            Some(b"LimitError".as_slice()),
            "{expression}"
        );
        assert_eq!(
            values[1].as_bytes(),
            Some(message.as_bytes()),
            "{expression}"
        );
        assert_eq!(
            values[2].as_array().unwrap()[1].as_int(),
            Some(2),
            "{expression}"
        );
        assert_eq!(
            values[3].as_array().unwrap()[1].as_int(),
            Some(0),
            "{expression}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{expression}");
    }
    // fill takes no block, so its block form is refused before anything
    // runs.
    let mut checked = common::static_engine();
    checked.register("entered", |_, _| panic!("entered ran"));
    for (expression, expected) in [
        ("a.fill(9223372036854775807,1) {entered()}", &["V0305"][..]),
        (
            "a.fill(..9223372036854775807) {entered()}",
            &["V0101", "V0305"],
        ),
    ] {
        let error = checked
            .compile(&format!("a=[1,2]\n{expression}"))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), expected, "{expression}");
    }
}

#[test]
fn validation_order_distinguishes_runtime_errors_from_limits() {
    for (expression, expected) in [
        ("0.step(9223372036854775808,0) {0}", "LimitError"),
        ("(1..).step(9223372036854775808) {0}", "LimitError"),
        ("(0..9223372036854775807).length", "RuntimeError"),
        ("[1,2].fill(0,-10..9223372036854775807)", "RuntimeError"),
    ] {
        let source =
            format!("begin\n{expression}\nrescue LimitError | RuntimeError => e\ne.class\nend");
        let output = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(
            output.value.as_bytes(),
            Some(expected.as_bytes()),
            "{expression}"
        );
    }
    // A missing block and arguments of the wrong type or count are refused
    // before any bound is checked.
    for (expression, expected) in [
        ("9223372036854775808.times", &["V0304"][..]),
        ("9223372036854775808.times(1) {0}", &["V0301"]),
        ("9223372036854775808.upto(nil) {0}", &["V0101"]),
        ("9223372036854775808.upto(1)", &["V0304"]),
        ("9223372036854775808.step(1,:bad)", &["V0304", "V0101"]),
        ("0.step(1.5,9223372036854775808)", &["V0304", "V0101"]),
    ] {
        let error = common::static_engine().compile(expression).err().unwrap();
        assert_eq!(common::codes(&error), expected, "{expression}");
    }
    let error = Engine::new()
        .compile("begin\n0.step(9223372036854775808) {0}\nrescue\n42\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Arithmetic);
    assert!(error.message.contains("bounds must fit"));
}

#[test]
fn recovered_guards_release_storage_and_preserve_exact_budgets() {
    let source = "def guarded -> array<int> | int\na=[1,2];begin\na.fill(0,9223372036854775807,1)\nrescue LimitError=>e\ne.message;42\nend\nend\ndef run(n: int) -> int\ni=0;while i<n\nguarded;i+=1\nend;42\nend";
    let script = Engine::new().compile(source).unwrap();
    let first = script
        .call("run", &[Value::int(1)], CallOptions::default())
        .unwrap();
    let repeated = script
        .call("run", &[Value::int(32)], CallOptions::default())
        .unwrap();
    assert_eq!(repeated.value.as_int(), Some(42));
    assert_eq!(repeated.stats.retained_memory_bytes, 0);
    assert_eq!(
        repeated.stats.peak_memory_bytes,
        first.stats.peak_memory_bytes
    );
    let mut options = CallOptions::default();
    options.limits.steps = Some(repeated.stats.steps);
    options.limits.memory_bytes = Some(repeated.stats.peak_memory_bytes);
    script
        .call("run", &[Value::int(32)], options.clone())
        .unwrap();
    options.limits.steps = Some(repeated.stats.steps - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(32)], options.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(repeated.stats.steps);
    options.limits.memory_bytes = Some(repeated.stats.peak_memory_bytes - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(32)], options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn cancellation_and_actual_exhaustion_cannot_be_replaced_or_rescued() {
    for cancel in [false, true] {
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = calls.clone();
        let mut engine = Engine::new();
        engine.register("stop", move |ctx, _| {
            if cancel {
                cancellation.cancel();
            } else {
                ctx.charge(u64::MAX)?;
            }
            Ok(Value::int(0))
        });
        engine.register("after", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        let source = "begin\nstop().as(int).step(9223372036854775808) {after()};after()\nrescue LimitError | RuntimeError\nafter()\nensure\nafter()\nend";
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions {
                cancellation: token,
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(
            error.kind,
            if cancel {
                ErrorKind::Cancelled
            } else {
                ErrorKind::Steps
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn bounded_endless_reads_stop_at_the_integer_ceiling() {
    let source = "[(9223372036854775807..).first(3),(9223372036854775806..).first(3),(9223372036854775807..).first(0),(-9223372036854775808..9223372036854775807).last(2),(9223372036854775807..-9223372036854775808).last(2)]";
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let expected = [
        vec![i64::MAX],
        vec![i64::MAX - 1, i64::MAX],
        vec![],
        vec![i64::MAX - 1, i64::MAX],
        vec![i64::MIN + 1, i64::MIN],
    ];
    for (actual, expected) in output.value.as_array().unwrap().iter().zip(expected) {
        let actual: Vec<_> = actual
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_int().unwrap())
            .collect();
        assert_eq!(actual, expected);
    }
}
