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

#[test]
fn time_constructors_and_members_use_go_wording() {
    use ErrorClass::{Limit, Runtime};
    rejects(&[
        (
            "def run\n  Time.utc(nil)\nend",
            Runtime,
            "Time constructor year must be numeric, got nil",
        ),
        (
            "def run\n  Time.utc(0.0/0.0)\nend",
            Runtime,
            "Time constructor year must be finite, got NaN",
        ),
        (
            "def run\n  Time.utc(-1.0/0)\nend",
            Runtime,
            "Time constructor year must be finite, got -Inf",
        ),
        (
            "def run\n  Time.utc(1e300)\nend",
            Runtime,
            "Time constructor year 1e+300 is out of range",
        ),
        (
            "def run\n  Time.new(2**70)\nend",
            Runtime,
            "Time.new parts must fit in a 64-bit integer",
        ),
        (
            "def run\n  Time.utc(2024, 1, 1, 0, 0, 0, 0, 1)\nend",
            Runtime,
            "Time constructor expects at most year, month, day, hour, minute, second, microsecond",
        ),
        (
            "def run\n  Time.utc(2024, 1, 1, 0, 0, 0, \"x\")\nend",
            Runtime,
            "Time constructor microsecond argument must be numeric",
        ),
        (
            "def run\n  Time.utc(2024, 1, 1, 0, 0, 0, 1e6)\nend",
            Runtime,
            "Time constructor microsecond argument out of range (must be within one second)",
        ),
        (
            "def run\n  Time.at(1, 2, 3, 4, bogus: 1)\nend",
            Runtime,
            "Time.at expects seconds since epoch with optional subsecond value and unit",
        ),
        (
            "def run\n  Time.at(1, bogus: 1)\nend",
            Runtime,
            "Time.at unknown keyword argument bogus",
        ),
        (
            "def run\n  Time.at(0.0/0.0)\nend",
            Runtime,
            "Time.at expects a finite numeric epoch",
        ),
        (
            "def run\n  Time.at(nil)\nend",
            Runtime,
            "Time.at expects numeric seconds",
        ),
        (
            "def run\n  Time.at(0, nil, :usec)\nend",
            Runtime,
            "Time.at subsecond value must be numeric",
        ),
        (
            "def run\n  Time.at(0, 1, :picosecond)\nend",
            Runtime,
            "unexpected unit: picosecond",
        ),
        (
            "def run\n  Time.at(0, 1, [1, 2])\nend",
            Runtime,
            "unexpected unit: [1, 2]",
        ),
        (
            "def run\n  Time.at(0, 1, \"x\" * 100)\nend",
            Runtime,
            "unexpected unit of type string",
        ),
        (
            "def run\n  Time.at(0, 2**70)\nend",
            Runtime,
            "Time.at subsecond value out of range",
        ),
        (
            "def run\n  Time.parse(1)\nend",
            Runtime,
            "Time.parse expects a time string and optional layout",
        ),
        (
            "def run\n  Time.parse(\"x\", 1)\nend",
            Runtime,
            "Time.parse layout must be string",
        ),
        (
            "def run\n  Time.parse(\"x\", foo: 1)\nend",
            Runtime,
            "Time.parse unknown keyword argument foo",
        ),
        (
            "def run\n  Time.parse(\"x\")\nend",
            Runtime,
            "Time.parse could not parse time",
        ),
        (
            "def run\n  Time.now(1)\nend",
            Runtime,
            "Time.now does not take positional arguments",
        ),
        (
            "def run\n  now(1)\nend",
            Runtime,
            "now does not take arguments",
        ),
        (
            "def run\n  Time.new(2024, in: 5)\nend",
            Runtime,
            "invalid timezone spec",
        ),
        (
            "def run\n  Time.at(0).getlocal(\"+0x:00\")\nend",
            Runtime,
            "invalid timezone offset",
        ),
        (
            "def run\n  Time.at(0).localtime(\"Not/AZone\")\nend",
            Runtime,
            "invalid timezone \"Not/AZone\"",
        ),
        (
            "def run\n  Time.at(0).getlocal(x: 1)\nend",
            Runtime,
            "getlocal does not take keyword arguments; pass the offset positionally",
        ),
        (
            "def run\n  Time.at(0).localtime(\"a\", \"b\")\nend",
            Runtime,
            "localtime expects at most one timezone offset argument",
        ),
        (
            "def run\n  Time.at(0).format(\"%Y-%m-%d\")\nend",
            Runtime,
            "time.format expects a Go layout such as \"2006-01-02\"; \"%Y-%m-%d\" is a strftime format, use strftime for that",
        ),
        (
            "def run\n  Time.at(0).strftime(\"2006-01-02\")\nend",
            Runtime,
            "time.strftime expects a percent format such as \"%Y-%m-%d\"; \"2006-01-02\" is a Go layout, use format for that",
        ),
        (
            "def run\n  Time.at(0).strftime(\"%Y%\")\nend",
            Runtime,
            "time.strftime invalid format: \"%Y%\"",
        ),
        (
            "def run\n  Time.at(0).strftime(1)\nend",
            Runtime,
            "time.strftime expects a format string",
        ),
        (
            "def run\n  Time.at(0).format(1)\nend",
            Runtime,
            "format expects a Go layout string",
        ),
        (
            "def run\n  Time.at(0).format\nend",
            Runtime,
            "format is a method and cannot be used as a value; call it with format(...)",
        ),
        (
            "def run\n  Time.at(0).iso8601(1.5)\nend",
            Runtime,
            "time.iso8601 precision must be an Integer",
        ),
        (
            "def run\n  Time.at(0).rfc3339(-1)\nend",
            Runtime,
            "time.rfc3339 precision must be non-negative",
        ),
        (
            "def run\n  Time.at(0).iso8601(101)\nend",
            Limit,
            "time.iso8601 precision exceeds maximum 100 digits",
        ),
        (
            "def run\n  Time.at(0).round(1, 2)\nend",
            Runtime,
            "time.round expects at most one precision argument",
        ),
        (
            "def run\n  Time.at(0).ceil(1)\nend",
            Runtime,
            "ceil does not accept precision",
        ),
        (
            "def run\n  Time.at(0).floor(x: 1)\nend",
            Runtime,
            "time.floor does not accept keyword arguments",
        ),
        (
            "def run\n  Time.at(0).httpdate(1)\nend",
            Runtime,
            "time.httpdate does not accept arguments",
        ),
        (
            "def run\n  Time.at(0).to_s(1) { 2 }\nend",
            Runtime,
            "time.to_s does not take arguments",
        ),
        (
            "def run\n  Time.at(0).<=>(1, 2)\nend",
            Runtime,
            "time.<=> expects 1 argument, got 2",
        ),
        (
            "def run\n  Time.at(0).nil?(x: 1)\nend",
            Runtime,
            "time.nil? does not take keyword arguments",
        ),
        (
            "def run\n  Time.at(0).itself(1)\nend",
            Runtime,
            "time.itself expects 0 arguments, got 1",
        ),
        (
            "def run\n  Time.at(0).dup { 1 }\nend",
            Runtime,
            "dup does not accept blocks",
        ),
        (
            "def run\n  Time.at(0).year(x: 1)\nend",
            Runtime,
            "attempted to call non-callable value",
        ),
        (
            "def run\n  1.hour.ago\nend",
            Runtime,
            "ago is a method and cannot be used as a value; call it with ago(...)",
        ),
    ]);
}

#[test]
fn time_parse_explains_rejections_with_go_parse_errors() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run\n  Time.parse(\"2024-13-01\", \"2006-01-02\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024-13-01\": month out of range",
        ),
        (
            "def run\n  Time.parse(\"2024-01-01 junk\", \"2006-01-02\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024-01-01 junk\": extra text: \" junk\"",
        ),
        (
            "def run\n  Time.parse(\"x2024\", \"2006\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"x2024\" as \"2006\": cannot parse \"x2024\" as \"2006\"",
        ),
        (
            "def run\n  Time.parse(\"2024-02-30\", \"2006-01-02\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024-02-30\": day out of range",
        ),
        (
            "def run\n  Time.parse(\"2024 +2500\", \"2006 -0700\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"2024 +2500\": time zone offset hour out of range",
        ),
        (
            "def run\n  Time.parse(\"\u{e9}2024\", \"2006\")\nend",
            Runtime,
            "Time.parse could not parse time: parsing time \"\\xc3\\xa92024\" as \"2006\": cannot parse \"\\xc3\\xa92024\" as \"2006\"",
        ),
        (
            "def run\n  1.hour.after(\"nope\")\nend",
            Runtime,
            "invalid time: parsing time \"nope\" as \"2006-01-02T15:04:05Z07:00\": cannot parse \"nope\" as \"2006\"",
        ),
        (
            "def run\n  1.hour.after(\"2024-02-30T00:00:00Z\")\nend",
            Runtime,
            "invalid time: parsing time \"2024-02-30T00:00:00Z\": day out of range",
        ),
    ]);
}

#[test]
fn numeric_members_and_conversions_use_go_wording() {
    use ErrorClass::{Limit, Runtime, ZeroDivision};
    rejects(&[
        ("def run\n  5.div(0)\nend", ZeroDivision, "int.div by zero"),
        (
            "def run\n  5.0.modulo(0)\nend",
            ZeroDivision,
            "float.modulo by zero",
        ),
        (
            "def run\n  7.modulo(0)\nend",
            ZeroDivision,
            "int.modulo by zero",
        ),
        (
            "def run\n  5.divmod(0.0)\nend",
            ZeroDivision,
            "int.divmod by zero",
        ),
        (
            "def run\n  5.div\nend",
            Runtime,
            "int.div expects one numeric argument",
        ),
        (
            "def run\n  5.remainder(\"x\")\nend",
            Runtime,
            "int.remainder expects a numeric argument",
        ),
        (
            "def run\n  (1.0/0).to_i\nend",
            Runtime,
            "float.to_i result out of int64 range",
        ),
        (
            "def run\n  0.0.div(0.0/0.0)\nend",
            Runtime,
            "float.div result out of int64 range",
        ),
        (
            "def run\n  1.5.round(1.5)\nend",
            Runtime,
            "float.round precision must be an Integer",
        ),
        (
            "def run\n  1.round(2**40)\nend",
            Runtime,
            "int.round precision 1099511627776 too big to convert to int",
        ),
        (
            "def run\n  1.floor(-(2**70))\nend",
            Runtime,
            "int.floor precision too small to convert to int",
        ),
        (
            "def run\n  1.ceil(1, 2)\nend",
            Runtime,
            "int.ceil expects at most one precision argument",
        ),
        (
            "def run\n  (0.0/0.0).floor\nend",
            Runtime,
            "float.floor result out of int64 range",
        ),
        (
            "def run\n  5.clamp(10, 0)\nend",
            Runtime,
            "int.clamp min must be <= max",
        ),
        (
            "def run\n  5.clamp(1...3)\nend",
            Runtime,
            "int.clamp cannot clamp with exclusive range",
        ),
        (
            "def run\n  5.clamp(1)\nend",
            Runtime,
            "int.clamp expects min and max or range",
        ),
        (
            "def run\n  5.5.clamp(\"x\", 1)\nend",
            Runtime,
            "float.clamp bounds must be numeric or nil",
        ),
        (
            "def run\n  5.clamp(0.0/0.0, nil)\nend",
            Runtime,
            "int.clamp values must not be NaN",
        ),
        (
            "def run\n  5.between?(1)\nend",
            Runtime,
            "int.between? expects min and max",
        ),
        (
            "def run\n  5.zero?(1)\nend",
            Runtime,
            "int.zero? does not take arguments",
        ),
        (
            "def run\n  1.5.nan?(1)\nend",
            Runtime,
            "float.nan? does not take arguments",
        ),
        (
            "def run\n  to_int(1.5)\nend",
            Runtime,
            "to_int cannot convert non-integer float",
        ),
        (
            "def run\n  to_int(1.0/0)\nend",
            Runtime,
            "to_int result out of int64 range",
        ),
        (
            "def run\n  to_int(\"abc\")\nend",
            Runtime,
            "to_int expects a base-10 integer string",
        ),
        (
            "def run\n  to_int(\"\")\nend",
            Runtime,
            "to_int expects a numeric string",
        ),
        (
            "def run\n  to_int(nil)\nend",
            Runtime,
            "to_int expects int, float, or string",
        ),
        (
            "def run\n  to_int(1, 2)\nend",
            Runtime,
            "to_int expects a single value argument",
        ),
        (
            "def run\n  to_float(\"1e400\")\nend",
            Runtime,
            "to_float expects a numeric string",
        ),
        (
            "def run\n  to_float(\"Infinity\")\nend",
            Runtime,
            "to_float expects a finite numeric string",
        ),
        (
            "def run\n  to_float(\"nan\")\nend",
            Runtime,
            "to_float expects a finite numeric string",
        ),
        (
            "def run\n  \"12a\".to_i\nend",
            Runtime,
            "string.to_i expects a base-10 integer string",
        ),
        (
            "def run\n  (\"1\" * 100001).to_i\nend",
            Limit,
            "string.to_i exceeds the 100000 digit conversion limit",
        ),
        (
            "def run\n  \"-inf\".to_f\nend",
            Runtime,
            "string.to_f expects a finite numeric string",
        ),
        (
            "def run\n  \"ffffffffffffffffffff\".hex\nend",
            Runtime,
            "string.hex integer out of range",
        ),
        (
            "def run\n  \"7777777777777777777777777\".oct\nend",
            Runtime,
            "string.oct integer out of range",
        ),
        (
            "def run\n  \"a\".hex(1)\nend",
            Runtime,
            "string.hex does not take arguments",
        ),
        (
            "def run\n  Math.sqrt(-1)\nend",
            Runtime,
            "Math.sqrt out of domain",
        ),
        (
            "def run\n  Math.log(1, -2)\nend",
            Runtime,
            "Math.log out of domain",
        ),
        (
            "def run\n  Math.atan2(1)\nend",
            Runtime,
            "Math.atan2 expects 2 arguments, got 1",
        ),
        (
            "def run\n  Math.log(1, 2, 3)\nend",
            Runtime,
            "Math.log expects 1 or 2 arguments, got 3",
        ),
        (
            "def run\n  Math.sqrt(\"x\")\nend",
            Runtime,
            "Math.sqrt expects a numeric argument, got string",
        ),
        (
            "def run\n  Math.sqrt(1) { 2 }\nend",
            Runtime,
            "Math.sqrt does not accept a block",
        ),
        (
            "def run\n  \"a\".center(1e20)\nend",
            Runtime,
            "string.center width is out of range",
        ),
        (
            "def run\n  \"a\".center(\"x\")\nend",
            Runtime,
            "string.center width must be integer",
        ),
        (
            "def run\n  \"a\".ljust(5, 1)\nend",
            Runtime,
            "string.ljust pad must be string",
        ),
        (
            "def run\n  \"a\".rjust(5, \"\")\nend",
            Runtime,
            "string.rjust pad must not be empty",
        ),
        (
            "def run\n  \"a\".center(5, x: 1)\nend",
            Runtime,
            "string.center does not accept keyword arguments",
        ),
        (
            "def run\n  \"a\".partition(1)\nend",
            Runtime,
            "string.partition separator must be string",
        ),
        (
            "def run\n  \"a\".rpartition(\"a\", \"b\")\nend",
            Runtime,
            "string.rpartition expects exactly one separator",
        ),
        (
            "def run\n  \"a\".clamp(\"b\", \"a\")\nend",
            Runtime,
            "string.clamp min must be <= max",
        ),
        (
            "def run\n  \"a\".clamp(1, nil)\nend",
            Runtime,
            "string.clamp bounds must be strings or nil",
        ),
        (
            "def run\n  \"a\".between?(1)\nend",
            Runtime,
            "string.between? expects min and max",
        ),
    ]);
}

#[test]
fn money_literals_and_members_use_go_wording() {
    use ErrorClass::Runtime;
    rejects(&[
        (
            "def run\n  money(\"1 US\")\nend",
            Runtime,
            "currency must be 3 letters, got \"US\"",
        ),
        (
            "def run\n  money(\". USD\")\nend",
            Runtime,
            "invalid money amount \". USD\"",
        ),
        (
            "def run\n  money(\"1.2x4 USD\")\nend",
            Runtime,
            "invalid money amount \"1.2x4 USD\"",
        ),
        (
            "def run\n  money(\"1.234 USD\")\nend",
            Runtime,
            "money literal supports at most 2 decimal places: \"1.234 USD\"",
        ),
        (
            "def run\n  money(\"1 2 USD\")\nend",
            Runtime,
            "invalid money literal \"1 2 USD\"",
        ),
        (
            "def run\n  money(\"a\", \"b\")\nend",
            Runtime,
            "money expects a single string literal",
        ),
        (
            "def run\n  money_cents(\"1\", \"USD\")\nend",
            Runtime,
            "money_cents expects integer cents",
        ),
        (
            "def run\n  money_cents(1, :USD)\nend",
            Runtime,
            "money_cents expects currency string",
        ),
        (
            "def run\n  money_cents(1)\nend",
            Runtime,
            "money_cents expects cents and currency",
        ),
        (
            "def run\n  money_cents(1e19, \"USD\")\nend",
            Runtime,
            "money_cents expects integer cents: float 1e+19 is out of integer range",
        ),
        (
            "def run\n  money(\"1.00 USD\").to_s(1)\nend",
            Runtime,
            "money.to_s does not take arguments",
        ),
        (
            "def run\n  money(\"1.00 USD\").between?(1)\nend",
            Runtime,
            "money.between? expects min and max",
        ),
        (
            "def run\n  money(\"1.00 USD\").itself(x: 1)\nend",
            Runtime,
            "money.itself does not accept keyword arguments",
        ),
    ]);
}

#[test]
fn regex_errors_quote_go_syntax_errors_and_name_the_operation() {
    use ErrorClass::{Limit, Runtime};
    rejects(&[
        (
            "def run\n  Regex.match(\"(\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: missing closing ): `(`",
        ),
        (
            "def run\n  Regex.match(\")\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: unexpected ): `)`",
        ),
        (
            "def run\n  Regex.match(\"a*?*\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid nested repetition operator: `*?*`",
        ),
        (
            "def run\n  Regex.match(\"a|+?\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: missing argument to repetition operator: `+?`",
        ),
        (
            "def run\n  Regex.match(\"a{2,1}\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid repeat count: `{2,1}`",
        ),
        (
            "def run\n  Regex.match(\"(?P<na-me>x)\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid named capture: `(?P<na-me>`",
        ),
        (
            "def run\n  Regex.match(\"(?i\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid or unsupported Perl syntax: `(?i`",
        ),
        (
            "def run\n  Regex.match(\"\\\\\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: trailing backslash at end of expression: ``",
        ),
        (
            "def run\n  Regex.match(\"\\\\xZq\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid escape sequence: `\\xZq`",
        ),
        (
            "def run\n  Regex.match(\"\\\\p{Greek\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid character class range: `\\p{Greek`",
        ),
        (
            "def run\n  Regex.match(\"[z-a]\", \"a\")\nend",
            Runtime,
            "Regex.match invalid regex: error parsing regexp: invalid character class range: `z-a`",
        ),
        (
            "def run\n  \"a\".match?(\"[a\")\nend",
            Runtime,
            "string.match? invalid regex: error parsing regexp: missing closing ]: `[a`",
        ),
        (
            "def run\n  /(/i\nend",
            Runtime,
            "regex literal invalid regex: error parsing regexp: missing closing ): `(?i)(`",
        ),
        (
            "def run\n  \"a\".sub(\"(\", \"b\", regex: true)\nend",
            Runtime,
            "string.sub invalid regex: error parsing regexp: missing closing ): `(`",
        ),
        (
            "def run\n  Regex.match(\"x\" * 20000, \"a\")\nend",
            Limit,
            "Regex.match pattern exceeds limit 16384 bytes",
        ),
        (
            "def run\n  Regex.replace(\"a\", \"a\", \"b\" * 1048577)\nend",
            Limit,
            "Regex.replace replacement exceeds limit 1048576 bytes",
        ),
        (
            "def run\n  Regex.replace_all(\"a\" * 1024, \"a\", \"b\" * 1025)\nend",
            Limit,
            "Regex.replace_all output exceeds limit 1048576 bytes",
        ),
        (
            "def run\n  (\"a\" * 1048577) =~ /a/\nend",
            Limit,
            "=~ text exceeds limit 1048576 bytes",
        ),
        (
            "def run\n  case \"a\" * 1048577\n  when /a/ then 1\n  end\nend",
            Limit,
            "regex match text exceeds limit 1048576 bytes",
        ),
        (
            "def run\n  (\"a\" * 1024).gsub(\"a\", \"b\" * 1025)\nend",
            Limit,
            "string.gsub output exceeds limit 1048576 bytes",
        ),
        (
            "def run\n  \"a\".sub(\"a\") { \"b\" * 1048577 }\nend",
            Limit,
            "output exceeds limit 1048576 bytes",
        ),
        (
            "def run\n  /a/ =~ /a/\nend",
            Runtime,
            "=~ expects a string and a regex operand",
        ),
        (
            "def run\n  Regex.match(\"a\")\nend",
            Runtime,
            "Regex.match expects pattern and text",
        ),
        (
            "def run\n  Regex.replace_all(\"a\", 1, \"b\")\nend",
            Runtime,
            "Regex.replace_all expects string text, pattern, replacement",
        ),
        (
            "def run\n  /a/.match(1)\nend",
            Runtime,
            "regex.match text must be string",
        ),
        (
            "def run\n  /a/.source(1)\nend",
            Runtime,
            "regex.source does not take arguments",
        ),
        (
            "def run\n  \"a\".match(\"a\", \"x\")\nend",
            Runtime,
            "string.match offset must be integer",
        ),
        (
            "def run\n  \"a\".match(\"a\", x: 1)\nend",
            Runtime,
            "string.match does not accept keyword arguments",
        ),
        (
            "def run\n  \"a\".match?(\"a\", -1)\nend",
            Runtime,
            "string.match? offset must be non-negative integer",
        ),
        (
            "def run\n  \"a\".scan\nend",
            Runtime,
            "string.scan expects exactly one pattern",
        ),
        (
            "def run\n  \"a\".scan(1)\nend",
            Runtime,
            "string.scan pattern must be string or regex",
        ),
        (
            "def run\n  \"a\".gsub!(\"a\", \"b\", x: 1)\nend",
            Runtime,
            "string.gsub! supports only regex keyword",
        ),
        (
            "def run\n  \"a\".sub(/a/, \"b\", regex: true)\nend",
            Runtime,
            "string.sub does not take the regex keyword with a regex pattern",
        ),
        (
            "def run\n  \"a\".sub(\"a\", \"b\") { 1 }\nend",
            Runtime,
            "string.sub cannot take both a replacement argument and a block",
        ),
        (
            "def run\n  \"a\".gsub(\"a\")\nend",
            Runtime,
            "string.gsub expects pattern and replacement",
        ),
        (
            "def run\n  \"a\".sub(\"a\", 1)\nend",
            Runtime,
            "string.sub replacement must be string",
        ),
        (
            "def run\n  \"a\".sub(/(?<x>a)/, \"\\\\k<y>\")\nend",
            Runtime,
            "string.sub undefined group name reference: y",
        ),
        (
            "def run\n  \"a\".gsub(/(?<x>a)/, \"\\\\k<y\")\nend",
            Runtime,
            "string.gsub invalid group name reference format",
        ),
        (
            "def run\n  Regexp.new(1)\nend",
            Runtime,
            "Regexp.new pattern must be string",
        ),
        (
            "def run\n  Regexp.union(\"a\", 1)\nend",
            Runtime,
            "Regexp.union expects string patterns",
        ),
        (
            "def run\n  Regexp.union(\"a\" * 20000)\nend",
            Limit,
            "Regexp.union pattern exceeds limit 16384 bytes",
        ),
        (
            "def run\n  Regexp.quote(\"a\") { 1 }\nend",
            Runtime,
            "Regexp.escape does not accept blocks",
        ),
        (
            "def run\n  Regexp.last_match(1)\nend",
            Runtime,
            "Regexp.last_match does not take arguments",
        ),
    ]);
}

#[test]
fn calls_name_missing_arguments_visibility_and_removed_constructors() {
    use ErrorClass::{Argument, Runtime};
    rejects(&[
        (
            "def add(a, b)\n  a + b\nend\ndef run\n  add(1)\nend",
            Argument,
            "missing argument b",
        ),
        (
            "def one(a)\n  a\nend\ndef run\n  one(1, 2)\nend",
            Argument,
            "unexpected positional arguments",
        ),
        (
            "def add(a, b)\n  a + b\nend\ndef run\n  add(*nil)\nend",
            Runtime,
            "splat argument must be an array, got nil",
        ),
        (
            "def kw(a, x: 0)\n  a\nend\ndef run\n  kw(1, **[1, 2])\nend",
            Runtime,
            "keyword splat argument must be a hash, got array",
        ),
        (
            "class C\n  private def secret\n    1\n  end\nend\ndef run\n  C.new.secret\nend",
            Runtime,
            "private method secret",
        ),
        (
            "class C\n  private\n  def x=(v)\n    1\n  end\nend\ndef run\n  c = C.new\n  c.x = 2\nend",
            Runtime,
            "private method x=",
        ),
        (
            "class C\n  private def ==(o)\n    true\n  end\nend\ndef run\n  C.new != 1\nend",
            Runtime,
            "private method ==",
        ),
        (
            "module M\n  protected\n  def self.f\n    1\n  end\nend\ndef run\n  M.f\nend",
            Runtime,
            "protected method f",
        ),
        (
            "def run\n  proc { 1 }\nend",
            Runtime,
            "proc was removed; executable code is not a value. Define a named function and call it, or attach a block to the call that runs it",
        ),
        (
            "def run\n  Proc.new { 1 }\nend",
            Runtime,
            "Proc.new was removed; executable code is not a value. Define a named function and call it, or attach a block to the call that runs it",
        ),
        (
            "def run\n  money\nend",
            Runtime,
            "money is a method and cannot be used as a value; call it with money(...)",
        ),
        (
            "def run\n  JSON.stringify\nend",
            Runtime,
            "stringify is a method and cannot be used as a value; call it with stringify(...)",
        ),
        (
            "def f(v: Missing)\n  v\nend\ndef run\n  f(1)\nend",
            Runtime,
            "argument v type check failed: unknown type Missing",
        ),
        (
            "def f(v) -> Missing\n  v\nend\ndef run\n  f(1)\nend",
            Runtime,
            "return type check failed for f: unknown type Missing",
        ),
        (
            "enum STATUS\n  A\nend\nenum Status\n  B\nend\ndef f(s: status)\n  s\nend\ndef run\n  f(1)\nend",
            Runtime,
            "argument s type check failed: ambiguous enum type status matches STATUS, Status",
        ),
        (
            "class User\nend\nenum USER\n  A\nend\ndef f(v: user)\n  v\nend\ndef run\n  f(1)\nend",
            Runtime,
            "argument v type check failed: ambiguous type user matches enum USER, class User",
        ),
        ("def run\n  next\nend", Runtime, "next used outside of loop"),
        (
            "module M\nend\ndef run\n  M.new\nend",
            Runtime,
            "module M cannot be instantiated",
        ),
        (
            "def run\n  m = \"a\".match(/(a)(b)?/)\n  m.end(nil)\nend",
            Runtime,
            "match_data.end capture index must be integer",
        ),
        (
            "def run\n  m = \"a\".match(/a/)\n  m.begin\nend",
            Runtime,
            "begin is a method and cannot be used as a value; call it with begin(...)",
        ),
    ]);
}
