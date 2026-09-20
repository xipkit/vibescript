use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value};

fn compile(source: &str) -> vibescript::Script {
    Engine::new().compile(source).unwrap()
}

#[test]
fn range_operands_concatenate_with_strings_on_either_side() {
    for (expression, expected) in [
        ("\"a\" + (1..2)", "a1..2"),
        ("(1...3) + \"b\"", "1...3b"),
        ("\"a\" + (1...3)", "a1...3"),
        ("(1..2) + \"b\"", "1..2b"),
        ("\"d\" + (5..1)", "d5..1"),
        ("(5...1) + \"d\"", "5...1d"),
        ("\"n\" + (-3..-1)", "n-3..-1"),
        ("\"b\" + (..5)", "b..5"),
        ("(...5) + \"b\"", "...5b"),
        ("\"e\" + (1..)", "e1.."),
        ("(1...) + \"e\"", "1...e"),
        (
            "\"m\" + (9223372036854775807..9223372036854775807)",
            "m9223372036854775807..9223372036854775807",
        ),
        (
            "(-9223372036854775808..-9223372036854775808) + \"m\"",
            "-9223372036854775808..-9223372036854775808m",
        ),
        (
            "\"s\" + (-9223372036854775808...9223372036854775807)",
            "s-9223372036854775808...9223372036854775807",
        ),
        ("\"<\" + (1..2) + \">\"", "<1..2>"),
        ("(1..2) + \"|\" + (3...4)", "1..2|3...4"),
    ] {
        let source = format!("def run -> string; {expression}; end");
        let script = compile(&source);
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            result.value.as_bytes(),
            Some(expected.as_bytes()),
            "{expression}"
        );
        assert!(
            result.stats.retained_memory_bytes >= expected.len(),
            "{expression}"
        );
    }
}

#[test]
fn typed_range_and_string_declarations_check_and_execute_consistently() {
    let script = compile(
        "def wrap(r:range, s:string) -> string; s + r + s; end\n\
         def lead(r:range, s:string) -> string; r + s; end\n\
         def run -> string; wrap((1...3), \"b\") + lead((..5), \"|\"); end",
    );
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(result.value.as_bytes(), Some(&b"b1...3b..5|"[..]));

    for (source, range, expected) in [
        (
            "def run(r:range, s:string) -> string; s + r; end",
            Value::range(Some(1), Some(2), false),
            "x1..2",
        ),
        (
            "def run(r:range, s:string) -> string; r + s; end",
            Value::range(Some(1), Some(3), true),
            "1...3x",
        ),
        (
            "def run(r:range, s:string) -> string; r + s; end",
            Value::range(None, Some(i64::MIN), true),
            "...-9223372036854775808x",
        ),
        (
            "def run(r:range, s:string) -> string; s + r; end",
            Value::range(Some(i64::MAX), None, false),
            "x9223372036854775807..",
        ),
    ] {
        let script = compile(source);
        let general = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        assert!(general.is_clean(), "{source}: {general:?}");
        let args = [range, Value::bytes(b"x".to_vec())];
        let report = script
            .check_call("run", &args, &CallOptions::default())
            .unwrap();
        assert!(report.is_clean(), "{source}: {report:?}");
        let result = script.call("run", &args, CallOptions::default()).unwrap();
        assert_eq!(
            result.value.as_bytes(),
            Some(expected.as_bytes()),
            "{source}"
        );
    }
}

#[test]
fn ranges_do_not_widen_the_other_rejected_operand_forms() {
    for expression in [
        "\"a\" + nil",
        "\"a\" + [1]",
        "\"a\" + {}",
        "(1..2) + (3..4)",
        "(1..2) + 1",
        "(1..2) + :s",
    ] {
        let source = format!("def run -> string; {expression}; end");
        let script = compile(&source);
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        assert!(!report.is_clean(), "{source}: {report:?}");
        let error = script.call("run", &[], CallOptions::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{expression}");
        assert_eq!(error.message, "unsupported operand types", "{expression}");
    }
    let error = compile("class Plain; end\ndef run; \"a\" + Plain.new; end")
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(error.message, "unsupported operand types");
    // `sum` keeps its own compatibility guard even though `"a" + (1..2)` is
    // now a valid binary expression.
    for expression in ["[\"a\", (1..2)].sum(\"\")", "[(1..2)].sum(\"\")"] {
        let error = compile(&format!("def run; {expression}; end"))
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{expression}");
        assert_eq!(
            error.message, "sum cannot add incompatible values",
            "{expression}"
        );
    }
}

#[test]
fn range_concatenation_stays_metered_and_cancellable() {
    let source = "def run(n:int) -> int\n\
         out = []\n\
         i = 0\n\
         while i < n\n\
           out << (\"x\" + (i...9223372036854775807))\n\
           i += 1\n\
         end\n\
         out.size\n\
         end";
    let script = compile(source);
    let report = script
        .check_call("run", &[Value::int(1000)], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    let result = script
        .call("run", &[Value::int(1000)], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1000));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    let error = script
        .call(
            "run",
            &[Value::int(1000)],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(8192),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);

    let token = CancellationToken::new();
    token.cancel();
    let error = script
        .call(
            "run",
            &[Value::int(1)],
            CallOptions {
                cancellation: token,
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}
