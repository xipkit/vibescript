//! Tests for the terminal-facing REPL: the Go REPL's program and key-handling
//! tests (cmd/vibes/repl_test.go), the screen layout, line mode and flags. The
//! session's own tests live with `vibescript_tools::repl`.

use super::{
    Arguments,
    model::{Command, Key, Model},
    parse,
    quota::{self, QuotaFlags},
    render::{self, Colors},
};
use std::ffi::OsString;
use vibescript::CancellationToken;
use vibescript_tools::repl::{Entry, ReplOptions};

/// A model with the REPL's default production quota, xhigh, as `vibes repl`
/// builds it.
fn model() -> Model {
    Model::new(ReplOptions {
        limits: QuotaFlags::default().resolve().unwrap(),
        ..ReplOptions::default()
    })
}

fn last(model: &Model) -> &Entry {
    model
        .session
        .transcript()
        .last()
        .expect("expected a transcript entry")
}

fn run_lines(model: &mut Model, input: &str) -> std::io::Result<String> {
    let mut output = Vec::new();
    super::lines::run(model, input.as_bytes(), &mut output, Colors::None)?;
    Ok(String::from_utf8(output).unwrap())
}

fn screen(lines: &[render::Line]) -> Vec<String> {
    lines.iter().map(|line| render::plain(line)).collect()
}

#[test]
fn repl_program_uses_session_cancellation() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let mut m = Model::new(ReplOptions {
        cancellation,
        ..ReplOptions::default()
    });
    let error = run_lines(&mut m, "1 + 2\n").unwrap_err();
    assert!(error.to_string().contains("execution cancelled"), "{error}");
}

#[test]
fn repl_program_returns_on_ctrl_d() {
    let mut m = model();
    assert_eq!(run_lines(&mut m, "\x04").unwrap(), "");
}

#[test]
fn update_quit_command_returns_quit() {
    let mut m = model();
    m.editor.set_value(":quit");
    assert_eq!(m.update(Key::Enter), Command::Quit);
    assert!(m.quitting, "quitting flag not set");
    assert_eq!(m.editor.value(), "", "input not cleared after quit command");
}

#[test]
fn update_non_quit_command_does_not_quit() {
    let mut m = model();
    m.editor.set_value(":help");
    assert_eq!(m.update(Key::Enter), Command::Continue);
    assert!(!m.quitting, "quitting should remain false");
    assert!(m.show_help, "help toggle should be enabled");
    assert_eq!(m.editor.value(), "", "input not cleared after command");
}

#[test]
fn run_repl_rejects_unknown_profile() {
    let Ok(Arguments::Run(flags)) = parse(["-profile", "bogus"].map(OsString::from)) else {
        panic!("expected flags");
    };
    let error = flags.resolve().unwrap_err();
    assert!(error.contains("unknown quota profile"), "{error}");
}

#[test]
fn keys_drive_the_session_like_the_go_repl() {
    let mut m = model();
    m.editor.set_value("   ");
    m.update(Key::Enter);
    assert_eq!(
        m.editor.value(),
        "   ",
        "a blank line leaves the input alone"
    );
    m.editor.clear();
    m.update(Key::Text("x = 4".to_owned()));
    m.update(Key::Enter);
    assert_eq!(last(&m).output, "4");
    m.editor.set_value(":v");
    m.update(Key::Enter);
    assert!(m.show_vars);
    m.update(Key::CtrlV);
    assert!(!m.show_vars);
    m.update(Key::CtrlK);
    assert!(m.show_help);
    m.update(Key::CtrlL);
    assert!(m.session.transcript().is_empty());
    m.editor.set_value("JSON.parse_a");
    m.update(Key::Tab);
    assert_eq!(m.editor.value(), "JSON.parse_as");
    m.editor.set_value("m");
    m.update(Key::Tab);
    assert_eq!(m.editor.value(), "m");
    assert!(last(&m).output.starts_with("Completions: "));
    assert_eq!(m.update(Key::CtrlD), Command::Quit);
}

#[test]
fn ctrl_c_discards_an_unfinished_input_before_quitting() {
    let mut m = model();
    m.editor.set_value("if true");
    m.update(Key::Enter);
    m.editor.set_value("  1");
    assert_eq!(m.update(Key::CtrlC), Command::Continue);
    assert!(m.session.pending().is_empty());
    assert_eq!(m.editor.value(), "");
    assert!(!m.quitting);
    assert_eq!(m.update(Key::CtrlC), Command::Quit);
}

#[test]
fn up_and_down_recall_whole_inputs() {
    let mut m = model();
    for line in ["1 + 1", "if true", "  2", "end"] {
        m.submit(line);
    }
    m.update(Key::Up);
    assert_eq!(m.session.pending(), ["if true", "  2"]);
    assert_eq!(m.editor.value(), "end");
    m.update(Key::Up);
    assert_eq!(m.editor.value(), "1 + 1");
    m.update(Key::Down);
    m.update(Key::Down);
    assert_eq!(m.editor.value(), "");
    m.update(Key::Up);
    m.update(Key::Enter);
    assert_eq!(last(&m).output, "2");
}

#[test]
fn pasted_lines_submit_one_at_a_time() {
    let mut m = model();
    m.update(Key::Paste(
        "def triple(n)\r\n  n * 3\nend\ntriple(".to_owned(),
    ));
    assert_eq!(last(&m).input, "def triple(n)\n  n * 3\nend");
    assert_eq!(m.editor.value(), "triple(");
    m.update(Key::Text("2)".to_owned()));
    m.update(Key::Enter);
    assert_eq!(last(&m).output, "6");
}

#[test]
fn the_view_follows_the_go_layout() {
    let mut m = model();
    m.submit("1 + 2");
    m.submit("unknown");
    let (lines, cursor) = m.view(80, 24);
    let text = screen(&lines);
    assert_eq!(
        text[0],
        concat!(" Vibescript REPL  v", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(text[1], "─".repeat(60));
    assert_eq!(text[2], "");
    assert_eq!(&text[3..6], ["  › 1 + 2", "  → 3", ""]);
    assert_eq!(text[6], "  › unknown");
    assert!(text[7].starts_with("  ✗ runtime error: undefined variable unknown"));
    let prompt = text.len() - 3;
    assert_eq!(text[prompt], "vibes> type an expression...");
    assert_eq!(cursor, (prompt, 7));
    assert_eq!(text[prompt + 1], "");
    assert_eq!(
        text[prompt + 2],
        "ctrl+k help  ctrl+v vars  ctrl+l clear  ctrl+c quit"
    );
    // A short terminal keeps the input in view and drops older lines.
    let (lines, cursor) = m.view(40, 8);
    let text = screen(&lines);
    assert_eq!(lines.len(), 8);
    assert_eq!(text[1], "─".repeat(38));
    assert_eq!(text[5], "vibes> type an expression...");
    assert_eq!(cursor, (5, 7));
    // Unfinished input shows its lines with continuation prompts.
    m.submit("def f(x)");
    m.editor.set_value("  x");
    let (lines, cursor) = m.view(80, 24);
    let text = screen(&lines);
    let prompt = text.len() - 3;
    assert_eq!(text[prompt - 1], "vibes> def f(x)");
    assert_eq!(text[prompt], "  ...>   x");
    assert_eq!(cursor, (prompt, 10));
}

#[test]
fn the_view_shows_panels_above_the_input() {
    let mut m = model();
    m.submit("x = 1");
    m.submit(":vars");
    m.submit(":help");
    let (lines, _) = m.view(80, 60);
    let text = screen(&lines);
    let vars = text
        .iter()
        .position(|line| line == "│ Variables │")
        .unwrap();
    assert_eq!(text[vars - 1], "╭───────────╮");
    assert_eq!(
        &text[vars + 1..vars + 4],
        ["│   _ = 1   │", "│   x = 1   │", "╰───────────╯"]
    );
    assert_eq!(text[vars + 4], "╭───────────────────────────────────────╮");
    assert_eq!(text[vars + 5], "│ Help                                  │");
    assert_eq!(text[vars + 18], "╰───────────────────────────────────────╯");
    assert_eq!(text[vars + 19], "vibes> type an expression...");
    m.submit(":reset");
    let (lines, _) = m.view(80, 60);
    assert!(screen(&lines).contains(&"│ No variables defined │".to_owned()));
}

#[test]
fn line_mode_prints_each_entry_and_stops_at_quit() {
    let mut m = model();
    let output = run_lines(
        &mut m,
        "x = 2\r\ndef sq(n)\n  n * n\nend\rsq(x)\n:vars\n:quit\n1 + 1\n",
    )
    .unwrap();
    assert_eq!(
        output,
        "  › x = 2\n  → 2\n\n\
         \x20 › def sq(n)\n      n * n\n    end\n  → nil\n\n\
         \x20 › sq(x)\n  → 4\n\n\
         ╭───────────╮\n│ Variables │\n│   _ = 4   │\n│   x = 2   │\n╰───────────╯\n"
    );
}

#[test]
fn line_mode_reports_an_input_left_unfinished() {
    let mut m = model();
    let output = run_lines(&mut m, "[1,\n2").unwrap();
    assert_eq!(
        output,
        "  › [1,\n    2\n  ✗ compile error: parse error at 2:2: unexpected end of snippet\n  \
         --> line 2, column 2\n 2 | 2\n   |  ^\n\n"
    );
    let mut m = model();
    assert_eq!(
        run_lines(&mut m, "if true\n\x03 3").unwrap(),
        "  › 3\n  → 3\n\n"
    );
}

#[test]
fn quota_flags_parse_like_the_go_cli() {
    let args = |list: &[&str]| parse(list.iter().map(OsString::from));
    assert_eq!(args(&[]), Ok(Arguments::Run(QuotaFlags::default())));
    assert_eq!(
        args(&[
            "--profile=low",
            "-step-quota",
            "-1",
            "--memory-quota",
            "0",
            "-recursion-limit=9"
        ]),
        Ok(Arguments::Run(QuotaFlags {
            profile: "low".to_owned(),
            steps: Some(-1),
            memory: Some(0),
            recursion: Some(9),
        }))
    );
    assert_eq!(args(&["-h", "extra"]), Ok(Arguments::Help));
    assert_eq!(args(&["--help"]), Ok(Arguments::Help));
    for (list, message) in [
        (
            &["script.vibe"][..],
            "vibes repl: does not accept positional arguments",
        ),
        (
            &["--", "x"],
            "vibes repl: does not accept positional arguments",
        ),
        (&["-bogus"], "flag provided but not defined: -bogus"),
        (&["-profile"], "flag needs an argument: -profile"),
        (
            &["-step-quota", "abc"],
            "invalid value \"abc\" for flag -step-quota: parse error",
        ),
    ] {
        assert_eq!(args(list), Err(message.to_owned()), "{list:?}");
    }
    assert_eq!(quota::DEFAULT_PROFILE, "xhigh");
}
