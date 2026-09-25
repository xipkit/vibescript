//! `vibes migrate`: diffs, writes and reports over files and directories.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes_in};

const HALF: &str = "def half(n)\n  n / 2 unless n.nil?\nend\n";

#[test]
fn prints_a_diff_typed_from_recorded_calls() {
    let files = Files::new();
    files.write("lib/half.vibe", HALF);
    files.write(
        "inputs.jsonl",
        "{\"file\": \"half.vibe\", \"function\": \"half\", \"args\": [7]}\n",
    );
    let run = vibes_in(
        Some(&files.0),
        &["migrate", "lib", "--inputs", "inputs.jsonl"],
    );
    run.expect(
        0,
        "--- a/half.vibe\n+++ b/half.vibe\n@@ -1,3 +1,3 @@\n-def half(n)\n-  n / 2 unless n.nil?\n+def half(n: int) -> int\n+  n // 2 if n != nil\n end\n",
        "",
    );
    assert_eq!(files.read("lib/half.vibe"), HALF);
}

#[test]
fn writes_files_and_a_second_pass_changes_nothing() {
    let files = Files::new();
    files.write("half.vibe", HALF);
    files.write("inputs.jsonl", "{\"function\": \"half\", \"args\": [7]}\n");
    let run = vibes_in(
        Some(&files.0),
        &[
            "migrate",
            "--write",
            "--inputs",
            "inputs.jsonl",
            "half.vibe",
        ],
    );
    run.expect(0, "", "");
    let migrated = "def half(n: int) -> int\n  n // 2 if n != nil\nend\n";
    assert_eq!(files.read("half.vibe"), migrated);
    let again = vibes_in(
        Some(&files.0),
        &["migrate", "--inputs", "inputs.jsonl", "half.vibe"],
    );
    again.expect(0, "", "");
}

#[test]
fn reports_what_needs_a_person() {
    let files = Files::new();
    files.write("pick.vibe", "def pick(x, name)\n  x.send(name)\nend\n");
    let run = vibes_in(
        Some(&files.0),
        &["migrate", "--report", "json", "pick.vibe"],
    );
    assert_eq!(run.status, Some(0), "{run:?}");
    assert!(run.stdout.starts_with("[\n  {\n"), "{}", run.stdout);
    for code in [
        "\"code\": \"any\"",
        "\"code\": \"dispatch\"",
        "\"changed\": true",
    ] {
        assert!(run.stdout.contains(code), "{}", run.stdout);
    }
    let text = vibes_in(Some(&files.0), &["migrate", "pick.vibe"]);
    assert!(
        text.stderr
            .contains("pick.vibe:2:5: dispatch: send with a name known only at runtime"),
        "{}",
        text.stderr
    );
}

#[test]
fn compatible_mode_keeps_new_syntax_out() {
    let files = Files::new();
    files.write("half.vibe", HALF);
    files.write("inputs.jsonl", "{\"function\": \"half\", \"args\": [7]}\n");
    let run = vibes_in(
        Some(&files.0),
        &[
            "migrate",
            "--compatible",
            "--inputs",
            "inputs.jsonl",
            "half.vibe",
        ],
    );
    assert!(
        run.stdout.contains("+  n / 2 if n != nil\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stderr.contains("half.vibe:2:5: syntax:"),
        "{}",
        run.stderr
    );
}

#[test]
fn refuses_bad_arguments() {
    let files = Files::new();
    vibes_in(Some(&files.0), &["migrate"]).expect(
        1,
        "",
        "vibes migrate: file or directory required\n",
    );
    files.write("a.vibe", "1\n");
    let run = vibes_in(Some(&files.0), &["migrate", "--report", "xml", "a.vibe"]);
    run.expect(
        1,
        "",
        "vibes migrate: unknown report format \"xml\"; use text or json\n",
    );
}
