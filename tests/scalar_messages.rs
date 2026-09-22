//! Runtime error messages that scripts and hosts observe, checked against Go v0.70.0's wording.
use vibescript::{CallOptions, Engine, ErrorClass, Limits, Value};

fn fail(source: &str, args: &[Value], limits: Limits) -> vibescript::Error {
    let script = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let options = CallOptions {
        limits,
        ..CallOptions::default()
    };
    match script.call("run", args, options) {
        Ok(outcome) => panic!("{source}: returned {:?}", outcome.value),
        Err(error) => error,
    }
}

/// Asserts the message and class of each script's failure when calling `run`.
fn rejects(cases: &[(&str, ErrorClass, &str)]) {
    for (source, class, message) in cases {
        limited(source, &[], Limits::default(), *class, message);
    }
}

fn limited(source: &str, args: &[Value], limits: Limits, class: ErrorClass, message: &str) {
    let error = fail(source, args, limits);
    assert_eq!(error.message, message, "{source}");
    assert_eq!(error.class(), Some(class), "{source}");
}

#[test]
fn guard_limits_name_the_configured_limit() {
    let steps = Limits {
        steps: Some(50),
        ..Limits::default()
    };
    limited(
        "def run()\n  begin\n    while true\n    end\n  rescue => e\n    e.message\n  end\nend",
        &[],
        steps,
        ErrorClass::Limit,
        "step quota exceeded (50)",
    );
    let memory = Limits {
        memory_bytes: Some(8192),
        ..Limits::default()
    };
    limited(
        "def run()\n  \"x\" * 1000000\nend",
        &[],
        memory.clone(),
        ErrorClass::Limit,
        "memory quota exceeded (8192 bytes)",
    );
    limited(
        "def run(s)\n  s.length\nend",
        &[Value::bytes(vec![b'a'; 20_000])],
        memory,
        ErrorClass::Limit,
        "check memory after binding call env: memory quota exceeded (8192 bytes)",
    );
    let recursion = Limits {
        recursion: 4,
        ..Limits::default()
    };
    limited(
        "def f(n)\n  f(n + 1)\nend\ndef run()\n  f(0)\nend",
        &[],
        recursion,
        ErrorClass::Limit,
        "recursion depth exceeded (limit 4)",
    );
}

#[test]
fn operators_name_the_operation_they_refuse() {
    use ErrorClass::{Argument, Limit, Runtime, ZeroDivision};
    rejects(&[
        (
            "def run\n  \"a\" + nil\nend",
            Runtime,
            "unsupported addition operands",
        ),
        (
            "def run\n  (1..2) + 1\nend",
            Runtime,
            "unsupported addition operands",
        ),
        (
            "def run\n  [1] - 1\nend",
            Runtime,
            "unsupported subtraction operands",
        ),
        (
            "def run\n  1.second - Time.now\nend",
            Runtime,
            "unsupported subtraction operands",
        ),
        (
            "def run\n  Time.now * 2\nend",
            Runtime,
            "unsupported multiplication operands",
        ),
        (
            "def run\n  \"ab\" * (2**70)\nend",
            Runtime,
            "unsupported multiplication operands",
        ),
        (
            "def run\n  money(\"1.00 USD\") * 1.5\nend",
            Runtime,
            "unsupported multiplication operands",
        ),
        (
            "def run\n  nil / 2\nend",
            Runtime,
            "unsupported division operands",
        ),
        (
            "def run\n  1.5 % 2.0\nend",
            Runtime,
            "unsupported modulo operands",
        ),
        (
            "def run\n  1.second % 2\nend",
            Runtime,
            "unsupported modulo operands",
        ),
        (
            "def run\n  1.second ** 2\nend",
            Runtime,
            "unsupported exponentiation operands",
        ),
        (
            "def run\n  x = 1\n  x << 2\nend",
            Runtime,
            "unsupported shovel operands",
        ),
        (
            "def run\n  1 << 2\nend",
            Runtime,
            "unsupported shovel operands",
        ),
        (
            "def run\n  [1] & 1\nend",
            Runtime,
            "unsupported intersection operands",
        ),
        (
            "def run\n  1 < \"a\"\nend",
            Argument,
            "unsupported comparison operands",
        ),
        (
            "def run\n  -\"a\"\nend",
            Runtime,
            "unsupported unary - operand",
        ),
        (
            "def run\n  +nil\nend",
            Runtime,
            "unsupported unary + operand",
        ),
        (
            "def run\n  money(\"1.00 USD\") < money(\"1.00 EUR\")\nend",
            Argument,
            "money currency mismatch for comparison",
        ),
        ("def run\n  1 % 0\nend", ZeroDivision, "modulo by zero"),
        (
            "def run\n  (2**70) % 0\nend",
            ZeroDivision,
            "modulo by zero",
        ),
        (
            "def run\n  1.second % 0.seconds\nend",
            ZeroDivision,
            "modulo by zero",
        ),
        ("def run\n  1 / 0\nend", ZeroDivision, "division by zero"),
        (
            "def run\n  \"ab\" * -1.5\nend",
            Runtime,
            "negative argument for string repetition",
        ),
        (
            "def run\n  \"ab\" * -(2**70)\nend",
            Runtime,
            "negative argument for string repetition",
        ),
        (
            "def run\n  2 ** (2**70)\nend",
            Limit,
            "integer exponentiation exponent is too large",
        ),
    ]);
}

#[test]
fn temporal_arithmetic_names_the_overflowing_operation() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run\n  Duration.build(9223372036854775807) + 1.second\nend",
            Runtime,
            "duration addition result out of int64 range",
        ),
        (
            "def run\n  Duration.build(-9223372036854775807) - 2.seconds\nend",
            Runtime,
            "duration subtraction result out of int64 range",
        ),
        (
            "def run\n  1.hour * 1e300\nend",
            Runtime,
            "duration multiplication result out of int64 range",
        ),
        (
            "def run\n  1.hour * (2**70)\nend",
            Runtime,
            "duration multiplication result out of int64 range",
        ),
        (
            "def run\n  1.hour / 1e-300\nend",
            Runtime,
            "duration division result out of int64 range",
        ),
        (
            "def run\n  1.hour * (0.0/0.0)\nend",
            Runtime,
            "cannot convert NaN to integer",
        ),
        (
            "def run\n  1.hour * (1.0/0)\nend",
            Runtime,
            "cannot convert Infinity to integer",
        ),
        (
            "def run\n  1.second + 1e19\nend",
            Runtime,
            "float 1e+19 is out of integer range",
        ),
        (
            "def run\n  Time.at(0) + (2**70)\nend",
            Runtime,
            "time addition result out of int64 range",
        ),
        (
            "def run\n  Time.at(0) + (0.0/0.0)\nend",
            Runtime,
            "time addition result out of int64 range",
        ),
        (
            "def run\n  Time.at(0) - 9223372036854775807\nend",
            Runtime,
            "time subtraction result out of int64 range",
        ),
        (
            "def run\n  (2**70).seconds\nend",
            Runtime,
            "int.seconds result out of int64 range",
        ),
    ]);
}

#[test]
fn duration_parsing_and_members_use_go_wording() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run\n  Duration.parse(\"\")\nend",
            Runtime,
            "empty duration string",
        ),
        (
            "def run\n  Duration.parse(\"P\")\nend",
            Runtime,
            "invalid duration format",
        ),
        (
            "def run\n  Duration.parse(\"PT1S30M\")\nend",
            Runtime,
            "invalid duration format",
        ),
        (
            "def run\n  Duration.parse(\"1.5s\")\nend",
            Runtime,
            "duration must be whole seconds",
        ),
        (
            "def run\n  Duration.parse(\"P1W2D\")\nend",
            Runtime,
            "invalid mixed week duration",
        ),
        (
            "def run\n  Duration.parse(\"PW\")\nend",
            Runtime,
            "invalid week duration format",
        ),
        (
            "def run\n  Duration.parse(\"P+W\")\nend",
            Runtime,
            "invalid week duration",
        ),
        (
            "def run\n  Duration.parse(\"PT99999999999999999999S\")\nend",
            Runtime,
            "invalid duration number",
        ),
        (
            "def run\n  Duration.parse(\"PT99999999999999999999X\")\nend",
            Runtime,
            "invalid duration format",
        ),
        (
            "def run\n  Duration.parse(1)\nend",
            Runtime,
            "Duration.parse expects a duration string",
        ),
        (
            "def run\n  Duration.build(1, hours: 2)\nend",
            Runtime,
            "Duration.build accepts either seconds or named parts, not both",
        ),
        (
            "def run\n  Duration.build(hours: nil, bogus: 2)\nend",
            Runtime,
            "Duration.build unknown part \"bogus\"",
        ),
        (
            "def run\n  Duration.build(hours: 0.0/0.0)\nend",
            Runtime,
            "Duration.build hours: cannot convert NaN to integer",
        ),
        (
            "def run\n  Duration.build(nil)\nend",
            Runtime,
            "duration expects numeric seconds",
        ),
        (
            "def run\n  Duration.build(2**70)\nend",
            Runtime,
            "integer must fit in a 64-bit integer",
        ),
        (
            "def run\n  1.hour.to_s(1)\nend",
            Runtime,
            "duration.to_s does not take arguments",
        ),
        (
            "def run\n  1.hour.inspect(k: 1)\nend",
            Runtime,
            "duration.inspect does not take keyword arguments",
        ),
        (
            "def run\n  1.hour.string { 1 }\nend",
            Runtime,
            "duration.string does not take a block",
        ),
        (
            "def run\n  1.hour.between?(1)\nend",
            Runtime,
            "duration.between? expects min and max",
        ),
        (
            "def run\n  1.hour.between?(1, 2)\nend",
            Runtime,
            "unsupported comparison operands",
        ),
        (
            "def run\n  1.hour.ago(x: 1)\nend",
            Runtime,
            "duration.before does not accept keyword arguments",
        ),
        (
            "def run\n  1.hour.ago(1, 2)\nend",
            Runtime,
            "before expects at most one time argument",
        ),
        (
            "def run\n  1.hour.since(1)\nend",
            Runtime,
            "after expects a Time or RFC3339 string",
        ),
    ]);
}
