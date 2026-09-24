//! Ports of the Go REPL's model tests (cmd/vibes/repl_test.go) at the session
//! level, followed by tests for the behavior this session adds.

use super::*;
use vibescript::{HostMethod, Limits};

/// The limits `vibes repl` selects by default: the xhigh profile.
fn xhigh() -> Limits {
    Limits {
        steps: None,
        memory_bytes: None,
        recursion: 10_000,
    }
}

fn session() -> ReplSession {
    ReplSession::new(ReplOptions {
        limits: xhigh(),
        ..ReplOptions::default()
    })
}

fn last(session: &ReplSession) -> &Entry {
    session
        .transcript()
        .last()
        .expect("expected a transcript entry")
}

fn feed_all(session: &mut ReplSession, lines: &[&str]) -> Vec<Response> {
    lines.iter().map(|line| session.feed_line(line)).collect()
}

#[test]
fn evaluation_uses_session_cancellation() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut s = ReplSession::new(ReplOptions {
        limits: xhigh(),
        cancellation,
    });
    let evaluation = s.evaluate("i = 0\nwhile i >= 0\n  i = i + 1\nend");
    assert!(evaluation.is_error(), "{evaluation:?}");
    assert!(
        evaluation.output.contains("execution cancelled"),
        "{evaluation:?}"
    );
    assert!(s.is_cancelled());
}

#[test]
fn quit_command_returns_quit() {
    let mut s = session();
    assert!(matches!(s.feed_line(":quit"), Response::Quit));
    assert!(matches!(s.feed_line(":q"), Response::Quit));
    assert!(s.transcript().is_empty());
}

#[test]
fn non_quit_command_does_not_quit() {
    let mut s = session();
    assert!(matches!(s.feed_line(":help"), Response::ToggleHelp));
    assert!(matches!(s.feed_line(":h"), Response::ToggleHelp));
    assert!(matches!(s.feed_line(":vars"), Response::ToggleVariables));
}

struct EvaluateCase {
    name: &'static str,
    setup: fn(&mut ReplSession),
    input: &'static str,
    want_error: bool,
    check: fn(&ReplSession, &str),
    /// A substring the output must contain.
    want_output: &'static str,
    /// A substring the last error must contain when the input fails.
    error_in_last: &'static str,
}

#[test]
fn evaluate() {
    fn none(_: &mut ReplSession) {}
    fn nothing(_: &ReplSession, _: &str) {}
    let cases = [
        EvaluateCase {
            name: "assignment_stores_variable",
            setup: none,
            input: "score = 42",
            want_error: false,
            check: |s, _| assert_eq!(s.variables()["score"].as_int(), Some(42)),
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "destructuring_assignment_stores_variables",
            setup: none,
            input: "first, *rest, last = [1, 2, 3, 4]",
            want_error: false,
            check: |s, _| {
                assert_eq!(s.variables()["first"].as_int(), Some(1));
                let rest: Vec<_> = s.variables()["rest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(Value::as_int)
                    .collect();
                assert_eq!(rest, [Some(2), Some(3)]);
                assert_eq!(s.variables()["last"].as_int(), Some(4));
            },
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "equality_does_not_overwrite_variable",
            setup: |s| {
                s.variables_mut().insert("a".to_owned(), Value::int(5));
            },
            input: "a == 5",
            want_error: false,
            check: |s, _| assert_eq!(s.variables()["a"].as_int(), Some(5)),
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "sets_underscore_to_last_result",
            setup: none,
            input: "40 + 2",
            want_error: false,
            check: |s, _| assert_eq!(s.variables()["_"].as_int(), Some(42)),
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "compile_error_returns_error",
            setup: none,
            input: "def broken(",
            want_error: true,
            check: |_, output| assert!(!output.is_empty(), "expected a compile error"),
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "runtime_error_returns_error",
            setup: none,
            input: "unknown_var",
            want_error: true,
            check: nothing,
            want_output: "undefined variable",
            error_in_last: "runtime error:",
        },
        EvaluateCase {
            name: "puts_writes_to_history",
            setup: none,
            input: "puts \"hi\"",
            want_error: false,
            check: |_, output| assert_eq!(output, "hi"),
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "print_writes_to_history",
            setup: none,
            input: "print \"hi\"",
            want_error: false,
            check: |_, output| assert_eq!(output, "hi"),
            want_output: "",
            error_in_last: "",
        },
        EvaluateCase {
            name: "warn_writes_to_history",
            setup: none,
            input: "warn \"careful\"",
            want_error: false,
            check: |_, output| assert_eq!(output, "careful"),
            want_output: "",
            error_in_last: "",
        },
    ];
    for case in cases {
        let mut s = session();
        (case.setup)(&mut s);
        let evaluation = s.evaluate(case.input);
        assert_eq!(
            evaluation.is_error(),
            case.want_error,
            "{}: {evaluation:?}",
            case.name
        );
        assert!(
            evaluation.output.contains(case.want_output),
            "{}: {evaluation:?}",
            case.name
        );
        if !case.error_in_last.is_empty() {
            let error = s.last_error().expect("expected the last error");
            assert!(error.contains(case.error_in_last), "{}: {error}", case.name);
        }
        (case.check)(&s, &evaluation.output);
    }
}

#[test]
fn evaluate_errors_use_repl_source() {
    for (input, kind, wants, rejects) in [
        (
            "y = (",
            "compile error:",
            &["parse error at 1:", "y = (", "unexpected end of snippet"][..],
            &["__repl__", "line 3"][..],
        ),
        (
            "1 / 0",
            "runtime error:",
            &["division by zero", "line 1", "at <repl> (1:"],
            &["__repl__", "<script>"],
        ),
    ] {
        let mut s = session();
        let evaluation = s.evaluate(input);
        let output = &evaluation.output;
        assert!(evaluation.is_error(), "{input}: {output}");
        assert!(output.contains(kind), "{input}: {output}");
        for want in wants {
            assert!(output.contains(want), "{input}: {output:?} lacks {want:?}");
        }
        for reject in rejects {
            assert!(
                !output.contains(reject),
                "{input}: {output:?} has {reject:?}"
            );
        }
    }
}

/// A runaway loop under a low step budget must fail fast instead of freezing
/// the session.
#[test]
fn quota_is_configurable() {
    let mut s = ReplSession::new(ReplOptions {
        limits: Limits::default(),
        ..ReplOptions::default()
    });
    let evaluation = s.evaluate("i = 0\nwhile i < 100000000\n  i = i + 1\nend\ni");
    assert!(evaluation.is_error(), "{evaluation:?}");
    assert!(
        evaluation.output.contains("step quota exceeded"),
        "{evaluation:?}"
    );
}

#[test]
fn last_error_command_shows_previous_error() {
    let mut s = session();
    assert!(s.evaluate("unknown_var").is_error());
    let Response::Command(entry) = s.command(":last_error") else {
        panic!("expected an entry");
    };
    assert!(entry.is_error);
    assert!(entry.output.contains("runtime error:"), "{}", entry.output);
    assert_eq!(last(&s), &entry);
}

#[test]
fn last_error_command_when_no_error() {
    let mut s = session();
    s.command(":last_error");
    let entry = last(&s);
    assert!(!entry.is_error);
    assert_eq!(entry.output, "No previous error");
}

#[test]
fn globals_command_prints_sorted_globals() {
    let mut s = session();
    s.variables_mut()
        .insert("zeta".to_owned(), Value::bytes("last"));
    s.variables_mut().insert("alpha".to_owned(), Value::int(1));
    s.command(":globals");
    let entry = last(&s);
    assert!(!entry.is_error);
    assert_eq!(entry.output, "alpha = 1\nzeta = last");
}

#[test]
fn functions_command_lists_builtins_and_env_callables() {
    let mut s = session();
    let worker = HostMethod::new("worker.call", |_, _, _| Ok(Value::bytes("ok"))).value();
    s.variables_mut().insert("worker".to_owned(), worker);
    s.variables_mut().insert("count".to_owned(), Value::int(1));
    s.command(":functions");
    let entry = last(&s);
    assert!(!entry.is_error);
    let mut want = s.builtin_functions().to_vec();
    want.push("worker".to_owned());
    want.sort();
    assert_eq!(entry.output, want.join("\n"));
    assert!(!entry.output.contains("count"), "{}", entry.output);
}

#[test]
fn types_command_shows_kinds() {
    let mut s = session();
    s.variables_mut().insert("count".to_owned(), Value::int(1));
    s.variables_mut()
        .insert("name".to_owned(), Value::bytes("alex"));
    s.command(":types");
    let entry = last(&s);
    assert!(!entry.is_error);
    assert!(entry.output.contains("count: int"), "{}", entry.output);
    assert!(entry.output.contains("name: string"), "{}", entry.output);
}

#[test]
fn autocomplete() {
    for (name, variable, input, want_value, want_listing) in [
        ("single_completion", None, "requ", "require", false),
        (
            "runtime_namespace_completion",
            None,
            "Dur",
            "Duration",
            false,
        ),
        (
            "qualified_builtin_completion",
            None,
            "JSON.parse_a",
            "JSON.parse_as",
            false,
        ),
        (
            "namespace_constant_completion",
            None,
            "Math.P",
            "Math.PI",
            false,
        ),
        ("parser_keyword_completion", None, "unle", "unless", false),
        (
            "multiple_completions_add_history_entry",
            None,
            "m",
            "",
            true,
        ),
        (
            "uses_env_variables",
            Some(("tenant_id", "acme")),
            "tenant",
            "tenant_id",
            false,
        ),
        ("completes_commands", None, ":gl", ":globals", false),
    ] {
        let mut s = session();
        if let Some((key, value)) = variable {
            s.variables_mut()
                .insert(key.to_owned(), Value::bytes(value));
        }
        let completion = s.complete(input);
        if want_listing {
            let Completion::Candidates(names) = completion else {
                panic!("{name}: {completion:?}");
            };
            assert!(names.iter().any(|n| n == "money"), "{name}: {names:?}");
            let entry = last(&s);
            assert!(entry.output.starts_with("Completions: "), "{name}");
            assert!(entry.output.contains("money"), "{name}: {entry:?}");
            assert_eq!(entry.input, "");
        } else {
            assert_eq!(
                completion,
                Completion::Completed(want_value.to_owned()),
                "{name}"
            );
            assert!(s.transcript().is_empty(), "{name}");
        }
    }
}

// The tests below cover behavior the Go REPL does not have.

#[test]
fn unfinished_input_continues_until_complete() {
    let mut s = session();
    for line in ["def add(a,", "        b)", "  a + b"] {
        assert!(
            matches!(s.feed_line(line), Response::NeedsMoreInput),
            "{line}"
        );
    }
    assert_eq!(s.pending(), ["def add(a,", "        b)", "  a + b"]);
    let Response::Evaluated(evaluation) = s.feed_line("end") else {
        panic!("expected an evaluation");
    };
    assert_eq!(evaluation.input, "def add(a,\n        b)\n  a + b\nend");
    assert_eq!(evaluation.output, "nil");
    assert!(s.pending().is_empty());
    let Response::Evaluated(evaluation) = s.feed_line("add(2, 3)") else {
        panic!("expected an evaluation");
    };
    assert_eq!(evaluation.result.unwrap().as_int(), Some(5));
    for unfinished in ["[1,", "{a: 1,", "x = 1 +", "\"open", "if true", "foo."] {
        let mut s = session();
        assert!(
            matches!(s.feed_line(unfinished), Response::NeedsMoreInput),
            "{unfinished}"
        );
        assert!(s.discard_pending());
        assert!(!s.discard_pending());
    }
    for finished in ["end", "1 +* 2", "x = )", "foo(1))"] {
        assert!(
            matches!(session().feed_line(finished), Response::Evaluated(_)),
            "{finished}"
        );
    }
}

#[test]
fn declarations_persist_across_inputs() {
    let mut s = session();
    feed_all(
        &mut s,
        &[
            "rate = 3",
            "def scale(n)",
            "  n * rate",
            "end",
            "class Counter",
            "  def initialize",
            "    @n = 0",
            "  end",
            "  def bump",
            "    @n += 1",
            "  end",
            "end",
            "module Tax",
            "  RATE = 2",
            "end",
            "enum Level",
            "  Low",
            "  High",
            "end",
            "counter = Counter.new",
            "counter.bump",
            "[scale(2), counter.bump, Tax::RATE, Level::High]",
        ],
    );
    assert_eq!(last(&s).output, "[6, 2, 2, Level::High]");
    let declared: Vec<_> = s.declarations().collect();
    assert_eq!(
        declared,
        [
            (DeclarationKind::Function, "scale"),
            (DeclarationKind::Class, "Counter"),
            (DeclarationKind::Enum, "Level"),
            (DeclarationKind::Module, "Tax"),
        ]
    );
    assert!(s.functions().split('\n').any(|name| name == "scale"));
    // Instances and enum members keep matching their types, and variables
    // list no declarations.
    feed_all(
        &mut s,
        &[
            "def total(c: Counter) -> int",
            "  c.bump",
            "end",
            "[counter.is_a?(Counter), total(counter), Level::High == Level::High]",
        ],
    );
    assert_eq!(last(&s).output, "[true, 3, true]");
    assert!(!s.variables().contains_key("Counter"));
    // A redefinition replaces the earlier declaration.
    feed_all(&mut s, &["def scale(n)", "  n * 10", "end", "scale(2)"]);
    assert_eq!(last(&s).output, "20");
    feed_all(
        &mut s,
        &["class Counter", "  def bump", "    99", "  end", "end"],
    );
    feed_all(&mut s, &["[Counter.new.bump, counter.bump]"]);
    assert_eq!(last(&s).output, "[99, 4]");
    assert_eq!(s.declarations().count(), 5);
    s.feed_line(":reset");
    assert_eq!(last(&s).output, "Environment reset");
    s.feed_line("scale(2)");
    assert!(last(&s).is_error, "{:?}", last(&s));
    assert!(s.variables().is_empty());
}

#[test]
fn failures_leave_the_session_unchanged() {
    let mut s = session();
    s.feed_line("x = 1");
    s.feed_line("x = 2; def helper; 1; end; missing_name");
    assert!(last(&s).is_error);
    assert_eq!(s.variables()["x"].as_int(), Some(1));
    assert_eq!(s.declarations().count(), 0);
    // Mutations persist once an input succeeds, unlike the Go REPL.
    feed_all(&mut s, &["items = [1]", "items.push(2)", "items"]);
    assert_eq!(last(&s).output, "[1, 2]");
}

#[test]
fn errors_keep_diagnostics_in_the_typed_text() {
    let mut s = session();
    feed_all(&mut s, &["x = 1", "def ratio(n)", "  n / 0", "end"]);
    let Response::Evaluated(evaluation) = s.feed_line("y = ratio(4)") else {
        panic!("expected an evaluation");
    };
    assert_eq!(
        evaluation.output,
        "runtime error: division by zero\n  --> line 2, column 5\n 2 |   n / 0\n   |     ^\n  \
         at ratio (2:5)\n  at ratio (1:5)"
    );
    let error = evaluation.result.unwrap_err();
    let diagnostic = error.diagnostic.unwrap();
    assert_eq!(
        (diagnostic.position.line, diagnostic.position.column),
        (2, 5)
    );
    assert_eq!(diagnostic.frames[1].position.column, 5);
    s.feed_line("ratio(");
    let Response::Evaluated(evaluation) = s.feed_line(")) ") else {
        panic!("expected an evaluation");
    };
    assert_eq!(
        evaluation.output,
        "compile error: parse error at 2:2: unexpected token \")\"\n  --> line 2, column 2\n \
         2 | ))\n   |  ^"
    );
    let error = evaluation.result.unwrap_err();
    assert_eq!(error.kind, vibescript::ErrorKind::Syntax);
    let error = s.evaluate("[1,").result.unwrap_err();
    assert_eq!(error.message, "unexpected end of snippet");
}

#[test]
fn top_level_aliases_can_name_carried_functions() {
    let mut s = session();
    feed_all(
        &mut s,
        &["def double(n)", "  n * 2", "end", "alias twice double"],
    );
    s.feed_line("twice(4)");
    assert_eq!(last(&s).output, "8");
}

#[test]
fn history_recalls_whole_inputs() {
    let mut s = session();
    assert_eq!(s.history_back(), None);
    assert_eq!(s.history_forward(), None);
    feed_all(&mut s, &["1 + 1", "if true", "  2", "end", ":globals"]);
    assert_eq!(s.history(), ["1 + 1", "if true\n  2\nend"]);
    assert_eq!(s.history_back().as_deref(), Some("end"));
    assert_eq!(s.pending(), ["if true", "  2"]);
    assert_eq!(s.history_back().as_deref(), Some("1 + 1"));
    assert!(s.pending().is_empty());
    assert_eq!(s.history_back().as_deref(), Some("1 + 1"));
    assert_eq!(s.history_forward().as_deref(), Some("end"));
    assert_eq!(s.history_forward().as_deref(), Some(""));
    assert!(s.pending().is_empty());
    assert_eq!(s.history_forward(), None);
    assert_eq!(s.history_back().as_deref(), Some("end"));
    let Response::Evaluated(evaluation) = s.feed_line("end") else {
        panic!("expected an evaluation");
    };
    assert_eq!(evaluation.output, "2");
}

#[test]
fn commands_follow_the_go_repl() {
    let mut s = session();
    assert!(matches!(s.feed_line("   "), Response::Ignored));
    let Response::Command(entry) = s.feed_line(":bogus extra") else {
        panic!("expected an entry");
    };
    assert_eq!(
        entry,
        Entry {
            input: ":bogus extra".to_owned(),
            output: "Unknown command: :bogus".to_owned(),
            is_error: true,
        }
    );
    s.feed_line(":g");
    assert_eq!(last(&s).output, "No globals defined");
    s.feed_line(":t");
    assert_eq!(last(&s).output, "No globals defined");
    assert!(matches!(s.feed_line(":clear"), Response::Cleared));
    assert!(s.transcript().is_empty());
    s.feed_line("x = nil");
    s.feed_line(":globals");
    assert_eq!(last(&s).output, "_ = \nx = ");
    assert_eq!(s.history(), ["x = nil"]);
    s.clear_transcript();
    assert!(s.transcript().is_empty());
}

#[test]
fn outputs_combine_printed_text_and_results_like_the_go_repl() {
    let mut s = session();
    for (input, output) in [
        ("puts \"a\"; 5", "a\n5"),
        ("puts \"\"", ""),
        ("p 5", "5\n5"),
        ("p \"s\"", "\"s\"\ns"),
        ("warn \"w\"; 7", "w\n7"),
        ("nil", "nil"),
        ("[1, nil, \"a\", :b, 2.5]", "[1, , a, b, 2.5]"),
        ("{a: nil, b: 1e-7}", "{a: , b: 1e-07}"),
        (
            "[1.0 / 0, 1e21, 123456789.0]",
            "[Infinity, 1e+21, 1.23456789e+08]",
        ),
        ("money(\"12.50 USD\")", "12.50 USD"),
        ("90.seconds", "90s"),
        ("1...5", "1...5"),
        ("Time.at(1.5).utc", "1970-01-01T00:00:01.5Z"),
    ] {
        s.feed_line(input);
        assert_eq!(last(&s).output, output, "{input}");
    }
}

#[test]
fn completion_includes_declarations_and_ignores_trailing_space() {
    let mut s = session();
    feed_all(&mut s, &["def compute_total", "  1", "end"]);
    assert_eq!(
        s.complete("x = compute_t"),
        Completion::Completed("x = compute_total".to_owned())
    );
    assert_eq!(s.complete("compute "), Completion::None);
    assert_eq!(s.complete(""), Completion::None);
    assert_eq!(s.complete("zzz_nothing"), Completion::None);
}

#[test]
fn chunks_feed_one_line_at_a_time() {
    let mut s = session();
    let responses = s.feed("def triple(n)\r\n  n * 3\rend\ntriple(2)\n");
    assert_eq!(responses.len(), 4);
    assert!(matches!(responses[0], Response::NeedsMoreInput));
    let Response::Evaluated(evaluation) = &responses[3] else {
        panic!("expected an evaluation");
    };
    assert_eq!(evaluation.output, "6");
    s.feed("[1,\n2");
    let evaluation = s.finish().expect("pending input");
    assert!(evaluation.is_error());
    assert!(evaluation.output.contains("unexpected end of snippet"));
    assert!(s.finish().is_none());
}
