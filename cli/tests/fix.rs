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
fn fixes_required_regex_captures_and_runs_the_result() {
    let files = Files::new();
    let source = "m = /(?<n>42)/.match(\"42\").as(match_data)\nputs m[\"n\"].to_i\n";
    files.write("capture.vibe", source);
    let run = vibes_in(Some(&files.0), &["fix", "capture.vibe"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    assert!(run.stdout.contains("fixed V0107"), "{run:?}");
    assert_eq!(
        files.read("capture.vibe"),
        source.replace("m[\"n\"]", "m.fetch(\"n\")")
    );
    vibes_in(Some(&files.0), &["run", "capture.vibe"]).expect(0, "42\n", "");
}

#[test]
fn fixes_foreign_collection_length_and_runs_the_result() {
    let files = Files::new();
    files.write("length.vibe", "puts len([1, 2])\n");
    let run = vibes_in(Some(&files.0), &["fix", "length.vibe"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    assert!(run.stdout.contains("fixed V0201"), "{run:?}");
    vibes_in(Some(&files.0), &["run", "length.vibe"]).expect(0, "2\n", "");
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
        run.stdout.starts_with("broken.vibe:1:5: error[V0001]:"),
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

#[test]
fn explicit_files_keep_their_spelling_extension_and_operand_order() {
    let files = Files::new();
    files.write("z.txt", OLD);
    files.write("src/b/child.vibe", OLD);
    files.write("src/b.vibe", OLD);
    files.write("src/ignored.txt", OLD);
    files.write("src/.vibe", OLD);
    let run = vibes_in(
        Some(&files.0),
        &["fix", "./z.txt", "src", "./z.txt", "--dry-run"],
    );
    assert_eq!(run.status, Some(0), "{run:?}");
    let labels: Vec<_> = run
        .stdout
        .lines()
        .filter(|line| line.starts_with("--- a/"))
        .collect();
    assert_eq!(
        labels,
        [
            "--- a/./z.txt",
            "--- a/b/child.vibe",
            "--- a/b.vibe",
            "--- a/./z.txt"
        ]
    );
    assert_eq!(
        run.stderr,
        "vibes fix: would fix 12 issue(s) in 4 file(s)\n"
    );
    assert_eq!(files.read("z.txt"), OLD);
    assert_eq!(files.read("src/ignored.txt"), OLD);
    assert_eq!(files.read("src/.vibe"), OLD);
    let run = vibes_in(Some(&files.0), &["fix", "./z.txt", "./z.txt"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    assert_eq!(run.stderr, "vibes fix: fixed 3 issue(s) in 1 file(s)\n");
    assert_eq!(files.read("z.txt"), NEW);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    };

    #[test]
    fn recursive_links_cannot_redirect_reads_or_writes() {
        for dry_run in [false, true] {
            let files = Files::new();
            files.write("root/inside.vibe", OLD);
            files.write("outside/target.vibe", OLD);
            symlink("../outside/target.vibe", files.0.join("root/leaf.vibe")).unwrap();
            symlink(files.0.join("outside"), files.0.join("root/directory")).unwrap();
            let args = if dry_run {
                vec!["fix", "root", "--dry-run"]
            } else {
                vec!["fix", "root"]
            };
            let run = vibes_in(Some(&files.0), &args);
            assert_eq!(run.status, Some(0), "{run:?}");
            let verb = if dry_run { "would fix" } else { "fixed" };
            assert_eq!(
                run.stderr,
                format!("vibes fix: {verb} 3 issue(s) in 1 file(s)\n")
            );
            assert!(!run.stdout.contains("target.vibe"), "{run:?}");
            assert!(!run.stdout.contains("leaf.vibe"), "{run:?}");
            assert_eq!(files.read("outside/target.vibe"), OLD);
            assert_eq!(
                files.read("root/inside.vibe"),
                if dry_run { OLD } else { NEW }
            );

            // Even unreadable source, dangling links and cycles must be skipped.
            fs::write(files.0.join("outside/target.vibe"), [0xff]).unwrap();
            symlink("missing", files.0.join("root/dangling.vibe")).unwrap();
            symlink(".", files.0.join("root/cycle")).unwrap();
            let run = vibes_in(Some(&files.0), &args);
            assert_eq!(run.status, Some(0), "{run:?}");
            assert_eq!(
                fs::read(files.0.join("outside/target.vibe")).unwrap(),
                [0xff]
            );
        }
    }

    #[test]
    fn explicit_links_authorize_their_targets_and_keep_the_links() {
        let files = Files::new();
        files.write("outside/target.txt", OLD);
        symlink("outside/target.txt", files.0.join("selected")).unwrap();
        let run = vibes_in(Some(&files.0), &["fix", "selected"]);
        assert_eq!(run.status, Some(0), "{run:?}");
        assert!(run.stdout.starts_with("selected:1:9: fixed"), "{run:?}");
        assert_eq!(files.read("outside/target.txt"), NEW);
        assert!(
            fs::symlink_metadata(files.0.join("selected"))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        files.write("outside/file.vibe", OLD);
        symlink("outside", files.0.join("selected-dir")).unwrap();
        let run = vibes_in(Some(&files.0), &["fix", "selected-dir"]);
        assert_eq!(run.status, Some(0), "{run:?}");
        assert!(run.stdout.starts_with("file.vibe:1:9: fixed"), "{run:?}");
        assert_eq!(files.read("outside/file.vibe"), NEW);
    }

    #[test]
    fn rewriting_preserves_identity_permissions_and_hard_links() {
        let files = Files::new();
        let path = files.write("file.vibe", OLD);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, files.0.join("alias")).unwrap();
        let before = fs::metadata(&path).unwrap();
        let run = vibes_in(Some(&files.0), &["fix", "file.vibe"]);
        assert_eq!(run.status, Some(0), "{run:?}");
        let after = fs::metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        assert_eq!(after.permissions().mode() & 0o777, 0o600);
        assert_eq!(files.read("alias"), NEW);
    }

    #[test]
    fn explicit_parent_components_are_resolved_after_symlinks() {
        let files = Files::new();
        files.write("selected/file.vibe", OLD);
        files.write("file.vibe", OLD);
        fs::create_dir(files.0.join("selected/nested")).unwrap();
        symlink("selected/nested", files.0.join("alias")).unwrap();
        let run = vibes_in(Some(&files.0), &["fix", "alias/../file.vibe"]);
        assert_eq!(run.status, Some(0), "{run:?}");
        assert!(run.stdout.starts_with("alias/../file.vibe:"), "{run:?}");
        assert_eq!(files.read("selected/file.vibe"), NEW);
        assert_eq!(files.read("file.vibe"), OLD);
    }
}

#[test]
fn invalid_utf8_is_rejected_without_rewriting() {
    let files = Files::new();
    let path = files.0.join("invalid.vibe");
    std::fs::write(&path, [0xff]).unwrap();
    vibes_in(Some(&files.0), &["fix", "invalid.vibe"])
        .fails("read invalid.vibe: stream did not contain valid UTF-8");
    assert_eq!(std::fs::read(path).unwrap(), [0xff]);
}
