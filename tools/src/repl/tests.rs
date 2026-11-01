//! Ports of the Go REPL's model tests (cmd/vibes/repl_test.go) at the session
//! level, followed by tests for the behavior this session adds.

use super::*;
use vibescript::{ErrorKind, HostMethod, Limits};

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
        ..ReplOptions::default()
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
            input: "[1].fetch(3)",
            want_error: true,
            check: nothing,
            want_output: "outside of array bounds",
            error_in_last: "runtime error:",
        },
        EvaluateCase {
            name: "undefined_names_are_compile_errors",
            setup: none,
            input: "unknown_var",
            want_error: true,
            check: nothing,
            want_output: "error[V0201]",
            error_in_last: "compile error:",
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
    assert!(s.evaluate("[1].fetch(3)").is_error());
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
    for line in ["def add(a: int,", "        b: int) -> int", "  a + b"] {
        assert!(
            matches!(s.feed_line(line), Response::NeedsMoreInput),
            "{line}"
        );
    }
    assert_eq!(
        s.pending(),
        ["def add(a: int,", "        b: int) -> int", "  a + b"]
    );
    let Response::Evaluated(evaluation) = s.feed_line("end") else {
        panic!("expected an evaluation");
    };
    assert_eq!(
        evaluation.input,
        "def add(a: int,\n        b: int) -> int\n  a + b\nend"
    );
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
            "def scale(n: int, by: int) -> int",
            "  n * by",
            "end",
            "class Counter",
            "  @n: int = 0",
            "  def bump -> int",
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
            "[scale(2, rate), counter.bump, Tax::RATE, Level::High]",
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
    // Instances and enum members keep matching their types, variables keep
    // theirs, and variables list no declarations.
    feed_all(
        &mut s,
        &[
            "def total(c: Counter) -> int",
            "  c.bump",
            "end",
            "[counter.is_type?(:Counter), total(counter), Level::High == Level::High]",
        ],
    );
    assert_eq!(last(&s).output, "[true, 3, true]");
    assert!(!s.variables().contains_key("Counter"));
    s.feed_line("rate = \"three\"");
    assert!(last(&s).output.contains("error[V0102]"), "{:?}", last(&s));
    // A redefinition replaces the earlier declaration.
    feed_all(
        &mut s,
        &["def scale(n: int) -> int", "  n * 10", "end", "scale(2)"],
    );
    assert_eq!(last(&s).output, "20");
    feed_all(
        &mut s,
        &[
            "class Counter",
            "  def bump -> int",
            "    99",
            "  end",
            "end",
        ],
    );
    feed_all(&mut s, &["Counter.new.bump"]);
    assert_eq!(last(&s).output, "99");
    // An instance of the replaced class is no longer a `Counter`, so it is
    // `any` from then on.
    s.feed_line("counter.bump");
    assert!(last(&s).output.contains("error[V0106]"), "{:?}", last(&s));
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
    feed_all(
        &mut s,
        &["x = 1", "def ratio(n: int) -> int", "  n // 0", "end"],
    );
    let Response::Evaluated(evaluation) = s.feed_line("y = ratio(4)") else {
        panic!("expected an evaluation");
    };
    assert_eq!(
        evaluation.output,
        "runtime error: division by zero\n  --> line 2, column 5\n 2 |   n // 0\n   |     ^\n  \
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
        &[
            "def double(n: int) -> int",
            "  n * 2",
            "end",
            "alias twice double",
        ],
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
    s.feed_line("x: int? = nil");
    s.feed_line(":globals");
    assert_eq!(last(&s).output, "_ = \nx = ");
    assert_eq!(s.history(), ["x: int? = nil"]);
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
    feed_all(&mut s, &["def compute_total -> int", "  1", "end"]);
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
    let responses = s.feed("def triple(n: int) -> int\r\n  n * 3\rend\ntriple(2)\n");
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

fn bounded(limits: SessionLimits) -> ReplSession {
    ReplSession::new(ReplOptions {
        limits: xhigh(),
        session_limits: limits,
        ..ReplOptions::default()
    })
}

fn evaluated(response: Response) -> Evaluation {
    let Response::Evaluated(evaluation) = response else {
        panic!("expected evaluation, got {response:?}");
    };
    evaluation
}

#[test]
fn bounded_history_and_transcript_keep_recent_inputs() {
    let mut s = bounded(SessionLimits {
        history_bytes: Some(3 * (size_of::<String>() + 3)),
        transcript_bytes: Some(2 * (size_of::<Entry>() + 6)),
        ..SessionLimits::default()
    });
    for n in 100..200 {
        let input = n.to_string();
        assert_eq!(evaluated(s.feed_line(&input)).output, input);
        assert!(s.history_bytes <= s.limits.history_bytes.unwrap());
        assert!(s.transcript_bytes <= s.limits.transcript_bytes.unwrap());
    }
    assert_eq!(s.history(), ["197", "198", "199"]);
    assert_eq!(
        s.transcript()
            .iter()
            .map(|entry| entry.input.as_str())
            .collect::<Vec<_>>(),
        ["198", "199"]
    );
    assert_eq!(s.history_back().as_deref(), Some("199"));
    assert_eq!(s.history_back().as_deref(), Some("198"));
    assert_eq!(s.history_forward().as_deref(), Some("199"));
    assert_eq!(s.history_forward().as_deref(), Some(""));
    s.clear_history();
    s.clear_transcript();
    assert_eq!((s.history_bytes, s.transcript_bytes), (0, 0));
    assert_eq!((s.history.capacity(), s.transcript.capacity()), (0, 0));
    s.feed_line("200");
    assert_eq!(s.history(), ["200"]);
    assert_eq!(last(&s).output, "200");
}

#[test]
fn transcript_limits_cover_commands_completions_and_oversized_entries() {
    let mut s = bounded(SessionLimits {
        history_bytes: Some(size_of::<String>() + 1),
        transcript_bytes: Some(size_of::<Entry>() + 2),
        ..SessionLimits::default()
    });
    s.feed_line("1");
    let original = last(&s).clone();
    for _ in 0..50 {
        assert!(matches!(s.complete("m"), Completion::Candidates(_)));
        assert!(matches!(s.command(":globals"), Response::Command(_)));
        assert!(!evaluated(s.feed_line("100")).is_error());
    }
    assert_eq!(s.transcript(), [original]);
    assert_eq!(s.history(), ["1"]);
    s.feed_line(":clear");
    assert_eq!(s.transcript_bytes, 0);
    s.feed_line("2");
    assert_eq!(last(&s).output, "2");
    let mut no_storage = bounded(SessionLimits {
        history_bytes: Some(0),
        transcript_bytes: Some(0),
        ..SessionLimits::default()
    });
    no_storage.feed_line("nil");
    no_storage.command(":globals");
    no_storage.complete("m");
    assert!(no_storage.transcript().is_empty());
    assert!(no_storage.history().is_empty());
}

#[test]
fn input_cap_rejects_before_retaining_source_and_releases_pending() {
    let mut s = bounded(SessionLimits {
        pending_input_bytes: Some(12),
        ..SessionLimits::default()
    });
    assert!(matches!(s.feed_line("["), Response::NeedsMoreInput));
    for _ in 0..11 {
        assert!(matches!(s.feed_line(""), Response::NeedsMoreInput));
    }
    let rejected = evaluated(s.feed_line(""));
    assert_eq!(rejected.result.unwrap_err().kind, ErrorKind::Memory);
    assert!(rejected.input.is_empty());
    assert!(s.pending().is_empty());
    assert_eq!(s.pending_bytes, 0);
    assert!(s.history().is_empty());
    assert!(!evaluated(s.feed_line("1")).is_error());
    let steps = s.compilation_steps();
    assert!(
        evaluated(s.feed_line(&"x".repeat(100_000)))
            .input
            .is_empty()
    );
    assert!(s.evaluate(&"x".repeat(100_000)).input.is_empty());
    assert_eq!(s.compilation_steps(), steps);
    assert!(matches!(s.feed_line("[1,"), Response::NeedsMoreInput));
    assert!(s.discard_pending());
    assert!(matches!(s.feed_line("[2,"), Response::NeedsMoreInput));
    s.reset();
    assert!(s.pending().is_empty());
    assert_eq!(s.pending_bytes, 0);
    assert!(!evaluated(s.feed_line("[3]")).is_error());
    // UTF-8 bytes, rather than Unicode scalar count, decide admission.
    assert!(s.evaluate("\"éééééé\"").is_error());
}

#[test]
fn input_limit_allows_exact_multiline_boundary_and_history_recall() {
    let mut s = bounded(SessionLimits {
        pending_input_bytes: Some(5),
        ..SessionLimits::default()
    });
    assert!(matches!(s.feed_line("[1,"), Response::NeedsMoreInput));
    assert_eq!(evaluated(s.feed_line("]")).output, "[1]");
    assert_eq!(s.history_back().as_deref(), Some("]"));
    assert_eq!(s.pending_bytes, 3);
    assert_eq!(evaluated(s.feed_line("]")).output, "[1]");
    assert!(s.pending().is_empty());
    assert!(matches!(s.feed_line("[1,"), Response::NeedsMoreInput));
    assert!(s.finish().unwrap().is_error());
    assert_eq!(s.pending_bytes, 0);
}

#[test]
fn cumulative_compile_limit_latches_until_reset_and_preserves_history() {
    // Determine the exact allowance for one input and its incompleteness probe.
    let mut baseline = session();
    assert!(!evaluated(baseline.feed_line("1")).is_error());
    let allowance = baseline.compilation_steps();
    assert!(allowance > 0);
    let mut s = bounded(SessionLimits {
        compile_steps: Some(allowance),
        ..SessionLimits::default()
    });
    assert!(!evaluated(s.feed_line("1")).is_error());
    assert_eq!(s.compilation_steps(), allowance);
    let failure = evaluated(s.feed_line("2"));
    assert_eq!(failure.result.unwrap_err().kind, ErrorKind::Steps);
    let spent = s.compilation_steps();
    s.clear_transcript();
    assert_eq!(s.evaluate("3").result.unwrap_err().kind, ErrorKind::Steps);
    assert_eq!(s.compilation_steps(), spent);
    assert_eq!(s.history(), ["1"]);
    s.command(":reset");
    assert_eq!(s.compilation_steps(), 0);
    assert!(s.variables().is_empty());
    assert_eq!(s.history(), ["1"]);
    assert!(!evaluated(s.feed_line("1")).is_error());
    s.reset();
    assert_eq!(s.compilation_steps(), 0);
    assert!(!s.evaluate("1").is_error());
}

#[test]
fn incomplete_and_failed_inputs_spend_compile_allowance() {
    let mut probe = session();
    probe.feed_line("[");
    let spent = probe.compilation_steps();
    assert!(spent > 0);
    let mut s = bounded(SessionLimits {
        compile_steps: Some(spent),
        ..SessionLimits::default()
    });
    assert!(matches!(s.feed_line("["), Response::NeedsMoreInput));
    assert_eq!(
        evaluated(s.feed_line("1,")).result.unwrap_err().kind,
        ErrorKind::Steps
    );
    assert!(s.pending().is_empty());
    let mut s = session();
    assert!(s.evaluate("alias missing unknown").is_error());
    let failed = s.compilation_steps();
    assert!(failed > 0);
    assert!(s.evaluate("alias missing unknown").is_error());
    assert!(s.compilation_steps() > failed);
}

#[test]
fn compilation_memory_and_deadline_failures_are_reported_as_compile_errors() {
    for limits in [
        SessionLimits {
            compile_memory_bytes: Some(0),
            ..SessionLimits::default()
        },
        SessionLimits {
            compile_deadline: Some(std::time::Instant::now()),
            ..SessionLimits::default()
        },
    ] {
        let mut s = bounded(limits.clone());
        let expected = if limits.compile_memory_bytes.is_some() {
            ErrorKind::Memory
        } else {
            ErrorKind::Deadline
        };
        for _ in 0..2 {
            let evaluation = evaluated(s.feed_line("x = 1"));
            assert_eq!(evaluation.result.unwrap_err().kind, expected);
            assert!(evaluation.output.starts_with("compile error:"));
            assert!(s.variables().is_empty());
            s.reset();
        }
    }
}

#[test]
fn cancelled_compile_recovers_only_the_per_input_interrupter() {
    let mut s = session();
    s.interrupter().cancel();
    let evaluation = evaluated(s.feed_line("["));
    assert_eq!(evaluation.result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(evaluation.output.starts_with("compile error:"));
    assert!(s.pending().is_empty());
    assert!(!s.is_cancelled());
    assert!(!evaluated(s.feed_line("42")).is_error());
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut s = ReplSession::new(ReplOptions {
        cancellation,
        ..ReplOptions::default()
    });
    for _ in 0..2 {
        assert_eq!(
            evaluated(s.feed_line("1")).result.unwrap_err().kind,
            ErrorKind::Cancelled
        );
        s.reset();
    }
}

#[cfg(not(target_os = "wasi"))]
#[test]
fn cancellation_during_compilation_stops_before_execution() {
    let s = session();
    let token = s.interrupter();
    let input = "x = 1\n".repeat(500_000);
    let (ready, started) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut s = s;
        ready.send(()).unwrap();
        let evaluation = s.evaluate(&input);
        (s, evaluation)
    });
    started.recv().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));
    token.cancel();
    let (mut s, evaluation) = worker.join().unwrap();
    assert_eq!(evaluation.result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(
        evaluation.output.starts_with("compile error:"),
        "{}",
        evaluation.output
    );
    assert!(s.compilation_steps() > 0);
    assert!(s.variables().is_empty());
    assert!(!s.evaluate("42").is_error());
}

#[test]
fn bulk_feed_handles_line_endings_without_an_aggregate_copy() {
    for (text, count) in [
        ("", 1),
        ("\r\n", 1),
        ("\r\r\n", 2),
        ("1\r\n2\r3\n", 3),
        ("1\n\n", 2),
    ] {
        let mut s = session();
        assert_eq!(s.feed(text).len(), count, "{text:?}");
    }
    let mut s = bounded(SessionLimits {
        pending_input_bytes: Some(4),
        ..SessionLimits::default()
    });
    let responses = s.feed("[\r\n\r\n\r\n\r\n]\n");
    assert_eq!(responses.len(), 5);
    assert!(matches!(responses[3], Response::NeedsMoreInput));
    assert!(evaluated(responses[4].clone()).is_error());
    assert!(s.pending().is_empty());
}

#[test]
fn unlimited_default_preserves_local_history_across_reset() {
    let mut s = session();
    for n in 0..120 {
        assert!(!evaluated(s.feed_line(&n.to_string())).is_error());
    }
    s.reset();
    assert_eq!(s.history().len(), 120);
    assert_eq!(s.transcript().len(), 120);
    assert_eq!(s.history().first().map(String::as_str), Some("0"));
    assert_eq!(s.history_back().as_deref(), Some("119"));
}

#[test]
fn annotation_pass_spends_the_same_compile_budget_before_execution() {
    let mut baseline = session();
    assert!(!baseline.evaluate("x = 1").is_error());
    let total = baseline.compilation_steps();
    let mut s = bounded(SessionLimits {
        compile_steps: Some(total - 1),
        ..SessionLimits::default()
    });
    let evaluation = s.evaluate("x = 1");
    assert_eq!(evaluation.result.unwrap_err().kind, ErrorKind::Steps);
    assert!(evaluation.output.starts_with("compile error:"));
    assert!(s.variables().is_empty());
    assert!(s.compilation_steps() >= total);
}

#[test]
fn direct_input_overflow_discards_an_unfinished_input() {
    let mut s = bounded(SessionLimits {
        pending_input_bytes: Some(5),
        ..SessionLimits::default()
    });
    assert!(matches!(s.feed_line("[1,"), Response::NeedsMoreInput));
    assert_eq!(
        s.evaluate("123456").result.unwrap_err().kind,
        ErrorKind::Memory
    );
    assert!(s.pending().is_empty());
    assert_eq!(s.pending_bytes, 0);
    assert!(evaluated(s.feed_line("]")).is_error());
    assert!(!evaluated(s.feed_line("[2]")).is_error());
}

#[test]
fn carried_namespaces_spend_the_remaining_compile_allowance() {
    let source = format!(
        "# {}\nclass A; end\nclass B; end\nmodule C; end",
        "padding ".repeat(20_000)
    );
    let mut baseline = session();
    assert!(!baseline.evaluate(&source).is_error());
    let before = baseline.compilation_steps();
    assert!(!baseline.evaluate("1").is_error());
    let continued = baseline.compilation_steps() - before;
    // The original source is reparsed to recover namespace dependencies.
    assert!(continued > source.len() as u64 / 10);
    let allowance = before + continued - 1;
    let mut s = bounded(SessionLimits {
        compile_steps: Some(allowance),
        ..SessionLimits::default()
    });
    assert!(!s.evaluate(&source).is_error());
    let failure = s.evaluate("1");
    assert_eq!(failure.result.unwrap_err().kind, ErrorKind::Steps);
    assert!(failure.output.starts_with("compile error:"));
    assert_eq!(s.declarations().count(), 3);
    s.reset();
    assert!(!s.evaluate("1").is_error());
}
