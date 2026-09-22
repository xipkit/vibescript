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
