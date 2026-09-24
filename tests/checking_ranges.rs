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

fn run(script: &Script, args: &[Value]) -> String {
    script
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|error| panic!("{error}"))
        .value
        .to_string()
}

#[test]
fn literal_range_members_fold_to_their_runtime_values() {
    for (expression, expected) in [
        ("(1..3).first", "1"),
        ("(1..3).last", "3"),
        ("(1...3).last", "3"),
        ("(1...1).first", "1"),
        ("(3..1).first", "3"),
        ("(1.5..3.9).last", "3"),
        ("(-1.9..2.2).first", "-1"),
        ("(1..).first", "1"),
        ("(..3).last", "3"),
        ("(1..3).size", "3"),
        ("(1...3).size", "2"),
        ("(3..1).size", "3"),
        ("(3...1).size", "2"),
        ("(1...1).size", "0"),
        ("(1.5..3.9).size", "3"),
        ("(1..3).first(2).length", "2"),
        ("(3..1).last(2).first", "2"),
        ("(3...1).first(10).last", "2"),
        ("(1..).first(3).last", "3"),
        ("(9223372036854775806..).first(9).length", "2"),
        ("(1...1).last(2).length", "0"),
        ("(1..3).first(0).length", "0"),
        ("(1..3).send(:first)", "1"),
        ("(1..3).public_send(:size)", "3"),
    ] {
        let source = format!("def run -> int; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        assert_eq!(run(&script, &[]), expected, "{source}");
    }
    for (expression, expected) in [
        ("(1..3).include?(2)", "true"),
        ("(1...3).include?(3)", "false"),
        ("(3..1).cover?(2)", "true"),
        ("(3...1).member?(1)", "false"),
        ("(1..3).include?(2.5)", "true"),
        ("(1.5..3.9).include?(3.5)", "false"),
        ("(1..3).include?('2')", "false"),
        ("(1..3).member?(nil)", "false"),
        ("(1..3).exclude_end?", "false"),
        ("(1...3).exclude_end?", "true"),
    ] {
        // The folded boolean leaves the other branch unreachable.
        let (yes, no) = if expected == "true" {
            ("1", "'bad'")
        } else {
            ("'bad'", "1")
        };
        let source = format!("def run -> int; {expression} ? {yes} : {no}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        assert_eq!(run(&script, &[]), "1", "{source}");
        let source = format!("def run; {expression}; end");
        assert_eq!(run(&compile(&source), &[]), expected, "{source}");
    }
}

#[test]
fn big_integer_membership_stays_gradual_and_clean() {
    // Big integer literals have no exact fact, so membership stays a boolean.
    for (expression, expected) in [
        ("(1..).include?(9223372036854775808)", "true"),
        ("(..3).cover?(-9223372036854775809)", "true"),
        ("(1..3).member?(9223372036854775808)", "false"),
    ] {
        let source = format!("def run -> bool; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        assert_eq!(run(&script, &[]), expected, "{source}");
    }
}

#[test]
fn known_range_failures_are_diagnosed_and_fail_at_runtime() {
    for expression in [
        "(1..).last",
        "(..3).first",
        "(1..).last(2)",
        "(..3).first(2)",
        "(..3).last(2)",
        "(1..3).first(-1)",
        "(1..3).first(1.5)",
        "(1..3).first(2.0)",
        "(1..3).last('x')",
        "(1..3).first(nil)",
        "(1..3).first(1, 2)",
        "(1..3).length(1)",
        "(1..).size",
        "(..3).size",
        "(-9223372036854775808..9223372036854775807).size",
        "(1..3).size(1)",
        "(1..3).include?",
        "(1..3).cover?(1, 2)",
        "(1..3).exclude_end?(1)",
        "(1..3).first(k: 1)",
        "(1..3).include?(2, k: 1)",
        "(1..3).send(:size, k: 1)",
    ] {
        let source = format!("def run; {expression}; end");
        let script = compile(&source);
        let report = check(&script, &source);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("does not accept")),
            "{source}: {report:?}"
        );
        assert!(
            script.call("run", &[], CallOptions::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn attached_blocks_are_ignored_like_the_runtime() {
    for source in [
        "def run -> int; n = 0; (1..3).first { n = 'bad' }; n; end",
        "def run -> int; n = 0; (1..3).size { raise 'called' }; n; end",
        "def run -> int; (1..3).include?(2) { 'bad' } ? 1 : 'bad'; end",
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        script
            .call("run", &[], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

#[test]
fn general_ranges_keep_results_and_possible_open_range_failures() {
    let bounded = Value::range(Some(1), Some(3), false);
    let beginless = Value::range(None, Some(3), false);
    let endless = Value::range(Some(1), None, false);
    for (source, arg, expected) in [
        ("def run(r: range) -> int; r.first; end", &bounded, "1"),
        ("def run(r: range) -> int; r.last; end", &beginless, "3"),
        ("def run(r: range) -> int; r.size; end", &bounded, "3"),
        (
            "def run(r: range) -> array<int>; r.first(2); end",
            &endless,
            "[1, 2]",
        ),
        (
            "def run(r: range) -> bool; r.include?(2); end",
            &beginless,
            "true",
        ),
        (
            "def run(r: range) -> bool; r.exclude_end?; end",
            &bounded,
            "false",
        ),
    ] {
        let script = compile(source);
        let report = check(&script, source);
        assert!(report.diagnostics.is_empty(), "{source}: {report:?}");
        let report = script
            .check_call("run", std::slice::from_ref(arg), &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        assert_eq!(
            run(&script, std::slice::from_ref(arg)),
            expected,
            "{source}"
        );
    }
    // A general range may be open, so its failures stay reachable; the
    // runtime reaches each rescue with the witness input.
    for (source, arg) in [
        (
            "def run(r: range) -> int; begin; r.first; 0; rescue; 'bad'; end; end",
            &beginless,
        ),
        (
            "def run(r: range) -> int; begin; r.last(2); 0; rescue; 'bad'; end; end",
            &endless,
        ),
        (
            "def run(r: range) -> int; begin; r.size; 0; rescue; 'bad'; end; end",
            &endless,
        ),
        (
            "def run(r: range, n) -> int; begin; (1..3).first(n); 0; rescue; 'bad'; end; end",
            &bounded,
        ),
    ] {
        let script = compile(source);
        assert!(returns_bad_type(&check(&script, source)), "{source}");
        let mut args = vec![arg.clone()];
        if source.contains(", n)") {
            args.push(Value::int(-1));
        }
        let error = script
            .call("run", &args, CallOptions::default())
            .unwrap_err();
        assert!(error.message.ends_with("got string"), "{source}: {error}");
    }
    // Dynamic endpoints still produce integer arrays and never an open range.
    let source = "def run(n: int) -> array<int>; (1..n).first(2) + (n..1).last(n); end";
    let script = compile(source);
    assert!(check(&script, source).diagnostics.is_empty());
    assert_eq!(run(&script, &[Value::int(2)]), "[1, 2, 2, 1]");
}

#[test]
fn materializing_huge_ranges_keeps_the_limit_error() {
    let source = "def run -> int; begin; (-9223372036854775808..9223372036854775807).to_a; 0; rescue LimitError; 'bad'; end; end";
    let script = compile(source);
    assert!(returns_bad_type(&check(&script, source)), "{source}");
    let error = script.call("run", &[], CallOptions::default()).unwrap_err();
    assert!(error.message.ends_with("got string"), "{error}");
    let source = "def run -> int; begin; (1..3).to_a; 0; rescue LimitError; 'bad'; end; end";
    let script = compile(source);
    assert!(check(&script, source).diagnostics.is_empty(), "{source}");
}

#[test]
fn known_invalid_union_alternatives_are_still_reported() {
    let range = Value::range(Some(1), Some(3), false);
    // Strings have no `first`; the range alternative alone succeeds.
    let source = "def run(x: range | string) -> int; x.first; end";
    let script = compile(source);
    assert!(!check(&script, source).diagnostics.is_empty(), "{source}");
    assert_eq!(run(&script, std::slice::from_ref(&range)), "1");
    let source = "def run(x: range | array<int>) -> int; x.size; end";
    let script = compile(source);
    assert!(check(&script, source).diagnostics.is_empty(), "{source}");
    assert_eq!(run(&script, &[range]), "3");
}
