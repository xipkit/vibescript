use vibescript::{CallOptions, CheckReport, Engine, Script, Value};

fn compile(source: &str) -> Script {
    Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
}

fn check(script: &Script, source: &str) -> CheckReport {
    let report = script
        .check_function("run", &CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert!(report.incomplete.is_empty(), "{source}: {report:?}");
    report
}

fn returns_bad_type(report: &CheckReport) -> bool {
    report
        .diagnostics
        .iter()
        .any(|d| d.message.starts_with("Return value:"))
}

fn ints(values: &[i64]) -> Value {
    Value::array(values.iter().copied().map(Value::int).collect())
}

/// Each expression is clean as an `int` result and evaluates to `expected`.
fn exact_ints(cases: &[(&str, i64)]) {
    for &(expression, expected) in cases {
        let source = format!("def run -> int; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        let value = script
            .call("run", &[], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
}

/// Each expression is a known contradiction that also fails at runtime.
fn invalid(expressions: &[&str]) {
    for expression in expressions {
        let source = format!("def run; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(
            report.diagnostics.iter().any(|d| {
                d.message.contains("does not accept") || d.message.starts_with("Operator")
            }),
            "{source}: {report:?}"
        );
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{source}"
        );
    }
}

/// Each declaration checks cleanly, its exact call with `args` is clean and
/// execution returns `expected`.
fn witnesses(cases: Vec<(&str, Vec<Value>, Value)>) {
    for (source, args, expected) in cases {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        let report = script
            .check_call("run", &args, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let value = script
            .call("run", &args, CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.to_string(), expected.to_string(), "{source}");
    }
}

/// `failing` declarations keep their rescue path reachable; `safe` ones do not.
fn rescues(failing: &[&str], safe: &[&str]) {
    for source in failing {
        let script = compile(source);
        assert!(returns_bad_type(&check(&script, source)), "{source}");
    }
    for source in safe {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
    }
}

/// A finite invalid alternative stays a contradiction even when the supplied
/// alternative succeeds at runtime.
fn strict_arms(cases: Vec<(&str, Value)>) {
    for (source, arg) in cases {
        let script = compile(source);
        let report = check(&script, source);
        assert!(!report.diagnostics.is_empty(), "{source}");
        assert!(
            script.call("run", &[arg], CallOptions::default()).is_ok(),
            "{source}"
        );
    }
}

#[test]
fn array_set_operators_follow_membership_and_keep_order() {
    exact_ints(&[
        ("([1, 2, 3, 2] - [2]).length", 2),
        ("([1, 2, 3, 2] - [2]).last", 3),
        ("([1, 2.0, 2] - [2]).length", 2),
        ("([1, 2, 2, 3] & [3, 2]).first", 2),
        ("([1, 2, 2, 3] & [3, 2]).length", 2),
        ("([1, 2] & []).length", 0),
    ]);
    invalid(&["[1, 2] - 1", "[1] & nil", "[1] - 'x'"]);
    witnesses(vec![(
        "def run(xs: array<int>, ys: array<int>) -> array<int>; xs - ys; end",
        vec![ints(&[1, 2, 3]), ints(&[2])],
        ints(&[1, 3]),
    )]);
    strict_arms(vec![(
        "def run(ys: array<int> | int) -> array<int>; [1] - ys; end",
        ints(&[1]),
    )]);
}

#[test]
fn join_inspect_and_flatten_walk_nested_values() {
    exact_ints(&[
        ("[[1, [2]], 3].flatten(1).length", 3),
        ("[[1, [2]], 3].flatten.last", 3),
        ("[[1, [2]], 3].flatten(0)[1]", 3),
        ("[[1, [2]], 3].flatten(-1).length", 3),
        ("[1, [2, 3]].join(\",\").length", 5),
        ("[1, \"x\", nil].inspect.length", 13),
        ("{a: [1]}.inspect.length", 8),
    ]);
    invalid(&[
        "[1].join(1)",
        "[1].join(nil)",
        "[1].join(\",\", \"x\")",
        "[1].flatten(\"x\")",
        "[1].flatten(1, 2)",
        "[1].inspect(1)",
    ]);
    witnesses(vec![
        (
            "def run(xs: array<array<int>>) -> array<int>; xs.flatten; end",
            vec![Value::array(vec![ints(&[1]), ints(&[2, 3])])],
            ints(&[1, 2, 3]),
        ),
        (
            "def run(xs: array<string>, sep: string) -> string; xs.join(sep); end",
            vec![
                Value::array(vec![Value::bytes("a"), Value::bytes("b")]),
                Value::bytes("-"),
            ],
            Value::bytes("a-b"),
        ),
    ]);
    rescues(
        &[
            "def run(sep) -> int; begin; [1].join(sep); 0; rescue; 'bad'; end; end",
            "def run(n) -> int; begin; [[1]].flatten(n); 0; rescue; 'bad'; end; end",
        ],
        &["def run -> int; begin; ['a', 1, [nil]].join(','); 0; rescue; 'bad'; end; end"],
    );
    strict_arms(vec![(
        "def run(sep: string | int) -> string; [1].join(sep); end",
        Value::bytes(","),
    )]);
}
