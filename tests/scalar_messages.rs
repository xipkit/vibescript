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
        memory,
        ErrorClass::Limit,
        "memory quota exceeded (8192 bytes)",
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
