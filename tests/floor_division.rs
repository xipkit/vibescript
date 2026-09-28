//! Division (ADR-008): `/` divides numbers to a float, even two ints of any
//! size, and `//` floors: integers floor at any size, and a float operand
//! gives a floored float.

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
fn slash_divides_integers_to_the_nearest_float() {
    for (expression, expected) in [
        ("7 / 2", 3.5),
        ("-7 / 2", -3.5),
        ("7 / -2", -3.5),
        ("6 / 3", 2.0),
        ("1 / 3", 1.0 / 3.0),
        ("7.0 / 2", 3.5),
        ("7 / 2.0", 3.5),
        // Past 2^53 the quotient is rounded once, from the exact value.
        ("9007199254740993 / 1", 9007199254740992.0),
        ("(2 ** 70) / 3", 2f64.powi(70) / 3.0),
        ("(10 ** 20) / 3", 33333333333333332000.0),
        ("9223372036854775807 / 7", 1317624576693539300.0),
        ("(10 ** 400) / (10 ** 399)", 10.0),
        ("-(10 ** 400) / (10 ** 399)", -10.0),
        ("(2 ** 1024 - 2 ** 971) / 1", f64::MAX),
        // Quotients below the float range round to a subnormal or to zero.
        ("1 / (2 ** 1074)", 5e-324),
        ("3 / (2 ** 1076)", 5e-324),
        ("1 / (2 ** 1075)", 0.0),
        ("1 / (10 ** 400)", 0.0),
    ] {
        let value = run(expression);
        assert_eq!(value.type_name(), "float", "{expression}");
        assert_eq!(value.as_float(), Some(expected), "{expression}");
    }
    // The sign of a zero quotient follows the operands'.
    assert!(run("0 / -5").as_float().unwrap().is_sign_negative());
    assert!(
        run("-1 / (10 ** 400)")
            .as_float()
            .unwrap()
            .is_sign_negative()
    );
}

#[test]
fn slash_raises_for_zero_divisors_and_quotients_beyond_the_float_range() {
    for expression in ["7 / 0", "(2 ** 70) / 0", "0 / 0"] {
        let error = failure(expression);
        assert_eq!(error.message, "division by zero", "{expression}");
        assert_eq!(
            error.class(),
            Some(ErrorClass::ZeroDivision),
            "{expression}"
        );
    }
    // A float operand keeps float division's infinities.
    assert_eq!(run("1.0 / 0").as_float(), Some(f64::INFINITY));
    for expression in [
        "(10 ** 400) / 1",
        "-(10 ** 400) / 3",
        "(2 ** 1024) / 1",
        // Rounds up to 2^1024, one past the largest float.
        "(2 ** 1024 - 2 ** 970) / 1",
    ] {
        let error = failure(expression);
        assert_eq!(error.kind, ErrorKind::Arithmetic, "{expression}");
        assert_eq!(
            error.message, "integer division result is out of float range",
            "{expression}"
        );
    }
}

#[test]
fn slash_has_a_float_type_and_money_and_durations_keep_their_own() {
    let error = Engine::new()
        .compile("def run -> int\n  7 / 2\nend\n")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    // `/=` on an int local would change its type.
    let error = Engine::new()
        .compile("def run -> int\n  a = 12\n  a /= 5\n  a\nend\n")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0102"]);
    assert_eq!(run("b = 12.0\n  b /= 5\n  b").as_float(), Some(2.4));
    assert_eq!(run("money(\"10.00 USD\") / 4").to_string(), "2.50 USD");
    assert_eq!(run("30.minutes / 2").to_string(), "900s");
    assert_eq!(run("30.minutes / 10.minutes").as_float(), Some(3.0));
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
        let error = vibescript::Engine::new().compile(&source).err().unwrap();
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
fn compound_floor_division_assigns_the_floored_value() {
    for (body, expected) in [
        ("a = 7\n  a //= 2\n  a", "3"),
        ("a = -7\n  a //= 2\n  a", "-4"),
        ("a = 2 ** 70\n  a //= 3\n  a", "393530540239137101141"),
        ("b = 7.5\n  b //= 2\n  b", "3"),
        ("h = { n: 9 }\n  h[\"n\"] //= 4\n  h[\"n\"]", "2"),
    ] {
        assert_eq!(run(body).to_string(), expected, "{body}");
    }
    let source = "class Counter\n  @@n: int = 9\n  def self.halve -> int\n    @@n //= 2\n    @@n\n  end\nend\n\
                  def run -> int\n  Counter.halve\nend\n";
    let engine = Engine::new();
    let value = engine
        .compile(source)
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(4));
    // A zero divisor raises as `//` does.
    let error = failure("y = 1\n  y //= 0");
    assert_eq!(error.kind, ErrorKind::Arithmetic);
    assert_eq!(error.class(), Some(ErrorClass::ZeroDivision));
}

#[test]
fn the_checker_types_compound_floor_division() {
    let checked = |source: &str| -> Vec<String> {
        match vibescript::Engine::new().compile(source) {
            Ok(_) => Vec::new(),
            Err(error) => common::codes(&error),
        }
    };
    assert!(checked("def f(n: int) -> int\n  n //= 2\n  n\nend\n").is_empty());
    assert!(checked("def f(x: float) -> float\n  x //= 2\n  x\nend\n").is_empty());
    assert_eq!(
        checked("def f(s: string) -> string\n  s //= 2\n  s\nend\n"),
        ["V0108"]
    );
    // The result keeps the target's type: floor division of an int by a
    // float is a float.
    assert_eq!(
        checked("def f(n: int) -> int\n  n //= 2.0\n  n\nend\n"),
        ["V0102"]
    );
}

#[test]
fn float_modulo_has_the_divisors_sign() {
    for (expression, expected) in [
        ("7.5 % 2.0", 1.5),
        ("-7.5 % 2.0", 0.5),
        ("7.5 % -2.0", -0.5),
        ("-7.5 % -2.0", -1.5),
        ("7.5 % 2", 1.5),
        ("-7 % 2.5", 0.5),
        ("(2 ** 70) % 3.0", 1.0),
    ] {
        let value = run(expression);
        assert_eq!(value.type_name(), "float", "{expression}");
        assert_eq!(value.as_float(), Some(expected), "{expression}");
    }
    assert!(run("1.0 % 0.0").as_float().unwrap().is_nan());
    assert!(
        vibescript::Engine::new()
            .compile("x: float = 7.5 % 2\n")
            .is_ok()
    );
    let error = vibescript::Engine::new()
        .compile("x: int = 7.5 % 2\n")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
}
