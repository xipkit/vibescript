//! `vibes fix` over the static language's diagnostics.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes_in};

const OLD: &str = "names = %w[ada grace]\nn = names.size\nputs n unless n == 0\n";
const NEW: &str = "names = [\"ada\", \"grace\"]\nn = names.length\nputs n if n != 0\n";

#[test]
fn fixes_files_in_place_and_reports_each_fix() {
    let files = Files::new();
    files.write("src/names.vibe", OLD);
    files.write("src/clean.vibe", NEW);
    let run = vibes_in(Some(&files.0), &["fix", "src"]);
    run.expect(
        0,
        "names.vibe:1:9: fixed V0410: `%w[ada grace]` was removed; use the array literal `[\"ada\", \"grace\"]`\n\
         names.vibe:2:11: fixed V0401: `size` was removed; use `length`\n\
         names.vibe:3:8: fixed V0407: `unless` was removed; write `if` with the negated condition\n",
        "vibes fix: fixed 3 issue(s) in 1 file(s)\n",
    );
    assert_eq!(files.read("src/names.vibe"), NEW);
    assert_eq!(files.read("src/clean.vibe"), NEW);
    let again = vibes_in(Some(&files.0), &["fix", "src"]);
    again.expect(0, "", "vibes fix: fixed 0 issue(s) in 0 file(s)\n");
    assert_eq!(files.read("src/names.vibe"), NEW);
}

#[test]
fn fixes_rewrite_whole_expressions() {
    let files = Files::new();
    let source = "def pick(values: array<int>, i: int, n: int) -> int\n  values[(i + n) % values.length] + 1\nend\n";
    files.write("pick.vibe", source);
    let run = vibes_in(Some(&files.0), &["fix", "pick.vibe"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    assert_eq!(
        files.read("pick.vibe"),
        "def pick(values: array<int>, i: int, n: int) -> int\n  values.fetch((i + n) % values.length) + 1\nend\n"
    );
}

#[test]
fn a_dry_run_prints_a_diff_and_writes_nothing() {
    let files = Files::new();
    files.write("names.vibe", OLD);
    let run = vibes_in(Some(&files.0), &["fix", "names.vibe", "--dry-run"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    assert!(
        run.stdout.starts_with(
            "--- a/names.vibe\n+++ b/names.vibe\n@@ -1,3 +1,3 @@\n-names = %w[ada grace]\n"
        ),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("+puts n if n != 0\n"), "{}", run.stdout);
    assert!(
        run.stdout.contains("names.vibe:2:11: fixed V0401:"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "vibes fix: would fix 3 issue(s) in 1 file(s)\n");
    assert_eq!(files.read("names.vibe"), OLD);
}

#[test]
fn reports_what_remains_with_codes_and_fails() {
    let files = Files::new();
    let source = "items = [1]\nname = \"first\"\nn = items.send(name)\nok = items.eql?([1])\nm = items.size\n";
    files.write("pick.vibe", source);
    let run = vibes_in(Some(&files.0), &["fix", "pick.vibe"]);
    run.expect(
        1,
        "pick.vibe:5:11: fixed V0401: `size` was removed; use `length`\n\
         pick.vibe:3:11: error[V0405]: `send` was removed; call the member directly, or use `case` over the name\n\
         pick.vibe:4:12: error[V0403]: `eql?` was removed; use `==`, which compares values; check that no comparison of types or identity was meant\n",
        "vibes fix: fixed 1 issue(s) in 1 file(s)\nvibes fix: 2 error(s) remain\n",
    );
    // The suggestion for `eql?` is never applied.
    assert!(files.read("pick.vibe").contains("items.eql?([1])"));
}

#[test]
fn a_file_that_does_not_parse_is_reported() {
    let files = Files::new();
    files.write("broken.vibe", "def (\n");
    let run = vibes_in(Some(&files.0), &["fix", "broken.vibe"]);
    assert_eq!(run.status, Some(1), "{run:?}");
    assert!(
        run.stdout.starts_with("broken.vibe: does not parse:"),
        "{}",
        run.stdout
    );
    assert!(
        run.stderr.ends_with("vibes fix: 1 error(s) remain\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn fix_requires_a_path() {
    vibes_in(None, &["fix"]).fails("vibes fix: file or directory required");
}

#[test]
fn a_hash_argument_without_parentheses_gets_them() {
    let files = Files::new();
    files.write("log.vibe", "puts { id: 1 }\np 1, { id: 2 }\n");
    let run = vibes_in(Some(&files.0), &["fix", "log.vibe"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    assert!(
        run.stdout.contains("log.vibe:1:6: fixed V0002:"),
        "{}",
        run.stdout
    );
    assert_eq!(files.read("log.vibe"), "puts({ id: 1 })\np(1, { id: 2 })\n");
}
