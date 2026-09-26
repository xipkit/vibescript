mod common;

use std::time::Instant;
use vibescript::{CallOptions, Engine, Error, ErrorClass, ErrorKind, Limits, Value};

fn program(declarations: &str, body: &str) -> String {
    format!("{declarations}\ndef run(input: any) -> any\n{body}\nend")
}

fn options() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(5_000_000),
            memory_bytes: Some(64 << 20),
            recursion: 32,
        },
        ..CallOptions::default()
    }
}

fn failure(declarations: &str, body: &str) -> Error {
    common::runtime_engine()
        .compile(&program(declarations, body))
        .unwrap()
        .call("run", &[Value::nil()], options())
        .unwrap_err()
}

/// The static diagnostic codes of a body the checker refuses, asserting
/// that each points into the body.
fn refused(declarations: &str, body: &str) -> Vec<String> {
    let source = program(declarations, body);
    let error = common::static_engine().compile(&source).err().unwrap();
    let start = source.find(&format!("\n{body}\nend")).unwrap() + 1;
    for diagnostic in error.diagnostics() {
        assert!(
            (start..start + body.len()).contains(&diagnostic.span.start),
            "{body}: {diagnostic:?}"
        );
    }
    common::codes(&error)
}

/// Calls `target` from the host, where arguments are checked at run time.
fn host_failure(declaration: &str, args: &[Value], keywords: &[(&str, Value)]) -> Error {
    let keywords: Vec<(String, Value)> = keywords
        .iter()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect();
    Engine::new()
        .compile(declaration)
        .unwrap()
        .call_with_keywords("target", args, &keywords, options())
        .unwrap_err()
}

#[test]
fn language_classes_are_independent_of_native_error_categories() {
    for (source, class) in [
        ("1 // 0", ErrorClass::ZeroDivision),
        ("1 % 0", ErrorClass::ZeroDivision),
        ("9223372036854775808 // 0", ErrorClass::ZeroDivision),
        ("1.0.div(0)", ErrorClass::ZeroDivision),
        ("1.remainder(0)", ErrorClass::ZeroDivision),
        ("1.divmod(0)", ErrorClass::ZeroDivision),
        ("money(\"1 USD\") / 0", ErrorClass::Runtime),
        ("Duration.parse(\"1h\") / 0", ErrorClass::ZeroDivision),
        ("1.hours / 0.0", ErrorClass::ZeroDivision),
        ("1.hours / 0.seconds", ErrorClass::ZeroDivision),
        ("1.hours % 0.seconds", ErrorClass::ZeroDivision),
        ("break", ErrorClass::Runtime),
        ("next", ErrorClass::Runtime),
    ] {
        let error = failure("", source);
        assert_eq!(error.class(), Some(class), "{source}: {error}");
        assert!(error.diagnostic.is_some());
    }
    assert_eq!(failure("", "1 // 0").kind, ErrorKind::Arithmetic);
    // The type errors behind the other classes are refused before a
    // program runs.
    for (source, codes) in [
        ("1 + nil", &["V0107"][..]),
        ("1 < nil", &["V0107"]),
        ("[1][\"x\"]", &["V0101"]),
        ("1 < \"x\"", &["V0108"]),
        ("[1] < [2]", &["V0108"]),
        ("1.clamp(\"x\",2)", &["V0101"]),
        ("unknown()", &["V0201"]),
        ("Math.sqrt()", &["V0301"]),
        ("1.div", &["V0301"]),
        ("yield", &["V0308"]),
    ] {
        assert_eq!(refused("", source), codes, "{source}");
    }
    for source in ["1.fdiv(0)", "1.0 / 0"] {
        let output = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_float(), Some(f64::INFINITY));
    }
}

#[test]
fn script_binding_has_argument_errors_before_defaults_or_type_checks() {
    let int = Value::int;
    for (declaration, args, keywords, class) in [
        (
            "def target(a: int) -> int\n a\nend",
            vec![],
            vec![],
            ErrorClass::Argument,
        ),
        (
            "def target(a: int) -> int\n a\nend",
            vec![int(1), int(2)],
            vec![],
            ErrorClass::Argument,
        ),
        (
            "def target(a: int = 1) -> int\n a\nend",
            vec![int(1), int(2)],
            vec![],
            ErrorClass::Argument,
        ),
        (
            "def target(a: int = 1 // 0) -> int\n a\nend",
            vec![int(1), int(2)],
            vec![],
            ErrorClass::Argument,
        ),
        (
            "def target(a: int) -> int\n a\nend",
            vec![Value::bytes("x")],
            vec![],
            ErrorClass::Runtime,
        ),
        (
            "def target(*, a: int) -> int\n a\nend",
            vec![],
            vec![],
            ErrorClass::Argument,
        ),
        (
            "def target(*, a: int) -> int\n a\nend",
            vec![],
            vec![("a", int(1)), ("b", int(2))],
            ErrorClass::Argument,
        ),
        (
            "def target(a: int = 1, *, b: int) -> int\n a\nend",
            vec![],
            vec![("b", int(1)), ("c", int(2))],
            ErrorClass::Argument,
        ),
    ] {
        let error = host_failure(declaration, &args, &keywords);
        assert_eq!(error.class(), Some(class), "{declaration}: {error}");
    }
    // A result of the wrong type and a splat of a non-array are refused
    // before a program runs.
    let source = "def target -> int\n \"x\"\nend";
    let error = common::static_engine().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(error.diagnostics()[0].span.start, source.find('"').unwrap());
    assert_eq!(
        refused("def target(a: int) -> int\n a\nend", "target(*1)"),
        ["V0101"]
    );
}

#[test]
fn operation_guards_expose_the_limit_class_without_changing_native_kinds() {
    for (source, kind) in [
        ("random_id(1025)", ErrorKind::OutputLimit),
        ("JSON.parse(\"?\"*1048577)", ErrorKind::OutputLimit),
        ("JSON.stringify(\"a\"*1048576)", ErrorKind::OutputLimit),
        ("Regex.new(\"a\"*16385)", ErrorKind::Memory),
        ("Regex.new(\"(?:ab){1000}\"*101)", ErrorKind::Memory),
        ("Regex.match(\"a\",\"a\"*1048577)", ErrorKind::Memory),
        ("(\"a\"*20000).scan(\"()\"*1000)", ErrorKind::Memory),
        ("(\"a\"*40000).scan(\"a\")", ErrorKind::OutputLimit),
        ("2**9223372036854775808", ErrorKind::Arithmetic),
        ("(\"1\"*100001).to_i", ErrorKind::Argument),
        (
            "[].values_at(0..9223372036854775807)",
            ErrorKind::OutputLimit,
        ),
        ("Time.at(0).iso8601(101)", ErrorKind::OutputLimit),
        (
            "Time.at(0).strftime(\"%1000000000Y\")",
            ErrorKind::OutputLimit,
        ),
    ] {
        let error = failure("", source);
        assert_eq!(error.kind, kind, "{source}: {error}");
        assert_eq!(error.class(), Some(ErrorClass::Limit), "{source}: {error}");
    }
    assert_eq!(
        failure("def recurse -> any\n recurse\nend", "recurse").class(),
        Some(ErrorClass::Limit)
    );
}

#[test]
fn class_names_and_filters_preserve_the_documented_hierarchy() {
    let classes = [
        ErrorClass::Runtime,
        ErrorClass::Standard,
        ErrorClass::Assertion,
        ErrorClass::Limit,
        ErrorClass::Type,
        ErrorClass::ZeroDivision,
        ErrorClass::LocalJump,
        ErrorClass::Argument,
    ];
    for class in classes {
        for spelling in [
            class.name().to_owned(),
            class.name().to_ascii_lowercase(),
            class.name().to_ascii_uppercase(),
        ] {
            assert_eq!(ErrorClass::from_name(&spelling), Some(class));
        }
        assert!(ErrorClass::Runtime.matches(class));
        assert_eq!(
            ErrorClass::Standard.matches(class),
            class != ErrorClass::Limit
        );
        for exact in &classes[2..] {
            assert_eq!(exact.matches(class), *exact == class);
        }
    }
    assert_eq!(ErrorClass::from_name("eRrOr"), Some(ErrorClass::Runtime));
    assert_eq!(
        ErrorClass::from_name("ſtandardError"),
        Some(ErrorClass::Standard)
    );
    assert_eq!(
        ErrorClass::from_name("AſſertionError"),
        Some(ErrorClass::Assertion)
    );
    for invalid in [
        "",
        " RuntimeError",
        "ArgumentError!",
        "LımıtError",
        "LİmitError",
        "CustomError",
    ] {
        assert_eq!(ErrorClass::from_name(invalid), None, "{invalid}");
    }
}

#[test]
fn host_classes_survive_diagnostics_and_cannot_disguise_current_exhaustion() {
    let mut engine = Engine::new();
    engine.register("failure", |_, _| {
        Err(Error::new(ErrorKind::Host, "host assertion").with_class(ErrorClass::Assertion))
    });
    engine.register("spent", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Err(Error::new(ErrorKind::Host, "replacement").with_class(ErrorClass::Runtime))
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Err(Error::new(ErrorKind::Host, "replacement").with_class(ErrorClass::Limit))
    });
    let script = engine.compile("def run(which: int)\n if which == 0\n failure()\n elsif which == 1\n spent()\n else\n cancel()\n end\nend").unwrap();
    for (input, kind, class) in [
        (0, ErrorKind::Host, Some(ErrorClass::Assertion)),
        (1, ErrorKind::Steps, Some(ErrorClass::Limit)),
        (2, ErrorKind::Cancelled, None),
    ] {
        let error = script
            .call("run", &[Value::int(input)], CallOptions::default())
            .unwrap_err();
        assert_eq!((error.kind, error.class()), (kind, class));
        assert!(error.diagnostic.is_some());
        assert_ne!(error.message, "replacement");
    }
    let error = script
        .call(
            "run",
            &[Value::int(0)],
            CallOptions {
                deadline: Some(Instant::now()),
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!((error.kind, error.class()), (ErrorKind::Deadline, None));
    assert_eq!(Engine::new().compile("(").err().unwrap().class(), None);
}

#[test]
#[cfg(target_pointer_width = "64")]
fn exception_metadata_keeps_values_small() {
    assert_eq!(std::mem::size_of::<Error>(), 72);
    assert_eq!(std::mem::size_of::<Value>(), 16);
}
