use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Diagnostic, Engine, Error, ErrorKind, Position, StackFrame, Value};

mod common;

fn failure(source: &str) -> Error {
    Engine::new()
        .compile(source)
        .unwrap()
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap_err()
}

/// The code and offset of each static diagnostic that refuses `source`.
fn refused(source: &str) -> Vec<(String, usize)> {
    let error = common::static_engine()
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| (d.code.to_string(), d.span.start))
        .collect()
}

/// Each code with the offset of the first `text` in `source`.
fn at(source: &str, expected: &[(&str, &str)]) -> Vec<(String, usize)> {
    expected
        .iter()
        .map(|(code, text)| ((*code).to_owned(), source.find(text).unwrap()))
        .collect()
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
    // Two-character operators are stamped at their second character, as Go
    // stamped them, except `//`, which Go lacks, at its first.
    for (body, needle) in [
        ("1 // 0", "// 0"),
        ("[1].fetch(5)", "[1]"),
        ("Math.sqrt(-1)", "Math"),
        ("\"x\" * -1", "* -1"),
        (
            "row = {values:[1]}; row[\"values\"].fetch(3)",
            "[\"values\"]",
        ),
    ] {
        let source = format!("def run(input: any)\n  {body}\nend");
        let error = failure(&source);
        check_position(&error, &source, source.find(needle).unwrap());
        assert_eq!(error.diagnostic.as_ref().unwrap().frames.len(), 2);
    }
    // Indexing an array with a string, an unknown member, arithmetic with
    // nil and empty parentheses are refused before running.
    for (body, expected) in [
        ("[1][\"bad\"]", vec![("V0101", "\"bad\"")]),
        ("\"x\".missing", vec![("V0203", "missing")]),
        ("a = 1 + nil", vec![("V0107", "nil")]),
        (
            "row = {values:[1]}; row.values.push().missing",
            vec![("V0412", "()"), ("V0203", "missing")],
        ),
    ] {
        let source = format!("def run(input: any)\n  {body}\nend");
        assert_eq!(refused(&source), at(&source, &expected), "{body}");
    }
    let source = "def divide(a: int, b: int) -> int\n a // b\nend\ndef run(input: any) -> int\n divide(1,0)\nend";
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
        "division by zero\n  --> line 2, column 4\n 2 |  a // b\n   |    ^\n  at divide (2:4)\n  at divide (5:2)\n  at run (4:1)"
    );
}

#[test]
fn writes_and_their_compound_operators_point_at_the_target() {
    // Each is refused before running, at its target.
    for (body, expected) in [
        (
            "arr = [1, [2]]\n  arr[1].first = 3",
            vec![("V0203", "first =")],
        ),
        ("arr = [1]\n  arr[5] += 2", vec![("V0107", "arr[5]")]),
        ("h = {a: [1]}\n  h.a.first -= 1", vec![("V0415", ".a")]),
        (
            "h = {a: nil}\n  h[:a] **= 2",
            vec![("V0107", "h[:a]"), ("V0409", ":a]")],
        ),
    ] {
        let source = format!("def run(input: any)\n  {body}\nend");
        assert_eq!(refused(&source), at(&source, &expected), "{body}");
    }
}

#[test]
fn class_variable_reads_outside_a_class_have_no_class_context() {
    for body in ["@@x += 3", "@@x ||= 3", "@@x"] {
        let source = format!("def run(input: any)\n  {body}\nend");
        assert_eq!(refused(&source), at(&source, &[("V0204", "@@x")]), "{body}");
    }
    // The checker does not report a write, which fails when it runs.
    let source = "def run(input: any)\n  @@x = 3\nend";
    assert_eq!(failure(source).message, "no class context for class var");
}

#[test]
fn blocks_report_names_outside_their_enclosing_frames() {
    let source =
        "def run(input: any)\n  total = 1\n  [1].each { |item| [2].each { |inner| totl } }\nend";
    assert_eq!(refused(source), at(source, &[("V0201", "totl")]));
}

#[test]
fn binary_operators_are_located_where_the_reference_lexer_stamps_them() {
    let source = "def run(input: any)\n  x = 0 ** -1\nend";
    let error = failure(source);
    assert_eq!(error.message, "float exponentiation result is not finite");
    check_position(&error, source, source.find("* -1").unwrap());
    // Operands the operator does not take and unknown members are refused
    // before running; an operator's diagnostic spans all of it.
    for (body, expected) in [
        ("x = 2 ** nil", vec![("V0107", "nil")]),
        ("x = 1 << -1", vec![("V0108", "<< -1")]),
        ("x = 1 >= nil", vec![("V0107", "nil")]),
        ("x = (1..2).foo", vec![("V0203", "foo")]),
        ("x = (1...2).foo", vec![("V0203", "foo")]),
        ("x = (1 === 2).foo", vec![("V0203", "foo")]),
    ] {
        let source = format!("def run(input: any)\n  {body}\nend");
        assert_eq!(refused(&source), at(&source, &expected), "{body}");
    }
    for (source, message) in [
        ("x = ** 1", "parse error at 1:6: unexpected token \"**\""),
        ("x = != 1", "parse error at 1:6: unexpected token \"!=\""),
        ("x = <=> 1", "parse error at 1:5: unexpected token \"<=>\""),
        ("x = === 1", "parse error at 1:5: unexpected token \"===\""),
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        let position = error.diagnostic.as_ref().unwrap().position;
        assert_eq!(
            format!(
                "parse error at {}:{}: {}",
                position.line, position.column, error.message
            ),
            message
        );
    }
}

#[test]
fn interpolation_and_unicode_use_the_original_source() {
    for source in [
        "def run(input: any)\n  \"hello #{1//0}!\"\nend",
        "def run(input: any)\n  α=1; α//0\nend",
        "def run(input: any)\n  [1].map { |n| n//0 }\nend",
        "def run(input: any)\n  [\"hello\", \"#{1//0}\"]\nend",
    ] {
        let error = failure(source);
        check_position(&error, source, source.rfind("//").unwrap());
        assert_eq!(
            &*error.diagnostic.as_ref().unwrap().frames[0].function,
            "run"
        );
    }
}

#[test]
fn argument_and_return_checks_point_to_the_calling_expression() {
    // What the checker cannot see is checked when it runs, at the call.
    for (declaration, call) in [
        ("def target(a: int = \"x\")\n a\nend", "target()"),
        (
            "def target(a: int = [1].each { return \"x\" }) -> int\n a\nend",
            "target()",
        ),
    ] {
        let source = format!("{declaration}\ndef run(input: any)\n  {call}\nend");
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
    // The rest are refused before running, at the value.
    for (declaration, call, text) in [
        ("def target(a:int)\n a\nend", "target(\"x\")", "\"x\")"),
        ("def target -> int\n \"x\"\nend", "target", "\"x\""),
        ("def target -> int\n return \"x\"\nend", "target", "\"x\""),
        (
            "def target(&block: () -> int) -> int\n yield\nend",
            "target { break \"x\" }",
            "\"x\"",
        ),
        (
            "class C\n property value:int\nend",
            "C.new.value=\"x\"",
            "\"x\"",
        ),
    ] {
        let source = format!("{declaration}\ndef run(input: any)\n  {call}\nend");
        assert_eq!(
            refused(&source),
            at(&source, &[("V0101", text)]),
            "{source}"
        );
    }
}

#[test]
fn default_expressions_keep_their_locations_without_inventing_callee_frames() {
    for (ty, default) in [("int", "1//0"), ("array<int>", "[1].map { |n| n//0 }")] {
        let source = format!(
            "def target(a: {ty} = {default})\n a\nend\ndef run(input: any)\n target()\nend"
        );
        let error = failure(&source);
        check_position(&error, &source, source.rfind("//").unwrap());
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
    let source = "def bad -> int\n 1//0\nend\ndef target(a: int = bad)\n a\nend\ndef run(input: any)\n target()\nend";
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
            column: 21
        }
    );
}

#[test]
fn entry_checks_and_initializers_have_script_context() {
    for source in [
        "def run(input:int)\n input\nend",
        "class C\n X=1//0\nend\ndef run(input: any)\n C\nend",
        "module C\n X=1//0\nend\ndef run(input: any)\n C\nend",
    ] {
        let error = failure(source);
        let frames = &error.diagnostic.as_ref().unwrap().frames;
        assert_eq!(frames.len(), 1, "{source}: {error}");
        assert_eq!(&*frames[0].function, "<script>");
    }
}

#[test]
fn nested_blocks_use_the_active_call_stack_and_to_s_uses_its_source_expression() {
    let source = "def target(&block: ())\n [1].each { yield }\nend\ndef run(input: any)\n target { [1].map { 1//0 } }\nend";
    let error = failure(source);
    check_position(&error, source, source.rfind("//").unwrap());
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
    let source = "class C\n def to_s -> string\n  (1//0).to_s\n end\nend\ndef run(input: any)\n \"hello #{C.new}!\"\nend";
    let error = failure(source);
    check_position(&error, source, source.rfind("//").unwrap());
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
    // Like Go, the end of input is located at the last character, or at
    // column 0 of the line after a final line break.
    for (source, line, column) in [("def run(input)\n 1", 2, 2), ("def run(input)\n 1\n", 3, 0)] {
        let error = Engine::new().compile(source).err().unwrap();
        assert_eq!(error.offset, Some(source.len()), "{error}");
        assert_eq!(
            error.diagnostic.as_ref().unwrap().position,
            Position { line, column },
            "{error}"
        );
    }
}

#[test]
fn host_forwarded_script_errors_preserve_the_original_diagnostic() {
    let source = "def run(input: any)\n 1//0\nend";
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
fn forwarded_filenames_are_charged_once_per_shared_allocation() {
    let filename: Arc<[u8]> = vec![b'x'; 32768].into();
    let distinct: Arc<[u8]> = filename.as_ref().into();
    let mut outcomes = Vec::new();
    for (site, labels) in [
        (None, [None, None, None]),
        (
            Some(filename.clone()),
            [
                Some(filename.clone()),
                Some(filename.clone()),
                Some(filename.clone()),
            ],
        ),
        (
            Some(filename.clone()),
            [
                Some(filename.clone()),
                Some(distinct.clone()),
                Some(distinct.clone()),
            ],
        ),
        (None, [None, Some(filename.clone()), Some(filename.clone())]),
    ] {
        let function: Arc<str> = "remote".into();
        let mut original = Error::new(ErrorKind::Runtime, "failed elsewhere");
        original.diagnostic = Some(Arc::new(Diagnostic {
            filename: site,
            position: Position { line: 2, column: 3 },
            code_frame: String::new(),
            frames: labels
                .into_iter()
                .map(|filename| StackFrame {
                    function: function.clone(),
                    filename,
                    position: Position { line: 2, column: 3 },
                })
                .collect(),
        }));
        let forwarded = original.clone();
        let mut engine = Engine::new();
        engine.register("fail", move |_, _| Err(forwarded.clone()));
        engine.register("usage", |ctx, _| {
            Ok(Value::int(ctx.stats().retained_memory_bytes as i64))
        });
        let script = engine
            .compile("begin\nfail()\nrescue\nusage()\nend")
            .unwrap();
        let output = script.run(CallOptions::default()).unwrap();
        assert_eq!(output.stats.retained_memory_bytes, 0);
        let mut options = CallOptions::default();
        options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
        assert_eq!(
            script.run(options.clone()).unwrap().value.as_int(),
            output.value.as_int()
        );
        options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Memory);
        let mut options = CallOptions::default();
        options.limits.steps = Some(output.stats.steps);
        script.run(options.clone()).unwrap();
        options.limits.steps = Some(output.stats.steps - 1);
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Steps);
        outcomes.push(output.value.as_int().unwrap());
        let error = engine
            .compile("fail()")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error, original);
    }
    let single = outcomes[1] - outcomes[0];
    assert!((32768..33792).contains(&single), "{outcomes:?}");
    assert_eq!(outcomes[2] - outcomes[1], single);
    assert_eq!(outcomes[3], outcomes[1]);
}

#[test]
fn long_lines_and_many_lines_keep_snippets_bounded() {
    for prefix in ["\n".repeat(9000), format!("\"{}\";", "α".repeat(10000))] {
        let source = format!("def run(input: any)\n{prefix} 1//0\nend");
        let error = failure(&source);
        check_position(&error, &source, source.rfind("//").unwrap());
        let diagnostic = error.diagnostic.as_ref().unwrap();
        assert!(
            diagnostic.code_frame.len() < 1024,
            "{}",
            diagnostic.code_frame.len()
        );
        assert!(diagnostic.code_frame.contains("1//0"));
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
    let source = "def run(input: any)\n  fail()\nend";
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
        engine.register("later", |_, _| panic!("later ran"));
        let source = "def run(input: any)\n  stop()\n  later()\nend";
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
    let source = "def recurse\n  recurse\nend\ndef run(input: any)\n  recurse\nend";
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

#[test]
fn operator_diagnostics_span_the_whole_operator() {
    for (body, operator) in [
        ("x = 1 <=> \"a\"", "<=>"),
        ("x = true ** 2", "**"),
        ("x = \"a\" <= 3", "<="),
        ("x = 7 // \"a\"", "//"),
        ("x = \"a\" - 1", "-"),
    ] {
        let source = format!("def run(input: any)\n  {body}\nend");
        let error = common::static_engine().compile(&source).err().unwrap();
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.code.to_string(), "V0108", "{body}");
        assert_eq!(
            &source[diagnostic.span.start..diagnostic.span.end],
            operator,
            "{body}"
        );
    }
}
