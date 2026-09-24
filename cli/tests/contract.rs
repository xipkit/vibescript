//! The Go reference's command-line contract (cmd/vibes/cli_contract_test.go):
//! root dispatch, help, flag syntax and error texts.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, ROOT_HELP, RUN_HELP, vibes, vibes_in};

#[test]
fn root_dispatch_matches_the_reference() {
    vibes(&["--help"]).expect(0, ROOT_HELP, "");
    vibes(&["-h"]).expect(0, ROOT_HELP, "");
    vibes(&["help"]).expect(0, ROOT_HELP, "");
    vibes(&["h"]).expect(0, ROOT_HELP, "");
    vibes(&["-h", "-x"]).expect(0, ROOT_HELP, "");
    vibes(&[]).expect(1, "", &format!("{ROOT_HELP}command required\n"));
    for (args, command) in [
        (&["unknown"][..], "\"unknown\""),
        (&["unknown", "--help"], "\"unknown\""),
        (&["--", "run"], "\"--\""),
        (&["-help"], "\"-help\""),
        (&["--h"], "\"--h\""),
        (&["unknown", "", "--help"], "\"unknown\""),
        (&["--help=false", "fmt", "-w"], "\"--help=false\""),
        (&["-h="], "\"-h=\""),
        (&[""], "\"\""),
        (&[" run"], "\" run\""),
        (&["run\n"], "\"run\\n\""),
        (&["a\tb"], "\"a\\tb\""),
        (&["RUN"], "\"RUN\""),
        (&["-"], "\"-\""),
        (&["-1"], "\"-1\""),
        (&["-=x"], "\"-=x\""),
    ] {
        vibes(args).expect(1, "", &format!("{ROOT_HELP}unknown command {command}\n"));
    }
    for (args, name) in [
        (&["-x"][..], "-x"),
        (&["--bogus"], "-bogus"),
        (&["---x"], "--x"),
        (&["-x=1"], "-x"),
        (&["--1"], "-1"),
        (&["-é"], "-é"),
        (&["-version"], "-version"),
    ] {
        vibes(args).fails(&format!("flag provided but not defined: {name}"));
    }
}

#[test]
fn run_help_matches_the_reference() {
    for args in [
        &["run", "-h"][..],
        &["run", "--help"],
        &["help", "run"],
        &["h", "run"],
        &["run", "-h", "-x"],
        &["help", "--", "run"],
    ] {
        vibes(args).expect(0, RUN_HELP, "");
    }
}

#[test]
fn every_command_has_help() {
    let help = vibes(&["help", "--help"]);
    help.expect(
        0,
        "NAME:\n   vibes help - Shows a list of commands or help for one command\n\n\
         USAGE:\n   vibes help [options] [command]\n\n\
         OPTIONS:\n   --help, -h  show help\n",
        "",
    );
    vibes(&["help", "help"]).expect(0, &help.stdout, "");
    vibes(&["help", "-h", "run"]).expect(0, &help.stdout, "");
    for name in ["run", "check", "fmt", "analyze", "test", "lsp", "repl"] {
        let run = vibes(&[name, "--help"]);
        assert_eq!(run.status, Some(0), "{name}");
        assert_eq!(run.stderr, "", "{name}");
        assert!(
            run.stdout.contains(&format!("NAME:\n   vibes {name}")),
            "{name}: {}",
            run.stdout
        );
        if name == "lsp" || name == "repl" {
            assert!(
                run.stdout
                    .contains(&format!("USAGE:\n   vibes {name} [options]\n")),
                "{name}: {}",
                run.stdout
            );
            assert!(!run.stdout.contains("[argument"), "{name}");
        }
    }
    for (name, text) in [
        (
            "fmt",
            "NAME:\n   vibes fmt - canonically format Vibescript source files\n\n\
             USAGE:\n   vibes fmt [options] <path>...\n\n\
             OPTIONS:\n   -w          write results to source files instead of stdout\n   \
             --check     fail if any source file needs formatting\n   \
             --help, -h  show help\n",
        ),
        (
            "analyze",
            "NAME:\n   vibes analyze - analyze a script for lint issues\n\n\
             USAGE:\n   vibes analyze [options] <script>\n\n\
             OPTIONS:\n   --help, -h  show help\n",
        ),
        (
            "lsp",
            "NAME:\n   vibes lsp - start the language server over stdio\n\n\
             USAGE:\n   vibes lsp [options]\n\n\
             OPTIONS:\n   --help, -h  show help\n",
        ),
    ] {
        vibes(&["help", name]).expect(0, text, "");
    }
    let test_help = vibes(&["help", "test"]).stdout;
    assert!(test_help.contains(
        "   --run string                                   run only test functions matching this regular expression\n"
    ));
    assert!(test_help.contains("(default: \"xhigh\")"));
    let repl_help = vibes(&["help", "repl"]).stdout;
    assert!(repl_help.contains(
        "   --recursion-limit int  override the profile's recursion limit (-1 = unlimited, which can crash on infinite recursion)\n"
    ));
    vibes(&["help", "nope"]).fails("No help topic for 'nope'");
    vibes(&["help", ""]).fails("No help topic for ''");
}

#[test]
fn subcommand_flag_errors_use_the_reference_wording() {
    for (args, message) in [
        (
            &["run", "-unknown"][..],
            "flag provided but not defined: -unknown",
        ),
        (
            &["fmt", "--unknown=value"],
            "flag provided but not defined: -unknown",
        ),
        (
            &["help", "-unknown"],
            "flag provided but not defined: -unknown",
        ),
        (&["run", "--e"], "flag needs an argument: -e"),
        (&["fmt", "---w"], "bad flag syntax: ---w"),
        (&["run", "--=value"], "bad flag syntax: --=value"),
        (&["fmt", "-=value"], "bad flag syntax: -=value"),
        (&["run", "--help=1"], "help flag does not accept a value"),
        (&["run", "-x", "-h"], "flag provided but not defined: -x"),
        (
            &["run", "-check=yes", "-e", "1"],
            "invalid boolean value \"yes\" for -check: parse error",
        ),
    ] {
        vibes(args).fails(message);
    }
}

#[test]
fn leaf_commands_treat_help_as_an_argument() {
    let files = Files::new();
    files.write("help", "\"script\"\n");
    let dir = Some(files.0.as_path());
    for args in [&["run", "help"][..], &["run", "--", "help"]] {
        vibes_in(dir, args).expect(0, "script\n", "");
    }
    vibes(&["lsp", "help"]).fails("vibes lsp: does not accept positional arguments");
    vibes(&["repl", "h"]).fails("vibes repl: does not accept positional arguments");
}

/// The Go reference rejects `--version`; this CLI prints its version instead.
#[test]
fn version_prints_the_package_version() {
    vibes(&["--version"]).expect(
        0,
        &format!("vibescript.rs {}\n", env!("CARGO_PKG_VERSION")),
        "",
    );
}

#[test]
fn undocumented_extra_arguments_are_rejected() {
    let files = Files::new();
    let script = files.write("script.vibe", "1\n");
    for (args, message) in [
        (
            vec!["analyze", script.as_str(), "extra.vibe"],
            "vibes analyze: expected a single script path",
        ),
        (
            vec!["lsp", "extra"],
            "vibes lsp: does not accept positional arguments",
        ),
        (
            vec!["repl", "extra"],
            "vibes repl: does not accept positional arguments",
        ),
        (
            vec!["help", "run", "extra"],
            "vibes help: expected at most one command",
        ),
        (
            vec!["help", "run", "--"],
            "vibes help: expected at most one command",
        ),
        (
            vec!["check", "-", "extra"],
            "vibes check: expected a single script path",
        ),
    ] {
        vibes(&args).fails(message);
    }
}

#[test]
fn lsp_serves_stdio_and_repl_validates_its_flags_first() {
    // Without input the server stops at once, writing nothing.
    vibes(&["lsp"]).expect(0, "", "");
    vibes(&["lsp", "extra"]).fails("vibes lsp: does not accept positional arguments");
    vibes(&["repl", "-profile", "nope"]).fails(
        "vibes repl: unknown quota profile \"nope\" (choose one of: low, medium, high, xhigh)",
    );
    vibes(&["repl", "-step-quota", "x"])
        .fails("invalid value \"x\" for flag -step-quota: parse error");
}

#[test]
fn run_stops_parsing_flags_at_the_first_positional() {
    let files = Files::new();
    let echo = files.write("echo.vibe", "def run(value)\n  value\nend\n");
    vibes(&["run", &echo, "-check="]).expect(0, "-check=\n", "");
    vibes(&["run", "--", &echo, "-check="]).expect(0, "-check=\n", "");
    let pair = files.write(
        "pair.vibe",
        "def run(first, second)\n  first + \":\" + second\nend\n",
    );
    vibes(&["run", &pair, "--", "-check"]).expect(0, "--:-check\n", "");
    for first in ["", " "] {
        vibes(&["run", &pair, first, "-check"]).expect(0, &format!("{first}:-check\n"), "");
    }
    files.write("-", "def run(value)\n  value\nend\n");
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["run", "-", "argument"]).expect(0, "argument\n", "");
}

#[test]
fn rejected_flags_never_modify_files() {
    const SOURCE: &str = "def run()\n1\nend\n";
    let files = Files::new();
    let dir = Some(files.0.as_path());
    for (name, args, message) in [
        (
            "-1",
            vec!["fmt", "-w", "-1"],
            "flag provided but not defined: -1".to_owned(),
        ),
        (
            "-א.vibe",
            vec!["fmt", "-w", "-א.vibe"],
            "flag provided but not defined: -א.vibe".to_owned(),
        ),
        (
            "a.vibe",
            vec!["fmt", "-w ", "a.vibe"],
            "flag provided but not defined: -w ".to_owned(),
        ),
        (
            "b.vibe",
            vec!["fmt", "-w=true ", "b.vibe"],
            "invalid boolean value \"true \" for -w: parse error".to_owned(),
        ),
        (
            "c.vibe",
            vec!["fmt", "-h=false", "-w", "c.vibe"],
            "help flag does not accept a value".to_owned(),
        ),
        (
            "d.vibe",
            vec!["fmt", "-w=bogus", "-check=", "d.vibe"],
            "invalid boolean value \"bogus\" for -w: parse error".to_owned(),
        ),
        (
            "e.vibe",
            vec!["fmt", "-w=", "e.vibe"],
            "invalid boolean value \"\" for -w: parse error".to_owned(),
        ),
    ] {
        files.write(name, SOURCE);
        vibes_in(dir, &args).fails(&message);
        assert_eq!(files.read(name), SOURCE, "{args:?}");
    }
    files.write("f.vibe", SOURCE);
    let run = vibes_in(dir, &["fmt", " -w", "f.vibe"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(files.read("f.vibe"), SOURCE);
    files.write("g.vibe", SOURCE);
    let run = vibes_in(dir, &["--help=false", "fmt", "-w", "g.vibe"]);
    run.expect(
        1,
        "",
        &format!("{ROOT_HELP}unknown command \"--help=false\"\n"),
    );
    assert_eq!(files.read("g.vibe"), SOURCE);
}

#[test]
fn bare_help_short_circuits_mutating_flags() {
    const SOURCE: &str = "def run()\n1\nend\n";
    for args in [["-h", "-w"], ["-w", "--help"]] {
        let files = Files::new();
        let path = files.write("script.vibe", SOURCE);
        let run = vibes(&["fmt", args[0], args[1], &path]);
        assert_eq!(run.status, Some(0), "{args:?}");
        assert_eq!(run.stderr, "", "{args:?}");
        assert!(run.stdout.contains("NAME:\n   vibes fmt"), "{args:?}");
        assert_eq!(files.read("script.vibe"), SOURCE);
    }
}

#[test]
fn integer_errors_are_reported_before_later_flags() {
    for args in [
        &["run", "-step-quota=nope", "-unknown"][..],
        &["run", "-step-quota", "nope", "-unknown"],
    ] {
        vibes(args).fails("invalid value \"nope\" for flag -step-quota: parse error");
    }
    vibes(&["run", "-step-quota", "99999999999999999999", "-e", "1"])
        .fails("invalid value \"99999999999999999999\" for flag -step-quota: value out of range");
    vibes(&["run", "-step-quota", "-e", "1"])
        .fails("invalid value \"-e\" for flag -step-quota: parse error");
}

#[test]
fn empty_values_are_rejected_with_the_reference_wording() {
    vibes(&["run", "-e="]).fails("vibes run: -e requires a non-empty snippet");
    let files = Files::new();
    let script = files.write("script.vibe", "1\n");
    vibes(&["run", "-check=", &script]).fails("invalid boolean value \"\" for -check: parse error");
}

#[test]
fn run_defaults_to_top_level_statements_then_run() {
    let files = Files::new();
    let both = files.write(
        "both.vibe",
        "def run\n  \"function\"\nend\n\n\"top-level\"\n",
    );
    vibes(&["run", &both]).expect(0, "top-level\n", "");
    vibes(&["run", "-function=run", &both]).expect(0, "function\n", "");
    let only = files.write("only.vibe", "def run\n  \"ok\"\nend\n");
    vibes(&["run", &only]).expect(0, "ok\n", "");
}

#[test]
fn explicit_zero_quota_selects_the_engine_default() {
    let files = Files::new();
    let script = files.write(
        "count.vibe",
        "def count(n)\n  i = 0\n  while i < n\n    i = i + 1\n  end\n  i\nend\n\nputs count(200000)\n",
    );
    vibes(&["run", &script]).expect(0, "200000\n", "");
    let run = vibes(&["run", "-step-quota=0", &script]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.contains("step quota exceeded (1000000)"),
        "{}",
        run.stderr
    );
}

#[test]
fn module_paths_are_repeatable_and_never_split() {
    let files = Files::new();
    files.write("first,modules/first.vibe", "def value\n  \"first\"\nend\n");
    files.write(
        "second-modules/second.vibe",
        "def value\n  \"second\"\nend\n",
    );
    let main = files.write(
        "main/main.vibe",
        "first = require(\"first\")\nsecond = require(\"second\")\nfirst.value + \":\" + second.value\n",
    );
    vibes(&[
        "run",
        "-module-path",
        &files.path("first,modules"),
        "-module-path",
        &files.path("second-modules"),
        &main,
    ])
    .expect(0, "first:second\n", "");
}

/// `TestCLIContractLSPPreservesStdoutFraming`: stdout carries only the
/// server's framed messages, and exit ends the process cleanly.
#[test]
fn lsp_preserves_stdout_framing() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let frame = |payload: &str| format!("Content-Length: {}\r\n\r\n{payload}", payload.len());
    let input = frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        + &frame(r#"{"jsonrpc":"2.0","method":"exit"}"#);
    let mut child = Command::new(support::VIBES)
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (header, payload) = stdout.split_once("\r\n\r\n").unwrap();
    assert_eq!(header, format!("Content-Length: {}", payload.len()));
    assert!(
        payload.starts_with(r#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"#),
        "{payload}"
    );
}
