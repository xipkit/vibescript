mod common;

use vibescript::{CallOptions, CheckReport, ErrorKind, Script, Value};

fn compile(source: &str) -> Script {
    common::gradual_engine()
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
        .any(|d| d.message.contains("Return value: expected int"))
}

fn ints(values: &[i64]) -> Value {
    Value::array(values.iter().copied().map(Value::int).collect())
}

fn int_items(value: &Value) -> Vec<i64> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_int().unwrap())
        .collect()
}

fn text(value: &str) -> Value {
    Value::bytes(value.as_bytes().to_vec())
}

#[test]
fn mixed_product_dimensions_keep_the_successful_arm_and_the_failure() {
    // The valid arm keeps the tail reachable, so the bad return is reported.
    let source = "def run(x: int | array<int>) -> int; [1].product(x); 'bad'; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    let error = script
        .call("run", &[ints(&[2])], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
    assert!(
        error.message.contains("return value for run expected int"),
        "{source}: {error}"
    );
    let error = script
        .call("run", &[Value::int(1)], CallOptions::default())
        .unwrap_err();
    assert_eq!(
        error.message, "array.product arguments must be arrays",
        "{source}"
    );

    // The failing arm makes the rescue reachable.
    let source =
        "def run(x: int | array<int>) -> int; begin; [1].product(x); 0; rescue; 'bad'; end; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    let value = script
        .call("run", &[ints(&[2])], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(0), "{source}");
    let error = script
        .call("run", &[Value::int(1)], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
    assert!(
        error.message.contains("return value for run expected int"),
        "{source}: {error}"
    );

    // Unions whose arms are all arrays stay clean.
    let source = "def run(x: array<int> | array<string>) -> array; [1].product(x); end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(report.is_clean(), "{source}: {report:?}");
    for input in [ints(&[2, 3]), Value::array(vec![text("a")])] {
        let expected = input.as_array().unwrap().len();
        let value = script
            .call("run", &[input], CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.as_array().unwrap().len(), expected, "{source}");
    }
}

#[test]
fn unknown_dimensions_do_not_hide_later_invalid_dimensions() {
    let source = "def run(x); [1].product(x, 'y'); end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    for input in [ints(&[2]), Value::int(2)] {
        let error = script
            .call("run", &[input], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
        assert_eq!(
            error.message, "array.product arguments must be arrays",
            "{source}"
        );
    }

    let source = "def run(x) -> array; [1].product([2], x); end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(report.is_clean(), "{source}: {report:?}");
    let value = script
        .call("run", &[ints(&[3, 4])], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_array().unwrap().len(), 2, "{source}");
    let error = script
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
}

#[test]
fn empty_receivers_validate_every_dimension_before_shortcutting() {
    let source = "def run -> int; begin; [].product('x'); 7; rescue; 0; end; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    assert!(!returns_bad_type(&report), "{source}: {report:?}");
    let value = script
        .call("run", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(0), "{source}");

    let source = "def run -> int; [1].product([], 2); 7; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    let error = script.call("run", &[], CallOptions::default()).unwrap_err();
    assert_eq!(
        error.message, "array.product arguments must be arrays",
        "{source}"
    );

    // The successful arm still produces the known empty product.
    let source = "def run(x: string | array<int>) -> int; if [].product(x) == []; 7; else; 'wrong'; end; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    assert!(!returns_bad_type(&report), "{source}: {report:?}");
    let value = script
        .call("run", &[ints(&[1])], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(7), "{source}");
    let error = script
        .call("run", &[text("x")], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
}

#[test]
fn numeric_counts_follow_the_runtime_conversions() {
    for (source, input, expected) in [
        (
            "def run(n: int) -> array; [1,2,3].rotate(n); end",
            Value::int(-1),
            Some(vec![3, 1, 2]),
        ),
        (
            "def run(n: float) -> array; [1,2,3].rotate(n); end",
            Value::float(1.5),
            Some(vec![2, 3, 1]),
        ),
        (
            "def run(n: int) -> array; [1,2,3].combination(n); end",
            Value::int(-1),
            Some(Vec::new()),
        ),
        (
            "def run(n: int) -> array; [].repeated_combination(n); end",
            Value::int(2),
            Some(Vec::new()),
        ),
        (
            "def run(n: int) -> int; [1,2,3].permutation(n).length; end",
            Value::int(5),
            None,
        ),
        (
            "def run(n: int) -> int; [1,2,3].sample(2.9).length + n; end",
            Value::int(0),
            None,
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let value = script
            .call("run", &[input], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        match expected {
            Some(items) => assert_eq!(int_items(&value), items, "{source}"),
            None => assert!(value.as_int().is_some(), "{source}: {value:?}"),
        }
    }
    for (source, expected) in [
        (
            "def run -> int; if [1,2].sample(-0.5) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [1,2].combination(-1.5) == []; 7; else; 'wrong'; end; end",
            7,
        ),
        (
            "def run -> int; if [1,2].repeated_permutation(0.9) == [[]]; 7; else; 'wrong'; end; end",
            7,
        ),
        ("def run -> int; [1,2,3].sample(2.9).length; end", 2),
        ("def run -> int; [1,2].permutation(5).length; end", 0),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let value = script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.as_int(), Some(expected), "{source}");
    }
    for (source, message) in [
        (
            "def run; [1].sample(-1.5); end",
            "array.sample count must be non-negative",
        ),
        (
            "def run; [1].rotate([1]); end",
            "array.rotate count must be integer",
        ),
        (
            "def run; [1].combination([2]); end",
            "array.combination length must be integer",
        ),
        (
            "def run; [1].repeated_permutation(nil); end",
            "array.repeated_permutation length must be integer",
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.message, message, "{source}");
    }
    // A count proven negative by a guard is a known failure.
    let source = "def run(n: int) -> int; if n < 0; [1].sample(n).length; else; 0; end; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
    let error = script
        .call("run", &[Value::int(-1)], CallOptions::default())
        .unwrap_err();
    assert_eq!(
        error.message, "array.sample count must be non-negative",
        "{source}"
    );
    let value = script
        .call("run", &[Value::int(3)], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(0), "{source}");
}

#[test]
fn possible_count_failures_keep_rescues_reachable_while_safe_counts_do_not() {
    for (source, ok, failing) in [
        (
            "def run(n: int) -> int; begin; [1,2].sample(n); 0; rescue; 'bad'; end; end",
            Value::int(1),
            Value::int(-1),
        ),
        (
            "def run(n: float) -> int; begin; [1,2].rotate(n); 0; rescue; 'bad'; end; end",
            Value::float(1.5),
            Value::float(1e300),
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(returns_bad_type(&report), "{source}: {report:?}");
        let value = script
            .call("run", &[ok], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .value;
        assert_eq!(value.as_int(), Some(0), "{source}");
        let error = script
            .call("run", &[failing], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
        assert!(
            error.message.contains("return value for run expected int"),
            "{source}: {error}"
        );
    }
    // An int annotation also permits big integers, so only these concrete
    // arguments establish a safe conversion.
    for (source, arguments) in [
        (
            "def run(n: int) -> int; begin; [1,2].rotate(n); 0; rescue; 'bad'; end; end",
            vec![Value::int(1)],
        ),
        (
            "def run(n: int) -> int; begin; [1,2].combination(n); 0; rescue; 'bad'; end; end",
            vec![Value::int(1)],
        ),
        (
            "def run(n: int) -> int; if n >= 0; begin; [1,2].sample(n); 0; rescue; 'bad'; end; else; 0; end; end",
            vec![Value::int(1)],
        ),
        (
            "def run(a: array<int>, b: array<int>) -> int; begin; a.product(b); 0; rescue; 'bad'; end; end",
            vec![ints(&[1]), ints(&[2])],
        ),
    ] {
        let script = compile(source);
        let report = script
            .check_call("run", &arguments, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        assert_eq!(
            script
                .call("run", &arguments, CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(0)
        );
    }
}

#[test]
fn clean_typed_returns_keep_element_facts() {
    for source in [
        "def run(a: array<int>, b: array<string>) -> array; a.product(b); end",
        "def run(a: array<int>) -> int; a.product([1], [2]).length; end",
        "def run(a: array<int>, n: int) -> int; a.repeated_combination(n).length; end",
        "def run(a: array<int>) -> int?; a.sample; end",
        "def run(a: array<int>) -> array<int>; a.shuffle; end",
        "def run(a: array<int>) -> array<int>; a.sample(2); end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.is_clean(), "{source}: {report:?}");
    }
    let source = "def run(a: array<int>) -> int?; a.sample; end";
    let script = compile(source);
    let value = script
        .call("run", &[ints(&[])], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.type_name(), "nil", "{source}");
    let value = script
        .call("run", &[ints(&[4])], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(4), "{source}");

    // A general array may be empty, so an unqualified sample is not an int.
    let source = "def run(a: array<int>) -> int; a.sample; end";
    let script = compile(source);
    let report = check(&script, source);
    assert!(returns_bad_type(&report), "{source}: {report:?}");
    let error = script
        .call("run", &[ints(&[])], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
}

#[test]
fn known_lengths_survive_reordering_and_sampling() {
    for expression in [
        "[1,2].rotate.sample",
        "[1,2].rotate(1.5).sample",
        "[1,2].shuffle.sample",
        "[1,2].sample(1).sample",
        "[1,2].sample(10).sample",
    ] {
        let source = format!("def run()->int;{expression};end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        assert!(matches!(result.value.as_int(), Some(1 | 2)));
    }
}

#[test]
fn gradual_arguments_keep_successful_container_results() {
    for (expression, argument) in [
        ("[1].product(x)", ints(&[2])),
        ("[1].rotate(x)", Value::int(1)),
        ("[1].sample(x)", Value::int(1)),
        ("[1].combination(x)", Value::int(1)),
        ("[1].permutation(x)", Value::int(1)),
        ("[1].repeated_combination(x)", Value::int(1)),
        ("[1].repeated_permutation(x)", Value::int(1)),
    ] {
        let source = format!("def run(x)->int;{expression};end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(returns_bad_type(&report), "{source}: {report:?}");
        let error = script
            .call("run", &[argument], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert!(error.message.contains("return value for run expected int"));
    }
}

#[test]
fn integer_contracts_keep_out_of_range_count_failures_reachable() {
    for method in [
        "rotate",
        "sample",
        "combination",
        "permutation",
        "repeated_combination",
        "repeated_permutation",
    ] {
        let source = format!(
            "def run(n:int)->int;begin;[1,2].{method}(n);0;rescue;'bad';end;end;def fail;run(10**30);end"
        );
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(returns_bad_type(&report), "{source}: {report:?}");
        let error = script
            .call("fail", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert!(error.message.contains("return value for run expected int"));
    }
}
