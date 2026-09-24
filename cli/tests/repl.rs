// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

use std::{
    io::Write,
    process::{Command, Stdio},
};

const VIBES: &str = env!("CARGO_BIN_EXE_vibes");

struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `vibes repl` with `input` piped to stdin, the scripted line mode.
fn repl(args: &[&str], input: &str) -> Run {
    let mut child = Command::new(VIBES)
        .arg("repl")
        .args(args)
        .env_remove("NO_COLOR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A REPL that rejects its arguments exits without reading its input.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    let output = child.wait_with_output().unwrap();
    Run {
        status: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn piped_input_runs_one_line_at_a_time_without_escapes() {
    let run = repl(
        &[],
        "x = 10\ndef scale(n)\n  n * x\nend\nscale(4)\nputs \"hi\"; nil\nmissing\n",
    );
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "  › x = 10\n  → 10\n\n\
         \x20 › def scale(n)\n      n * x\n    end\n  → nil\n\n\
         \x20 › scale(4)\n  → 40\n\n\
         \x20 › puts \"hi\"; nil\n  → hi\n\n\
         \x20 › missing\n  ✗ runtime error: undefined variable missing\n  \
         --> line 1, column 1\n 1 | missing\n   | ^\n  at <repl> (1:1)\n\n"
    );
}

#[test]
fn quit_and_ctrl_d_end_the_session_early() {
    for input in [":quit\n1 + 1\n", ":q\n", "\x04 1 + 1\n"] {
        let run = repl(&[], input);
        assert_eq!(run.status, Some(0), "{input:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{input:?}");
    }
}

#[test]
fn quota_flags_select_the_profile() {
    let run = repl(&["-profile", "low"], "i = 0\nwhile true\n  i += 1\nend\n");
    assert_eq!(run.status, Some(0));
    assert!(
        run.stdout
            .contains("✗ runtime error: step quota exceeded (1000000)"),
        "{}",
        run.stdout
    );
    let run = repl(&["--step-quota=50"], "i = 0\nwhile true\n  i += 1\nend\n");
    assert!(
        run.stdout.contains("step quota exceeded (50)"),
        "{}",
        run.stdout
    );
    // xhigh leaves steps and memory unlimited and caps recursion at 10,000.
    let run = repl(&[], "def down(n)\n  down(n + 1)\nend\ndown(0)\n");
    assert!(
        run.stdout
            .contains("recursion depth exceeded (limit 10000)"),
        "{}",
        run.stdout
    );
}

#[test]
fn invalid_arguments_fail_before_reading_input() {
    for (args, message) in [
        (
            &["script.vibe"][..],
            "vibes repl: does not accept positional arguments\n",
        ),
        (
            &["-profile", "bogus"],
            "vibes repl: unknown quota profile \"bogus\" (choose one of: low, medium, high, xhigh)\n",
        ),
        (&["--bogus"], "flag provided but not defined: -bogus\n"),
    ] {
        let run = repl(args, "1 + 1\n");
        assert_eq!(run.status, Some(1), "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
        assert_eq!(run.stderr, message, "{args:?}");
    }
    let run = repl(&["-h"], "");
    assert_eq!(run.status, Some(0));
    assert!(
        run.stdout
            .starts_with("NAME:\n   vibes repl - start the interactive Vibescript REPL\n"),
        "{}",
        run.stdout
    );
}
