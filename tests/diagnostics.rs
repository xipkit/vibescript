use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, Error, ErrorKind, Position, Value};

fn failure(source: &str) -> Error {
    Engine::new()
        .compile(source)
        .unwrap()
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap_err()
}

fn check_position(error: &Error, source: &str, offset: usize) {
    assert_eq!(error.offset, Some(offset), "{error}");
    let before = &source[..offset];
    let position = Position {
        line: before.bytes().filter(|&b| b == b'\n').count() + 1,
        column: before.rsplit('\n').next().unwrap().chars().count() + 1,
    };
    assert_eq!(
        error.diagnostic.as_ref().unwrap().position,
        position,
        "{error}"
    );
}

#[test]
fn failures_point_at_operators_members_indices_and_nested_calls() {
    for (body, needle) in [
        ("1 / 0", "/"),
        ("[1][\"bad\"]", "[\"bad\"]"),
        ("\"x\".missing", "\"x\""),
        ("a = 1 + nil", "+"),
        (
            "row = {values:[1]}; row.values.push().missing",
            "row.values",
        ),
    ] {
        let source = format!("def run(input)\n  {body}\nend");
        let error = failure(&source);
        check_position(&error, &source, source.find(needle).unwrap());
        assert_eq!(error.diagnostic.as_ref().unwrap().frames.len(), 2);
    }
    let source = "def divide(a,b)\n a / b\nend\ndef run(input)\n divide(1,0)\nend";
    let error = failure(source);
    let frames = &error.diagnostic.as_ref().unwrap().frames;
    assert_eq!(
        frames
            .iter()
            .map(|frame| (&*frame.function, frame.position.line, frame.position.column))
            .collect::<Vec<_>>(),
        [("divide", 2, 4), ("divide", 5, 2), ("run", 4, 1)]
    );
    assert_eq!(
        error.to_string(),
        "division by zero\n  --> line 2, column 4\n 2 |  a / b\n   |    ^\n  at divide (2:4)\n  at divide (5:2)\n  at run (4:1)"
    );
}

#[test]
fn interpolation_and_unicode_use_the_original_source() {
    for source in [
        "def run(input)\n  \"hello #{1/0}!\"\nend",
        "def run(input)\n  α=1; α/0\nend",
        "def run(input)\n  [1].map { |n| n/0 }\nend",
        "def run(input)\n  %W(hello #{1/0})\nend",
    ] {
        let error = failure(source);
        check_position(&error, source, source.find('/').unwrap());
        assert_eq!(
            &*error.diagnostic.as_ref().unwrap().frames[0].function,
            "run"
        );
    }
}

#[test]
fn argument_and_return_checks_point_to_the_calling_expression() {
    for declaration in [
        "def target(a:int)\n a\nend",
        "def target(a:int=\"x\")\n a\nend",
        "def target -> int\n \"x\"\nend",
        "def target -> int\n return \"x\"\nend",
        "def target -> int\n [1].each { return \"x\" }\nend",
        "def target -> int\n yield\nend",
        "def target(a=[1].each { return \"x\" }) -> int\n a\nend",
    ] {
        let call = if declaration.starts_with("def target(a:int)") {
            "target(\"x\")"
        } else if declaration.contains("yield") {
            "target { break \"x\" }"
        } else {
            "target()"
        };
        let source = format!("{declaration}\ndef run(input)\n  {call}\nend");
        let error = failure(&source);
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
        check_position(&error, &source, source.rfind(call).unwrap());
        assert_eq!(
            error
                .diagnostic
                .as_ref()
                .unwrap()
                .frames
                .iter()
                .map(|f| &*f.function)
                .collect::<Vec<_>>(),
            ["run", "run"],
            "{source}: {error}"
        );
    }
    let source = "class C\n property value:int\nend\ndef run(input)\n C.new.value=\"x\"\nend";
    let error = failure(source);
    check_position(&error, source, source.find("C.new").unwrap());
    assert_eq!(error.diagnostic.as_ref().unwrap().frames.len(), 2);
}

#[test]
fn default_expressions_keep_their_locations_without_inventing_callee_frames() {
    for default in ["1/0", "[1].map { |n| n/0 }"] {
        let source = format!("def target(a={default})\n a\nend\ndef run(input)\n target()\nend");
        let error = failure(&source);
        check_position(&error, &source, source.find('/').unwrap());
        assert_eq!(
            error
                .diagnostic
                .as_ref()
                .unwrap()
                .frames
                .iter()
                .map(|f| &*f.function)
                .collect::<Vec<_>>(),
            ["run", "run"]
        );
    }
    let source = "def bad\n 1/0\nend\ndef target(a=bad)\n a\nend\ndef run(input)\n target()\nend";
    let error = failure(source);
    let frames = &error.diagnostic.as_ref().unwrap().frames;
    assert_eq!(
        frames.iter().map(|f| &*f.function).collect::<Vec<_>>(),
        ["bad", "bad", "run"]
    );
    assert_eq!(
        frames[1].position,
        Position {
            line: 4,
            column: 14
        }
    );
}

#[test]
fn entry_checks_and_initializers_have_script_context() {
    for source in [
        "def run(input:int)\n input\nend",
        "def run(input) -> int\n \"x\"\nend",
        "class C\n X=1/0\nend\ndef run(input)\n C\nend",
        "module C\n X=1/0\nend\ndef run(input)\n C\nend",
    ] {
        let error = failure(source);
        let frames = &error.diagnostic.as_ref().unwrap().frames;
        assert_eq!(frames.len(), 1, "{source}: {error}");
        assert_eq!(&*frames[0].function, "<script>");
    }
}

#[test]
fn nested_blocks_use_the_active_call_stack_and_to_s_uses_its_source_expression() {
    let source =
        "def target\n [1].each { yield }\nend\ndef run(input)\n target { [1].map { 1/0 } }\nend";
    let error = failure(source);
    check_position(&error, source, source.find('/').unwrap());
    assert_eq!(
        error
            .diagnostic
            .as_ref()
            .unwrap()
            .frames
            .iter()
            .map(|f| &*f.function)
            .collect::<Vec<_>>(),
        ["target", "target", "run"]
    );
    let source = "class C\n def to_s\n  1/0\n end\nend\ndef run(input)\n \"hello #{C.new}!\"\nend";
    let error = failure(source);
    check_position(&error, source, source.find('/').unwrap());
    let call = &error.diagnostic.as_ref().unwrap().frames[1];
    assert_eq!(&*call.function, "to_s");
    assert_eq!(
        call.position,
        Position {
            line: 7,
            column: 11
        }
    );
}

#[test]
fn invalid_tokens_are_highlighted_before_the_parser_advances() {
    for (source, token) in [
        ("def run(input)\n a.123\nend", "123"),
        ("def run(input)\n a&.123\nend", "123"),
        ("def run(input)\n A::123\nend", "123"),
        ("def run(input)\n (1 + )\nend", ")\n"),
        ("def 123\nend", "123"),
        ("enum 123\nend", "123"),
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        check_position(&error, source, source.rfind(token).unwrap());
    }
    let source = "def run(input)\n 1";
    let error = Engine::new().compile(source).err().unwrap();
    check_position(&error, source, source.len());
}

#[test]
fn host_forwarded_script_errors_preserve_the_original_diagnostic() {
    let source = "def run(input)\n 1/0\nend";
    let original = failure(source);
    let forwarded = original.clone();
    let mut engine = Engine::new();
    engine.register("fail", move |_, _| Err(forwarded.clone()));
    let error = engine
        .compile("def run\n fail()\nend")
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error, original);
    assert!(Arc::ptr_eq(
        error.diagnostic.as_ref().unwrap(),
        original.diagnostic.as_ref().unwrap()
    ));
}

#[test]
fn long_lines_and_many_lines_keep_snippets_bounded() {
    for prefix in ["\n".repeat(9000), format!("\"{}\";", "α".repeat(10000))] {
        let source = format!("def run(input)\n{prefix} 1/0\nend");
        let error = failure(&source);
        check_position(&error, &source, source.find('/').unwrap());
        let diagnostic = error.diagnostic.as_ref().unwrap();
        assert!(
            diagnostic.code_frame.len() < 1024,
            "{}",
            diagnostic.code_frame.len()
        );
        assert!(diagnostic.code_frame.contains("1/0"));
        assert!(Arc::ptr_eq(
            error.clone().diagnostic.as_ref().unwrap(),
            diagnostic
        ));
    }
}

#[test]
fn syntax_errors_publish_positions_without_script_frames() {
    let source = "def run(input)\n  $\nend";
    let error = Engine::new().compile(source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    check_position(&error, source, source.find('$').unwrap());
    let diagnostic = error.diagnostic.as_ref().unwrap();
    assert!(diagnostic.frames.is_empty());
    assert!(error.to_string().starts_with("parse error at 2:3:"));
    let too_large = "\n".repeat((8 << 20) + 1);
    let error = Engine::new().compile(&too_large).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert!(error.diagnostic.is_none());
}

#[test]
fn errors_do_not_retain_compiled_scripts_or_their_host_state() {
    let state = Arc::new(AtomicUsize::new(0));
    let held = state.clone();
    let mut engine = Engine::new();
    engine.register("fail", move |_, _| {
        held.fetch_add(1, Ordering::Relaxed);
        Err(Error::new(ErrorKind::Host, "host failed"))
    });
    let source = "def run(input)\n  fail()\nend";
    let script = engine.compile(source).unwrap();
    let error = script
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap_err();
    drop(script);
    drop(engine);
    assert_eq!(Arc::strong_count(&state), 1);
    assert_eq!(state.load(Ordering::Relaxed), 1);
    check_position(&error, source, source.find("fail()").unwrap());
    assert!(error.to_string().contains("host failed"));
}

#[test]
fn cancellation_and_ignored_exhaustion_keep_the_host_call_position() {
    for cancel in [false, true] {
        let token = vibescript::CancellationToken::new();
        let signal = token.clone();
        let mut engine = Engine::new();
        engine.register("stop", move |ctx, _| {
            if cancel {
                signal.cancel();
                let _ = ctx.checkpoint();
            } else {
                let _ = ctx.charge(u64::MAX);
            }
            Ok(Value::nil())
        });
        let source = "def run(input)\n  stop()\n  missing()\nend";
        let script = engine.compile(source).unwrap();
        let error = script
            .call(
                "run",
                &[Value::nil()],
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(
            error.kind,
            if cancel {
                ErrorKind::Cancelled
            } else {
                ErrorKind::Steps
            }
        );
        check_position(&error, source, source.find("stop()").unwrap());
    }
}

#[test]
fn recursion_keeps_structured_frames_and_shortens_only_the_rendering() {
    let source = "def recurse\n  recurse\nend\ndef run(input)\n  recurse\nend";
    let script = Engine::new().compile(source).unwrap();
    let mut options = CallOptions::default();
    options.limits.recursion = 32;
    let error = script.call("run", &[Value::nil()], options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    let frames = &error.diagnostic.as_ref().unwrap().frames;
    assert_eq!(frames.len(), 33);
    assert_eq!(error.to_string().matches("\n  at ").count(), 16);
    assert!(error.to_string().contains("17 frames omitted"));
    assert!(Arc::ptr_eq(&frames[0].function, &frames[1].function));
}
