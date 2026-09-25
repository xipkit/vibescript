//! `//` floor division (ADR-008): integers floor at any size, a float operand
//! gives a floored float, and `/` on two ints is refused until it divides.

mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorClass, ErrorKind, Value};

fn run(body: &str) -> Value {
    let source = format!("def run -> any\n  {body}\nend\n");
    Engine::new()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .value
}

fn failure(body: &str) -> vibescript::Error {
    let source = format!("def run\n  {body}\nend\n");
    Engine::new()
        .compile(&source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", &[], CallOptions::default())
        .unwrap_err()
}

#[test]
fn integers_floor_at_any_size() {
    for (expression, expected) in [
        ("7 // 2", "3"),
        ("-7 // 2", "-4"),
        ("7 // -2", "-4"),
        ("-7 // -2", "3"),
        ("6 // 3", "2"),
        ("0 // 5", "0"),
        ("(2 ** 70) // 3", "393530540239137101141"),
        ("-(2 ** 70) // 3", "-393530540239137101142"),
        ("(2 ** 70) // (2 ** 69)", "2"),
        ("-9223372036854775808 // -1", "9223372036854775808"),
    ] {
        let value = run(expression);
        assert_eq!(value.type_name(), "int", "{expression}");
        assert_eq!(value.to_string(), expected, "{expression}");
    }
}

#[test]
fn a_float_operand_gives_the_floored_float() {
    for (expression, expected) in [
        ("7.5 // 2", 3.0),
        ("7 // 2.0", 3.0),
        ("-7 // 2.0", -4.0),
        ("-7.5 // 2", -4.0),
        ("7.5 // -2.5", -3.0),
        ("(2 ** 70) // 2.0", 590295810358705651712.0),
        // Like float `/`, a zero divisor gives an infinity rather than an error.
        ("1.0 // 0", f64::INFINITY),
        ("-1 // 0.0", f64::NEG_INFINITY),
    ] {
        let value = run(expression);
        assert_eq!(value.type_name(), "float", "{expression}");
        assert_eq!(value.as_float(), Some(expected), "{expression}");
    }
    assert!(run("0.0 // 0").as_float().unwrap().is_nan());
}

#[test]
fn integer_slash_division_is_refused_until_it_divides() {
    // Until the switchover `/` still floors two ints, so the compiler asks
    // for `//` where both operands are ints; a float operand divides.
    let source = "def run -> array<number>\n  [7 / 2, -7 / 2, 7.0 / 2]\nend\n";
    let error = common::static_engine().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0109", "V0109"]);
    let spans: Vec<usize> = error.diagnostics().iter().map(|d| d.span.start).collect();
    assert_eq!(
        spans,
        [
            source.find("7 / 2").unwrap() + 2,
            source.find("-7 / 2").unwrap() + 3
        ]
    );
    assert_eq!(run("[7.0 / 2, 7 / 2.0]").to_string(), "[3.5, 3.5]");
}

#[test]
fn an_integer_zero_divisor_raises_as_division_does() {
    for expression in ["7 // 0", "(2 ** 70) // 0", "-3 // 0"] {
        let error = failure(expression);
        assert_eq!(error.message, "division by zero", "{expression}");
        assert_eq!(error.kind, ErrorKind::Arithmetic, "{expression}");
        assert_eq!(
            error.class(),
            Some(ErrorClass::ZeroDivision),
            "{expression}"
        );
    }
    assert_eq!(
        run("begin\n    7 // 0\n  rescue ZeroDivisionError => error\n    error.message\n  end")
            .to_string(),
        "division by zero"
    );
}

#[test]
fn other_operands_are_refused_at_compile_time() {
    for (expression, code) in [
        ("money(\"1.00 USD\") // 2", "V0108"),
        ("30.minutes // 2", "V0108"),
        ("30.minutes // 10.minutes", "V0108"),
        ("Time.at(0) // 2", "V0108"),
        ("\"a\" // 2", "V0108"),
        ("2 // \"a\"", "V0108"),
        ("nil // 2", "V0107"),
        ("[4] // 2", "V0108"),
        ("{ a: 1 } // 2", "V0108"),
    ] {
        let source = format!("def run\n  {expression}\nend\n");
        let error = common::static_engine().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{expression}");
        // An operand that cannot divide is reported at the operator, and a
        // nil operand where it is read.
        let at = match code {
            "V0108" => source.find(" // ").unwrap() + 1,
            _ => source.find("nil").unwrap(),
        };
        assert_eq!(error.diagnostics()[0].span.start, at, "{expression}");
    }
}

#[test]
fn slashes_after_an_operand_lex_as_floor_division() {
    for (body, expected) in [
        ("a = 7\n  b = 2\n  a // b", "3"),
        ("a = 7\n  a//2", "3"),
        ("x = 10 // 3\n  x", "3"),
        ("a = 9\n  a //2", "4"),
        ("[1, 2, 3].length // 2", "1"),
        ("[7, 8].map { |v| v // 2 }", "[3, 4]"),
        ("total = 9\n  total //\n    2", "4"),
        ("x = [1,\n    2].length // 2\n  x", "1"),
    ] {
        assert_eq!(run(body).to_string(), expected, "{body}");
    }
}

#[test]
fn slashes_that_start_an_operand_still_lex_as_regexes() {
    for (body, expected) in [
        ("x = 7 // 2\n  \"a/b\".sub(/\\//, \"-\") + x.to_s", "a-b3"),
        ("\"ab\" =~ /b/", "1"),
        ("x = //\n  x.source", ""),
        ("//.source", ""),
        ("[//, 7 // 2].length", "2"),
        ("[3 // 2, /a/.source]", "[1, a]"),
    ] {
        assert_eq!(run(body).to_string(), expected, "{body}");
    }
    assert_eq!(run("//").type_name(), "regex");
}

#[test]
fn command_arguments_may_still_start_with_an_empty_regex() {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = written.clone();
    let mut engine = Engine::new();
    engine.set_output_writer(move |_, bytes| {
        sink.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    });
    let script = engine
        .compile("def run\n  puts //\n  p //, 1\n  puts 7 // 2\nend\n")
        .unwrap();
    script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        String::from_utf8(written.lock().unwrap().clone()).unwrap(),
        "//\n//\n1\n3\n"
    );
}

#[test]
fn floor_division_binds_like_multiplication() {
    for (expression, expected) in [
        ("1 + 7 // 2", "4"),
        ("7 // 2 * 3", "9"),
        ("2 * 7 // 3", "4"),
        ("7 // 2 ** 2", "1"),
        ("-7 // 2", "-4"),
        ("(1 + 7) // 2", "4"),
        ("7 // 2 % 2", "1"),
        ("7 // 2 == 3", "true"),
    ] {
        assert_eq!(run(expression).to_string(), expected, "{expression}");
    }
}

#[test]
fn floor_division_errors_point_at_the_operator() {
    // The operator's first slash, not its second.
    for (body, column) in [("x = 7\n  x // 0", 5), ("x = 0\n  1 + 8 // x", 9)] {
        let error = failure(body);
        let position = &error.diagnostic.as_ref().unwrap().position;
        assert_eq!((position.line, position.column), (3, column), "{body}");
    }
    let source = "def run -> int\n  7 // \"a\"\nend\n";
    let checked = Engine::new().type_check(source).unwrap();
    let span = checked.diagnostics[0].span;
    assert_eq!(&source[span.start..span.end], "//");
}

#[test]
fn the_checker_types_floor_division() {
    let check = |source: &str| {
        common::gradual_engine()
            .compile(source)
            .unwrap()
            .check(&CallOptions::default())
            .unwrap()
    };
    for source in [
        "def half(n: int) -> int\n  n // 2\nend\n",
        "def ratio(a: float, b: int) -> float\n  a // b\nend\n",
        "def mixed(a: int, b: float) -> float\n  a // b\nend\n",
        "def big -> int\n  (2 ** 70) // 3\nend\n",
    ] {
        let report = check(source);
        assert!(report.is_clean(), "{source}: {report:?}");
    }
    let report = check("def ratio(a: float) -> int\n  a // 2\nend\n");
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert!(
        report.diagnostics[0].message.contains("got float"),
        "{}",
        report.diagnostics[0].message
    );
    let report = check("def bad(d: duration)\n  d // 2\nend\n");
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    assert_eq!(
        report.diagnostics[0].message,
        "Operator \"//\" does not accept duration and int"
    );
}
