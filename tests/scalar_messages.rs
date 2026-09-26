//! Runtime error messages that scripts and hosts observe, checked against Go v0.70.0's wording,
//! and the compile errors that now refuse what used to fail with them.
mod common;

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

/// Asserts that each script is refused at compile time, its first
/// diagnostic having the code and pointing at the text given.
fn refuses(cases: &[(&str, &str, &str)]) {
    for (source, code, text) in cases {
        let error = common::static_engine()
            .compile(source)
            .err()
            .unwrap_or_else(|| panic!("{source} compiled"));
        let first = &error.diagnostics()[0];
        assert_eq!(first.code.to_string(), *code, "{source}");
        assert_eq!(&source[first.span.start..first.span.end], *text, "{source}");
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
        "def run -> any\n  begin\n    while true\n    end\n  rescue => e\n    e.message\n  end\nend",
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
        "def run -> string\n  \"x\" * 1000000\nend",
        &[],
        memory.clone(),
        ErrorClass::Limit,
        "memory quota exceeded (8192 bytes)",
    );
    limited(
        "def run(s: string) -> int\n  s.length\nend",
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
        "def f(n: int) -> int\n  f(n + 1)\nend\ndef run -> int\n  f(0)\nend",
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
            "def run -> any\n  \"ab\" * (2**70)\nend",
            Runtime,
            "unsupported multiplication operands",
        ),
        (
            "def run -> any\n  1.5 % 2.0\nend",
            Runtime,
            "unsupported modulo operands",
        ),
        (
            "def run -> any\n  money(\"1.00 USD\") < money(\"1.00 EUR\")\nend",
            Argument,
            "money currency mismatch for comparison",
        ),
        (
            "def run -> any\n  1 % 0\nend",
            ZeroDivision,
            "modulo by zero",
        ),
        (
            "def run -> any\n  (2**70) % 0\nend",
            ZeroDivision,
            "modulo by zero",
        ),
        (
            "def run -> any\n  1.seconds % 0.seconds\nend",
            ZeroDivision,
            "modulo by zero",
        ),
        (
            "def run -> any\n  1 // 0\nend",
            ZeroDivision,
            "division by zero",
        ),
        (
            "def run -> any\n  \"ab\" * -1.5\nend",
            Runtime,
            "negative argument for string repetition",
        ),
        (
            "def run -> any\n  \"ab\" * -(2**70)\nend",
            Runtime,
            "negative argument for string repetition",
        ),
        (
            "def run -> any\n  2 ** (2**70)\nend",
            Limit,
            "integer exponentiation exponent is too large",
        ),
    ]);
    refuses(&[
        ("def run -> any\n  \"a\" + nil\nend", "V0107", "nil"),
        ("def run -> any\n  (1..2) + 1\nend", "V0108", "+"),
        ("def run -> any\n  [1] - 1\nend", "V0108", "-"),
        ("def run -> any\n  1.seconds - Time.now\nend", "V0108", "-"),
        ("def run -> any\n  Time.now * 2\nend", "V0108", "*"),
        (
            "def run -> any\n  money(\"1.00 USD\") * 1.5\nend",
            "V0108",
            "*",
        ),
        ("def run -> any\n  nil / 2\nend", "V0107", "nil"),
        ("def run -> any\n  1.seconds % 2\nend", "V0108", "%"),
        ("def run -> any\n  1.seconds ** 2\nend", "V0108", "**"),
        ("def run -> any\n  x = 1\n  x << 2\nend", "V0108", "<<"),
        ("def run -> any\n  1 << 2\nend", "V0108", "<<"),
        ("def run -> any\n  [1] & 1\nend", "V0108", "&"),
        ("def run -> any\n  1 < \"a\"\nend", "V0108", "<"),
        ("def run -> any\n  -\"a\"\nend", "V0108", "-\"a\""),
        ("def run -> any\n  +nil\nend", "V0107", "nil"),
    ]);
}

#[test]
fn temporal_arithmetic_names_the_overflowing_operation() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run -> any\n  Duration.build(seconds: 9223372036854775807) + 1.seconds\nend",
            Runtime,
            "duration addition result out of int64 range",
        ),
        (
            "def run -> any\n  Duration.build(seconds: -9223372036854775807) - 2.seconds\nend",
            Runtime,
            "duration subtraction result out of int64 range",
        ),
        (
            "def run -> any\n  1.hours * 1e300\nend",
            Runtime,
            "duration multiplication result out of int64 range",
        ),
        (
            "def run -> any\n  1.hours * (2**70)\nend",
            Runtime,
            "duration multiplication result out of int64 range",
        ),
        (
            "def run -> any\n  1.hours / 1e-300\nend",
            Runtime,
            "duration division result out of int64 range",
        ),
        (
            "def run -> any\n  1.hours * (0.0/0.0)\nend",
            Runtime,
            "cannot convert NaN to integer",
        ),
        (
            "def run -> any\n  1.hours * (1.0/0)\nend",
            Runtime,
            "cannot convert Infinity to integer",
        ),
        (
            "def run -> any\n  1.seconds + 1e19\nend",
            Runtime,
            "float 1e+19 is out of integer range",
        ),
        (
            "def run -> any\n  Time.at(0) + (2**70)\nend",
            Runtime,
            "time addition result out of int64 range",
        ),
        (
            "def run -> any\n  Time.at(0) + (0.0/0.0)\nend",
            Runtime,
            "time addition result out of int64 range",
        ),
        (
            "def run -> any\n  Time.at(0) - 9223372036854775807\nend",
            Runtime,
            "time subtraction result out of int64 range",
        ),
        (
            "def run -> any\n  (2**70).seconds\nend",
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
            "def run -> any\n  Duration.parse(\"\")\nend",
            Runtime,
            "empty duration string",
        ),
        (
            "def run -> any\n  Duration.parse(\"P\")\nend",
            Runtime,
            "invalid duration format",
        ),
        (
            "def run -> any\n  Duration.parse(\"PT1S30M\")\nend",
            Runtime,
            "invalid duration format",
        ),
        (
            "def run -> any\n  Duration.parse(\"1.5s\")\nend",
            Runtime,
            "duration must be whole seconds",
        ),
        (
            "def run -> any\n  Duration.parse(\"P1W2D\")\nend",
            Runtime,
            "invalid mixed week duration",
        ),
        (
            "def run -> any\n  Duration.parse(\"PW\")\nend",
            Runtime,
            "invalid week duration format",
        ),
        (
            "def run -> any\n  Duration.parse(\"P+W\")\nend",
            Runtime,
            "invalid week duration",
        ),
        (
            "def run -> any\n  Duration.parse(\"PT99999999999999999999S\")\nend",
            Runtime,
            "invalid duration number",
        ),
        (
            "def run -> any\n  Duration.parse(\"PT99999999999999999999X\")\nend",
            Runtime,
            "invalid duration format",
        ),
        (
            "def run -> any\n  Duration.build(hours: 0.0/0.0)\nend",
            Runtime,
            "Duration.build hours: cannot convert NaN to integer",
        ),
        (
            "def run -> any\n  Duration.build(seconds: 2**70)\nend",
            Runtime,
            "Duration.build seconds: integer must fit in a 64-bit integer",
        ),
    ]);
    refuses(&[
        ("def run -> any\n  Duration.parse(1)\nend", "V0101", "1"),
        (
            "def run -> any\n  Duration.build(1, hours: 2)\nend",
            "V0301",
            "build",
        ),
        (
            "def run -> any\n  Duration.build(hours: nil, bogus: 2)\nend",
            "V0101",
            "nil",
        ),
        (
            "def run -> any\n  Duration.build(seconds: nil)\nend",
            "V0101",
            "nil",
        ),
        ("def run -> any\n  1.hours.to_s(1)\nend", "V0301", "to_s"),
        (
            "def run -> any\n  1.hours.inspect(k: 1)\nend",
            "V0302",
            "k:",
        ),
        ("def run -> any\n  1.hours.string { 1 }\nend", "V0305", "{"),
        (
            "def run -> any\n  1.hours.between?(1)\nend",
            "V0301",
            "between?",
        ),
        (
            "def run -> any\n  1.hours.between?(1, 2)\nend",
            "V0101",
            "1",
        ),
        ("def run -> any\n  1.hours.ago(x: 1)\nend", "V0302", "x:"),
        ("def run -> any\n  1.hours.ago(1, 2)\nend", "V0301", "ago"),
        ("def run -> any\n  1.hours.after(1)\nend", "V0101", "1"),
    ]);
}

#[test]
fn removed_calls_refuse_arguments_without_static_types() {
    let error = common::gradual_engine()
        .compile("def run -> any\n  now(1)\nend")
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, "now does not take arguments");
    assert_eq!(error.class(), Some(ErrorClass::Runtime));
    let error = common::gradual_engine()
        .compile("def run -> any\n  Time.at(0).nil?(x: 1)\nend")
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, "time.nil? does not take keyword arguments");
}

#[test]
fn time_constructors_and_members_use_go_wording() {
    use ErrorClass::{Limit, Runtime};
    rejects(&[
        (
            "def run -> any\n  Time.local(2**70)\nend",
            Runtime,
            "Time constructor parts must fit in a 64-bit integer",
        ),
        (
            "def run -> any\n  Time.utc(2024, 1, 1, 0, 0, 0, 1e6)\nend",
            Runtime,
            "Time constructor microsecond argument out of range (must be within one second)",
        ),
        (
            "def run -> any\n  Time.at(0.0/0.0)\nend",
            Runtime,
            "Time.at expects a finite numeric epoch",
        ),
        (
            "def run -> any\n  Time.at(0, 2**70)\nend",
            Runtime,
            "Time.at subsecond value out of range",
        ),
        (
            "def run -> any\n  Time.parse(\"x\")\nend",
            Runtime,
            "Time.parse could not parse time",
        ),
        (
            "def run -> any\n  Time.at(0).localtime(\"+0x:00\")\nend",
            Runtime,
            "invalid timezone offset",
        ),
        (
            "def run -> any\n  Time.at(0).localtime(\"Not/AZone\")\nend",
            Runtime,
            "invalid timezone \"Not/AZone\"",
        ),
        (
            "def run -> any\n  Time.at(0).format(\"%Y-%m-%d\")\nend",
            Runtime,
            "time.format expects a Go layout such as \"2006-01-02\"; \"%Y-%m-%d\" is a strftime format, use strftime for that",
        ),
        (
            "def run -> any\n  Time.at(0).strftime(\"2006-01-02\")\nend",
            Runtime,
            "time.strftime expects a percent format such as \"%Y-%m-%d\"; \"2006-01-02\" is a Go layout, use format for that",
        ),
        (
            "def run -> any\n  Time.at(0).strftime(\"%Y%\")\nend",
            Runtime,
            "time.strftime invalid format: \"%Y%\"",
        ),
        (
            "def run -> any\n  Time.at(0).iso8601(-1)\nend",
            Runtime,
            "time.iso8601 precision must be non-negative",
        ),
        (
            "def run -> any\n  Time.at(0).iso8601(101)\nend",
            Limit,
            "time.iso8601 precision exceeds maximum 100 digits",
        ),
        (
            "def run -> any\n  Time.at(0).<=>(1, 2)\nend",
            Runtime,
            "time.<=> expects 1 argument, got 2",
        ),
        (
            "def run -> any\n  Time.at(0).itself(1)\nend",
            Runtime,
            "time.itself expects 0 arguments, got 1",
        ),
    ]);
    refuses(&[
        ("def run -> any\n  Time.utc(nil)\nend", "V0101", "nil"),
        (
            "def run -> any\n  Time.utc(0.0/0.0)\nend",
            "V0101",
            "0.0/0.0",
        ),
        ("def run -> any\n  Time.utc(-1.0/0)\nend", "V0101", "1.0/0"),
        ("def run -> any\n  Time.utc(1e300)\nend", "V0101", "1e300"),
        (
            "def run -> any\n  Time.utc(2024, 1, 1, 0, 0, 0, 0, 1)\nend",
            "V0301",
            "utc",
        ),
        (
            "def run -> any\n  Time.utc(2024, 1, 1, 0, 0, 0, \"x\")\nend",
            "V0101",
            "\"x\"",
        ),
        (
            "def run -> any\n  Time.at(1, 2, 3, 4, bogus: 1)\nend",
            "V0301",
            "at",
        ),
        (
            "def run -> any\n  Time.at(1, bogus: 1)\nend",
            "V0302",
            "bogus:",
        ),
        ("def run -> any\n  Time.at(nil)\nend", "V0101", "nil"),
        (
            "def run -> any\n  Time.at(0, nil, :microsecond)\nend",
            "V0101",
            "nil",
        ),
        (
            "def run -> any\n  Time.at(0, 1, :picosecond)\nend",
            "V0101",
            ":picosecond",
        ),
        (
            "def run -> any\n  Time.at(0, 1, [1, 2])\nend",
            "V0101",
            "[1, 2]",
        ),
        (
            "def run -> any\n  Time.at(0, 1, \"x\" * 100)\nend",
            "V0101",
            "\"x\" * 100",
        ),
        ("def run -> any\n  Time.parse(1)\nend", "V0101", "1"),
        ("def run -> any\n  Time.parse(\"x\", 1)\nend", "V0101", "1"),
        (
            "def run -> any\n  Time.parse(\"x\", foo: 1)\nend",
            "V0302",
            "foo:",
        ),
        ("def run -> any\n  Time.now(1)\nend", "V0301", "now"),
        // The removed global `now` is refused with any arguments; without
        // static types the runtime refuses them.
        ("def run -> any\n  now(1)\nend", "V0401", "now"),
        (
            "def run -> any\n  Time.at(0).nil?(x: 1)\nend",
            "V0402",
            "nil?",
        ),
        (
            "def run -> any\n  Time.local(2024, in: 5)\nend",
            "V0101",
            "5",
        ),
        (
            "def run -> any\n  Time.at(0).localtime(x: 1)\nend",
            "V0302",
            "x:",
        ),
        (
            "def run -> any\n  Time.at(0).localtime(\"a\", \"b\")\nend",
            "V0301",
            "localtime",
        ),
        (
            "def run -> any\n  Time.at(0).strftime(1)\nend",
            "V0101",
            "1",
        ),
        ("def run -> any\n  Time.at(0).format(1)\nend", "V0101", "1"),
        (
            "def run -> any\n  Time.at(0).format\nend",
            "V0301",
            "format",
        ),
        (
            "def run -> any\n  Time.at(0).iso8601(1.5)\nend",
            "V0101",
            "1.5",
        ),
        (
            "def run -> any\n  Time.at(0).round(1, 2)\nend",
            "V0301",
            "round",
        ),
        ("def run -> any\n  Time.at(0).ceil(1)\nend", "V0301", "ceil"),
        (
            "def run -> any\n  Time.at(0).floor(x: 1)\nend",
            "V0302",
            "x:",
        ),
        (
            "def run -> any\n  Time.at(0).httpdate(1)\nend",
            "V0301",
            "httpdate",
        ),
        (
            "def run -> any\n  Time.at(0).to_s(1) { 2 }\nend",
            "V0301",
            "to_s",
        ),
        ("def run -> any\n  Time.at(0).dup { 1 }\nend", "V0305", "{"),
        (
            "def run -> any\n  Time.at(0).year(x: 1)\nend",
            "V0302",
            "x:",
        ),
        ("def run -> any\n  1.hours.since\nend", "V0401", "since"),
    ]);
}

#[test]
fn time_parse_explains_rejections_with_go_parse_errors() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run -> any\n  Time.parse(\"2024-13-01\", \"2006-01-02\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024-13-01\": month out of range",
        ),
        (
            "def run -> any\n  Time.parse(\"2024-01-01 junk\", \"2006-01-02\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024-01-01 junk\": extra text: \" junk\"",
        ),
        (
            "def run -> any\n  Time.parse(\"x2024\", \"2006\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"x2024\" as \"2006\": cannot parse \"x2024\" as \"2006\"",
        ),
        (
            "def run -> any\n  Time.parse(\"2024-02-30\", \"2006-01-02\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024-02-30\": day out of range",
        ),
        (
            "def run -> any\n  Time.parse(\"2024 +2500\", \"2006 -0700\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024 +2500\": time zone offset hour out of range",
        ),
        (
            "def run -> any\n  Time.parse(\"é2024\", \"2006\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"\\xc3\\xa92024\" as \"2006\": cannot parse \"\\xc3\\xa92024\" as \"2006\"",
        ),
    ]);
    refuses(&[
        (
            "def run -> any\n  1.hours.after(\"nope\")\nend",
            "V0101",
            "\"nope\"",
        ),
        (
            "def run -> any\n  1.hours.after(\"2024-02-30T00:00:00Z\")\nend",
            "V0101",
            "\"2024-02-30T00:00:00Z\"",
        ),
    ]);
}

#[test]
fn numeric_members_and_conversions_use_go_wording() {
    use ErrorClass::{Limit, Runtime, ZeroDivision};
    rejects(&[
        (
            "def run -> any\n  5.div(0)\nend",
            ZeroDivision,
            "int.div by zero",
        ),
        (
            "def run -> any\n  7 % 0\nend",
            ZeroDivision,
            "modulo by zero",
        ),
        (
            "def run -> any\n  (1.0/0).to_i\nend",
            Runtime,
            "float.to_i result out of int64 range",
        ),
        (
            "def run -> any\n  0.0.div(0.0/0.0)\nend",
            Runtime,
            "float.div result out of int64 range",
        ),
        (
            "def run -> any\n  1.round(2**40)\nend",
            Runtime,
            "int.round precision 1099511627776 too big to convert to int",
        ),
        (
            "def run -> any\n  1.floor(-(2**70))\nend",
            Runtime,
            "int.floor precision too small to convert to int",
        ),
        (
            "def run -> any\n  (0.0/0.0).floor\nend",
            Runtime,
            "float.floor result out of int64 range",
        ),
        (
            "def run -> any\n  5.clamp(10, 0)\nend",
            Runtime,
            "int.clamp min must be <= max",
        ),
        (
            "def run -> any\n  5.clamp(1...3)\nend",
            Runtime,
            "int.clamp cannot clamp with exclusive range",
        ),
        (
            "def run -> any\n  to_int(1.5)\nend",
            Runtime,
            "to_int cannot convert non-integer float",
        ),
        (
            "def run -> any\n  to_int(1.0/0)\nend",
            Runtime,
            "to_int result out of int64 range",
        ),
        (
            "def run -> any\n  to_int(\"abc\")\nend",
            Runtime,
            "to_int expects a base-10 integer string",
        ),
        (
            "def run -> any\n  to_int(\"\")\nend",
            Runtime,
            "to_int expects a numeric string",
        ),
        (
            "def run -> any\n  to_float(\"1e400\")\nend",
            Runtime,
            "to_float expects a numeric string",
        ),
        (
            "def run -> any\n  to_float(\"Infinity\")\nend",
            Runtime,
            "to_float expects a finite numeric string",
        ),
        (
            "def run -> any\n  to_float(\"nan\")\nend",
            Runtime,
            "to_float expects a finite numeric string",
        ),
        (
            "def run -> any\n  \"12a\".to_i\nend",
            Runtime,
            "string.to_i expects a base-10 integer string",
        ),
        (
            "def run -> any\n  (\"1\" * 100001).to_i\nend",
            Limit,
            "string.to_i exceeds the 100000 digit conversion limit",
        ),
        (
            "def run -> any\n  \"-inf\".to_f\nend",
            Runtime,
            "string.to_f expects a finite numeric string",
        ),
        (
            "def run -> any\n  \"ffffffffffffffffffff\".hex\nend",
            Runtime,
            "string.hex integer out of range",
        ),
        (
            "def run -> any\n  \"7777777777777777777777777\".oct\nend",
            Runtime,
            "string.oct integer out of range",
        ),
        (
            "def run -> any\n  Math.sqrt(-1)\nend",
            Runtime,
            "Math.sqrt out of domain",
        ),
        (
            "def run -> any\n  Math.log(1, -2)\nend",
            Runtime,
            "Math.log out of domain",
        ),
        (
            "def run -> any\n  \"a\".rjust(5, \"\")\nend",
            Runtime,
            "string.rjust pad must not be empty",
        ),
        (
            "def run -> any\n  \"a\".clamp(\"b\", \"a\")\nend",
            Runtime,
            "string.clamp min must be <= max",
        ),
    ]);
    refuses(&[
        ("def run -> any\n  5.0.modulo(0)\nend", "V0401", "modulo"),
        ("def run -> any\n  5.divmod(0.0)\nend", "V0101", "0.0"),
        ("def run -> any\n  5.div\nend", "V0301", "div"),
        (
            "def run -> any\n  5.remainder(\"x\")\nend",
            "V0101",
            "\"x\"",
        ),
        ("def run -> any\n  1.5.round(1.5)\nend", "V0101", "1.5"),
        ("def run -> any\n  1.ceil(1, 2)\nend", "V0301", "ceil"),
        ("def run -> any\n  5.clamp(1)\nend", "V0101", "1"),
        (
            "def run -> any\n  5.5.clamp(\"x\", 1)\nend",
            "V0101",
            "\"x\"",
        ),
        (
            "def run -> any\n  5.clamp(0.0/0.0, nil)\nend",
            "V0101",
            "0.0/0.0",
        ),
        ("def run -> any\n  5.between?(1)\nend", "V0301", "between?"),
        ("def run -> any\n  5.zero?(1)\nend", "V0301", "zero?"),
        ("def run -> any\n  1.5.nan?(1)\nend", "V0301", "nan?"),
        ("def run -> any\n  to_int(nil)\nend", "V0101", "nil"),
        ("def run -> any\n  to_int(1, 2)\nend", "V0301", "to_int"),
        ("def run -> any\n  \"a\".hex(1)\nend", "V0301", "hex"),
        ("def run -> any\n  Math.atan2(1)\nend", "V0301", "atan2"),
        ("def run -> any\n  Math.log(1, 2, 3)\nend", "V0301", "log"),
        ("def run -> any\n  Math.sqrt(\"x\")\nend", "V0101", "\"x\""),
        ("def run -> any\n  Math.sqrt(1) { 2 }\nend", "V0305", "{"),
        ("def run -> any\n  \"a\".center(1e20)\nend", "V0101", "1e20"),
        (
            "def run -> any\n  \"a\".center(\"x\")\nend",
            "V0101",
            "\"x\"",
        ),
        ("def run -> any\n  \"a\".ljust(5, 1)\nend", "V0101", "1"),
        (
            "def run -> any\n  \"a\".center(5, x: 1)\nend",
            "V0302",
            "x:",
        ),
        ("def run -> any\n  \"a\".partition(1)\nend", "V0101", "1"),
        (
            "def run -> any\n  \"a\".rpartition(\"a\", \"b\")\nend",
            "V0301",
            "rpartition",
        ),
        ("def run -> any\n  \"a\".clamp(1, nil)\nend", "V0101", "1"),
        (
            "def run -> any\n  \"a\".between?(1)\nend",
            "V0301",
            "between?",
        ),
    ]);
}

#[test]
fn money_literals_and_members_use_go_wording() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run -> any\n  money(\"1 US\")\nend",
            Runtime,
            "currency must be 3 letters, got \"US\"",
        ),
        (
            "def run -> any\n  money(\". USD\")\nend",
            Runtime,
            "invalid money amount \". USD\"",
        ),
        (
            "def run -> any\n  money(\"1.2x4 USD\")\nend",
            Runtime,
            "invalid money amount \"1.2x4 USD\"",
        ),
        (
            "def run -> any\n  money(\"1.234 USD\")\nend",
            Runtime,
            "money literal supports at most 2 decimal places: \"1.234 USD\"",
        ),
        (
            "def run -> any\n  money(\"1 2 USD\")\nend",
            Runtime,
            "invalid money literal \"1 2 USD\"",
        ),
        (
            "def run -> any\n  money(\"1.00 USD\").itself(x: 1)\nend",
            Runtime,
            "money.itself does not accept keyword arguments",
        ),
    ]);
    refuses(&[
        (
            "def run -> any\n  money(\"a\", \"b\")\nend",
            "V0301",
            "money",
        ),
        (
            "def run -> any\n  money_cents(\"1\", \"USD\")\nend",
            "V0101",
            "\"1\"",
        ),
        (
            "def run -> any\n  money_cents(1, :USD)\nend",
            "V0101",
            ":USD",
        ),
        (
            "def run -> any\n  money_cents(1)\nend",
            "V0301",
            "money_cents",
        ),
        (
            "def run -> any\n  money_cents(1e19, \"USD\")\nend",
            "V0101",
            "1e19",
        ),
        (
            "def run -> any\n  money(\"1.00 USD\").to_s(1)\nend",
            "V0301",
            "to_s",
        ),
        (
            "def run -> any\n  money(\"1.00 USD\").between?(1)\nend",
            "V0301",
            "between?",
        ),
    ]);
}

#[test]
fn regex_errors_quote_go_syntax_errors_and_name_the_operation() {
    use ErrorClass::{Limit, Runtime};
    rejects(&[
        (
            "def run -> any\n  Regex.match(\"(\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: missing closing ): `(`",
        ),
        (
            "def run -> any\n  Regex.match(\")\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: unexpected ): `)`",
        ),
        (
            "def run -> any\n  Regex.match(\"a*?*\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid nested repetition operator: `*?*`",
        ),
        (
            "def run -> any\n  Regex.match(\"a|+?\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: missing argument to repetition operator: `+?`",
        ),
        (
            "def run -> any\n  Regex.match(\"a{2,1}\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid repeat count: `{2,1}`",
        ),
        (
            "def run -> any\n  Regex.match(\"(?P<na-me>x)\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid named capture: `(?P<na-me>`",
        ),
        (
            "def run -> any\n  Regex.match(\"(?i\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid or unsupported Perl syntax: `(?i`",
        ),
        (
            "def run -> any\n  Regex.match(\"\\\\\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: trailing backslash at end of expression: ``",
        ),
        (
            "def run -> any\n  Regex.match(\"\\\\xZq\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid escape sequence: `\\xZq`",
        ),
        (
            "def run -> any\n  Regex.match(\"\\\\p{Greek\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid character class range: `\\p{Greek`",
        ),
        (
            "def run -> any\n  Regex.match(\"[z-a]\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid character class range: `z-a`",
        ),
        (
            "def run -> any\n  \"a\".match?(\"[a\")\nend",
            Runtime,
            "string.match? invalid regex: error parsing regexp: missing closing ]: `[a`",
        ),
        (
            "def run -> any\n  /(/i\nend",
            Runtime,
            "regex literal invalid regex: error parsing regexp: missing closing ): `(?i)(`",
        ),
        (
            "def run -> any\n  \"a\".sub(Regex.new(\"(\"), \"b\")\nend",
            Runtime,
            "Regexp.new invalid regex: error parsing regexp: missing closing ): `(`",
        ),
        (
            "def run -> any\n  Regex.match(\"x\" * 20000, \"a\")\nend",
            Limit,
            "Regex.match pattern exceeds limit 16384 bytes",
        ),
        (
            "def run -> any\n  Regex.replace(\"a\", \"a\", \"b\" * 1048577)\nend",
            Limit,
            "Regex.replace replacement exceeds limit 1048576 bytes",
        ),
        (
            "def run -> any\n  Regex.replace_all(\"a\" * 1024, \"a\", \"b\" * 1025)\nend",
            Limit,
            "Regex.replace_all output exceeds limit 1048576 bytes",
        ),
        (
            "def run -> any\n  (\"a\" * 1048577) =~ /a/\nend",
            Limit,
            "=~ text exceeds limit 1048576 bytes",
        ),
        (
            "def run -> any\n  case \"a\" * 1048577\n  when /a/ then 1\n  end\nend",
            Limit,
            "regex match text exceeds limit 1048576 bytes",
        ),
        (
            "def run -> any\n  (\"a\" * 1024).gsub(\"a\", \"b\" * 1025)\nend",
            Limit,
            "string.gsub output exceeds limit 1048576 bytes",
        ),
        (
            "def run -> any\n  \"a\".sub(\"a\") { \"b\" * 1048577 }\nend",
            Limit,
            "output exceeds limit 1048576 bytes",
        ),
        (
            "def run -> any\n  \"a\".match?(\"a\", -1)\nend",
            Runtime,
            "string.match? offset must be non-negative integer",
        ),
        (
            "def run -> any\n  \"a\".sub(/(?<x>a)/, \"\\\\k<y>\")\nend",
            Runtime,
            "string.sub undefined group name reference: y",
        ),
        (
            "def run -> any\n  \"a\".gsub(/(?<x>a)/, \"\\\\k<y\")\nend",
            Runtime,
            "string.gsub invalid group name reference format",
        ),
        (
            "def run -> any\n  Regex.union(\"a\" * 20000)\nend",
            Limit,
            "Regexp.union pattern exceeds limit 16384 bytes",
        ),
    ]);
    refuses(&[
        ("def run -> any\n  /a/ =~ /a/\nend", "V0108", "=~"),
        (
            "def run -> any\n  Regex.match(\"a\")\nend",
            "V0301",
            "match",
        ),
        (
            "def run -> any\n  Regex.replace_all(\"a\", 1, \"b\")\nend",
            "V0101",
            "1",
        ),
        ("def run -> any\n  /a/.match(1)\nend", "V0101", "1"),
        ("def run -> any\n  /a/.source(1)\nend", "V0301", "source"),
        (
            "def run -> any\n  \"a\".match(\"a\", \"x\")\nend",
            "V0101",
            "\"x\"",
        ),
        (
            "def run -> any\n  \"a\".match(\"a\", x: 1)\nend",
            "V0302",
            "x:",
        ),
        ("def run -> any\n  \"a\".scan\nend", "V0301", "scan"),
        ("def run -> any\n  \"a\".scan(1)\nend", "V0101", "1"),
        (
            "def run -> any\n  \"a\".gsub!(\"a\", \"b\", x: 1)\nend",
            "V0301",
            "gsub!",
        ),
        (
            "def run -> any\n  \"a\".sub(Regex.new(/a/), \"b\")\nend",
            "V0101",
            "/a/",
        ),
        (
            "def run -> any\n  \"a\".sub(\"a\", \"b\") { 1 }\nend",
            "V0301",
            "sub",
        ),
        ("def run -> any\n  \"a\".gsub(\"a\")\nend", "V0301", "gsub"),
        ("def run -> any\n  \"a\".sub(\"a\", 1)\nend", "V0101", "1"),
        ("def run -> any\n  Regex.new(1)\nend", "V0101", "1"),
        ("def run -> any\n  Regex.union(\"a\", 1)\nend", "V0101", "1"),
        (
            "def run -> any\n  Regex.escape(\"a\") { 1 }\nend",
            "V0305",
            "{",
        ),
        (
            "def run -> any\n  Regex.last_match(1)\nend",
            "V0203",
            "last_match",
        ),
    ]);
}

#[test]
fn calls_name_missing_arguments_visibility_and_removed_constructors() {
    use ErrorClass::Runtime;
    rejects(&[(
        "def run -> any\n  next\nend",
        Runtime,
        "next used outside of loop",
    )]);
    // The checker refuses a call its visibility forbids (V0208); without
    // static types, the runtime refuses it when it runs.
    for (source, message, text) in [
        (
            "class C\n  private def secret\n    1\n  end\nend\ndef run -> any\n  C.new.secret\nend",
            "private method secret",
            "secret",
        ),
        (
            "class C\n  private\n  def x=(v: int)\n    1\n  end\nend\ndef run -> any\n  c = C.new\n  c.x = 2\nend",
            "private method x=",
            "x",
        ),
        (
            "class C\n  private def ==(o: any) -> bool\n    true\n  end\nend\ndef run -> any\n  C.new != 1\nend",
            "private method ==",
            "!=",
        ),
        (
            "module M\n  protected\n  def self.f\n    1\n  end\nend\ndef run -> any\n  M.f\nend",
            "protected method f",
            "f",
        ),
    ] {
        let error = common::gradual_engine()
            .compile(source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, message, "{source}");
        assert_eq!(error.class(), Some(Runtime), "{source}");
        refuses(&[(source, "V0208", text)]);
    }
    refuses(&[
        (
            "def add(a: int, b: int) -> int\n  a + b\nend\ndef run -> any\n  add(1)\nend",
            "V0301",
            "add",
        ),
        (
            "def one(a: int) -> int\n  a\nend\ndef run -> any\n  one(1, 2)\nend",
            "V0301",
            "one",
        ),
        (
            "def add(a: int, b: int) -> int\n  a + b\nend\ndef run -> any\n  add(*nil)\nend",
            "V0107",
            "nil",
        ),
        (
            "def kw(a: int, *, x: int = 0) -> int\n  a\nend\ndef run -> any\n  kw(1, **[1, 2])\nend",
            "V0101",
            "[1, 2]",
        ),
        ("def run -> any\n  proc { 1 }\nend", "V0201", "proc"),
        ("def run -> any\n  Proc.new { 1 }\nend", "V0201", "Proc"),
        ("def run -> any\n  money\nend", "V0301", "money"),
        (
            "def run -> any\n  JSON.stringify\nend",
            "V0301",
            "stringify",
        ),
        (
            "def f(v: Missing)\n  v\nend\ndef run -> any\n  f(1)\nend",
            "V0116",
            "Missing",
        ),
        (
            "def f(v: any) -> Missing\n  v\nend\ndef run -> any\n  f(1)\nend",
            "V0116",
            "Missing",
        ),
        (
            "enum STATUS\n  A\nend\nenum Status\n  B\nend\ndef f(s: status)\n  s\nend\ndef run -> any\n  f(1)\nend",
            "V0116",
            "status",
        ),
        (
            "class User\nend\nenum USER\n  A\nend\ndef f(v: user)\n  v\nend\ndef run -> any\n  f(1)\nend",
            "V0116",
            "user",
        ),
        (
            "module M\nend\ndef run -> any\n  M.new\nend",
            "V0203",
            "new",
        ),
        (
            "def run -> any\n  m = \"a\".match(/(a)(b)?/)\n  m&.end(nil)\nend",
            "V0101",
            "nil",
        ),
        (
            "def run -> any\n  m = \"a\".match(/a/)\n  m&.begin\nend",
            "V0301",
            "begin",
        ),
    ]);
}

#[test]
fn random_builtins_name_themselves_and_the_rejected_argument() {
    use ErrorClass::*;
    rejects(&[
        (
            "def run -> any\n  rand(0)\nend",
            Runtime,
            "rand integer bound must be positive",
        ),
        (
            "def run -> any\n  rand(2 ** 70)\nend",
            Runtime,
            "rand integer bound must fit in a 64-bit integer",
        ),
        (
            "def run -> any\n  rand(1..)\nend",
            Runtime,
            "rand range must be bounded",
        ),
        (
            "def run -> any\n  rand(2...2)\nend",
            Runtime,
            "rand range is empty",
        ),
        (
            "def run -> any\n  srand(2 ** 70)\nend",
            Runtime,
            "srand seed must fit in a 64-bit integer",
        ),
        (
            "def run -> any\n  random_id(0)\nend",
            Runtime,
            "random_id length must be positive",
        ),
        (
            "def run -> any\n  random_id(2 ** 70)\nend",
            Runtime,
            "random_id length must fit in a 64-bit integer",
        ),
        (
            "def run -> any\n  random_id(1025)\nend",
            Limit,
            "random_id length exceeds maximum 1024",
        ),
    ]);
    refuses(&[
        ("def run -> any\n  rand(1, 2)\nend", "V0301", "rand"),
        ("def run -> any\n  rand(1, 2, x: 1)\nend", "V0301", "rand"),
        ("def run -> any\n  rand {\n  }\nend", "V0301", "rand"),
        ("def run -> any\n  rand(1.5)\nend", "V0101", "1.5"),
        ("def run -> any\n  srand(1, 2)\nend", "V0301", "srand"),
        ("def run -> any\n  srand(\"x\")\nend", "V0101", "\"x\""),
        ("def run -> any\n  uuid(1, x: 2)\nend", "V0301", "uuid"),
        ("def run -> any\n  uuid(x: 2)\nend", "V0302", "x:"),
        ("def run -> any\n  uuid {\n  }\nend", "V0305", "{"),
        (
            "def run -> any\n  random_id(1, 2)\nend",
            "V0301",
            "random_id",
        ),
        ("def run -> any\n  random_id(nil)\nend", "V0101", "nil"),
    ]);
}

#[test]
fn integer_and_range_stepping_check_arguments_in_go_order() {
    use ErrorClass::*;
    rejects(&[(
        "def run -> any\n  (..3).step(1) { |i|\n  }\nend",
        Runtime,
        "cannot iterate a beginless range",
    )]);
    refuses(&[
        (
            "def run -> any\n  3.times(1) { |i|\n  }\nend",
            "V0301",
            "times",
        ),
        ("def run -> any\n  (2 ** 70).times\nend", "V0304", "times"),
        ("def run -> any\n  1.upto\nend", "V0301", "upto"),
        (
            "def run -> any\n  1.upto(2, 3, x: 1) { |i|\n  }\nend",
            "V0301",
            "upto",
        ),
        (
            "def run -> any\n  1.downto(0, x: 1) { |i|\n  }\nend",
            "V0302",
            "x:",
        ),
        ("def run -> any\n  1.upto(\"x\")\nend", "V0304", "upto"),
        (
            "def run -> any\n  (2 ** 70).downto(\"x\")\nend",
            "V0304",
            "downto",
        ),
        ("def run -> any\n  1.upto(3)\nend", "V0304", "upto"),
        ("def run -> any\n  1.step\nend", "V0301", "step"),
        ("def run -> any\n  1.step(3, x: 1)\nend", "V0304", "step"),
        (
            "def run -> any\n  1.step(3, nil) { |i|\n  }\nend",
            "V0101",
            "nil",
        ),
        ("def run -> any\n  1.step(3, 0)\nend", "V0304", "step"),
        ("def run -> any\n  1.step(3)\nend", "V0304", "step"),
        ("def run -> any\n  1.step(3, 2 ** 70)\nend", "V0304", "step"),
        ("def run -> any\n  (1..3).step(1, 2)\nend", "V0301", "step"),
        (
            "def run -> any\n  (1..3).step(1, x: 2)\nend",
            "V0304",
            "step",
        ),
        ("def run -> any\n  (1..3).step(\"x\")\nend", "V0304", "step"),
        ("def run -> any\n  (1..3).step(0)\nend", "V0304", "step"),
        ("def run -> any\n  (1..).step(1)\nend", "V0304", "step"),
    ]);
}

#[test]
fn string_members_name_themselves_and_the_rejected_argument() {
    use ErrorClass::Runtime;
    let cases = [
        (
            "\"a b\".split(\" \", 2 ** 70)",
            "string.split limit must fit in a 64-bit integer",
        ),
        ("\"\".ord", "string.ord requires non-empty string"),
        (
            "\"ab\".replace",
            "string.replace expects exactly one replacement",
        ),
        (
            "\"ab\".insert(-5, \"y\")",
            "string.insert index -5 out of string",
        ),
        (
            "\"{{a.b}}\".template({}, strict: true)",
            "string.template missing placeholder a.b",
        ),
        (
            "\"{{ a }}\".template({a: [1]})",
            "string.template placeholder a value must be scalar",
        ),
    ];
    for (expression, message) in cases {
        let source = format!("def run -> any\n  {expression}\nend");
        limited(&source, &[], Limits::default(), Runtime, message);
    }
    for (expression, code, text) in [
        ("\"abc\".index(\"b\", \"x\")", "V0101", "\"x\""),
        ("\"abc\".rindex(1, 2)", "V0101", "1"),
        ("\"abc\".rindex(\"b\", 1, 2)", "V0301", "rindex"),
        ("\"a b\".split(\" \", \"x\")", "V0101", "\"x\""),
        ("\"a b\".split(1)", "V0101", "1"),
        ("\"a\".concat(\"b\", 1)", "V0101", "1"),
        ("\"a\".prepend(\"b\", 1)", "V0101", "1"),
        ("\"a\".chomp!(1)", "V0101", "1"),
        ("\"a\".chomp(\"a\", \"b\")", "V0301", "chomp"),
        ("\"ab\".delete_suffix!(1)", "V0101", "1"),
        ("\"ab\".delete_prefix", "V0301", "delete_prefix"),
        ("\"ab\".start_with?", "V0301", "start_with?"),
        ("\"ab\".end_with?(\"x\", 1)", "V0101", "1"),
        ("\"ab\".include?(1)", "V0101", "1"),
        ("\"ab\".insert(\"x\", 2)", "V0101", "\"x\""),
        ("\"ab\".downcase!(:bogus)", "V0101", ":bogus"),
        ("\"ab\".upcase(:fold)", "V0101", ":fold"),
        ("\"ab\".upcase(:ascii, :x)", "V0301", "upcase"),
        ("\"ab\".capitalize(\"x\")", "V0101", "\"x\""),
        ("\"ab\".template({}, 1)", "V0301", "template"),
        ("\"ab\".template(1)", "V0101", "1"),
        ("\"ab\".template({}, strict: 1)", "V0101", "1"),
        ("\"ab\".template({}, other: 1)", "V0302", "other:"),
    ] {
        refuses(&[(&format!("def run -> any\n  {expression}\nend"), code, text)]);
    }
}
