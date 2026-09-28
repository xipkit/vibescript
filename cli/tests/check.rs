//! `vibes check`, ported from the Go reference's check_command_test.go.
//!
//! The diagnostics come from this library's static type checker (ADR-007),
//! whose messages differ from the reference's; these tests assert the
//! command's contract and report format.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes, vibes_in};

#[test]
fn reports_issues_one_per_line_and_fails() {
    let files = Files::new();
    for (source, issues) in [
        ("def run()\n  value = 1\n  value\nend", 0),
        (
            "def takes_int(value: int)\n  value\nend\n\nvalue = \"1\"\ntakes_int(value)",
            1,
        ),
        ("def helper -> int\n  \"text\"\nend", 1),
        (
            "def create_user(name: string)\n  name\nend\n\nbody = JSON.parse_as(\"{\\\"name\\\": \\\"Ada\\\"}\", { name: string })\ncreate_user(body[\"name\"])",
            0,
        ),
    ] {
        let path = files.write("script.vibe", source);
        let run = vibes(&["check", &path]);
        if issues == 0 {
            run.expect(0, "No issues found\n", "");
            continue;
        }
        assert_eq!(run.status, Some(1), "{source}: {}", run.stderr);
        let headers: Vec<&str> = run
            .stdout
            .lines()
            .filter(|line| line.starts_with(&format!("{path}:")))
            .collect();
        assert_eq!(headers.len(), issues, "{source}: {}", run.stdout);
        for line in headers {
            assert!(line.contains(": error[V"), "{line}");
        }
        assert_eq!(run.stderr, format!("check failed with {issues} error(s)\n"));
    }
    let path = files.write("helper.vibe", "def helper -> int\n  \"text\"\nend");
    vibes(&["check", &path]).expect(
        1,
        &format!(
            "{path}:2:3: error[V0101]: `helper` returns int, found string\n   |\n  2|   \"text\"\n   |   ^^^^^^\n   = expected int, found string\n"
        ),
        "check failed with 1 error(s)\n",
    );
}

#[test]
fn analysis_has_no_default_quota() {
    let source: String = (0..80)
        .map(|i| {
            format!(
                "def helper{i}(items: array<{{ name: string }}>) -> string\n  count = 0\n  names: array<string> = []\n  items.each {{ |item|\n    count = count + 1\n    names = names + [item[\"name\"]]\n  }}\n  \"#{{count}}: \" + names.join(\", \")\nend\n\n"
            )
        })
        .collect();
    let files = Files::new();
    let path = files.write("large.vibe", &source);
    vibes(&["check", &path]).expect(0, "No issues found\n", "");
    // The type check runs no script code, so it takes no quota flags.
    vibes(&["check", "--steps", "1000000", &path]).expect(
        1,
        "",
        "flag provided but not defined: -steps\n",
    );
}

#[test]
fn requires_exactly_one_script_path() {
    vibes(&["check"]).fails("vibes check: script path required");
    vibes(&["check", "a.vibe", "b.vibe"]).fails("vibes check: expected a single script path");
}

#[test]
fn resolves_and_attributes_required_modules() {
    let files = Files::new();
    let modules = files.path("modules");
    files.write("modules/helpers.vibe", "def bad -> int\n  \"text\"\nend\n");
    let script = files.write("script/main.vibe", "require \"helpers\"\n\nbad");
    // A diagnostic in a required file names it by its root-relative path.
    vibes(&["check", "-module-path", &modules, &script]).expect(
        1,
        "helpers.vibe:2:3: error[V0101]: `bad` returns int, found string\n   |\n  2|   \"text\"\n   |   ^^^^^^\n   = expected int, found string\n",
        "check failed with 1 error(s)\n",
    );
    let only = files.write("script/only.vibe", "require \"helpers\"");
    vibes(&["check", "--module-path", &modules, &only]).expect(
        1,
        "helpers.vibe:2:3: error[V0101]: `bad` returns int, found string\n   |\n  2|   \"text\"\n   |   ^^^^^^\n   = expected int, found string\n",
        "check failed with 1 error(s)\n",
    );
    files.write("modules/status.vibe", "enum Status\n  Draft\nend\n");
    let typed = files.write(
        "script/typed.vibe",
        "require \"status\"\n\ndef advance(status: Status) -> Status\n  status\nend",
    );
    vibes(&["check", "-module-path", &modules, &typed]).expect(0, "No issues found\n", "");
    let missing = files.path("missing");
    vibes(&["check", "-module-path", &missing, &typed]).fails(&format!(
        "compute module paths: access module path \"{missing}\": stat {missing}: no such file or directory"
    ));
}

#[test]
fn reports_every_type_error_with_its_code() {
    let files = Files::new();
    let path = files.write(
        "clean.vibe",
        "def add(a: int, b: int) -> int\n  a + b\nend\n",
    );
    vibes(&["check", &path]).expect(0, "No issues found\n", "");
    let source = "count = 1\ncount = \"one\"\nscores = [1]\nbest = scores[0] + 1\n";
    let path = files.write("broken.vibe", source);
    let run = vibes(&["check", &path]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with(&format!("{path}:2:9: error[V0102]: `count` is int")),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(&format!("{path}:4:8: error[V0107]: ")),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("   = fix: read it with `fetch(0)`, which raises when it is missing\n"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "check failed with 2 error(s)\n");
}

const OLD: &str = "names = %w[ada grace]\nn = names.size\nputs n unless n == 0\n";

#[test]
fn foreign_name_notes_reach_text_and_json_diagnostics() {
    let files = Files::new();
    files.write(
        "foreign.vibe",
        "len([1])\nfmt.Sprintf(\"%d\", 1)\nstrings.ToLower(\"HI\")\n",
    );
    let text = vibes_in(Some(&files.0), &["check", "foreign.vibe"]);
    assert_eq!(text.status, Some(1), "{text:?}");
    for advice in ["x.length", "format(pattern, ...)", "text.downcase"] {
        assert!(text.stdout.contains(advice), "{text:?}");
    }
    let json = vibes_in(Some(&files.0), &["check", "--json", "foreign.vibe"]);
    let diagnostics: Vec<serde_json::Value> = json
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(diagnostics.len(), 3, "{json:?}");
    for diagnostic in &diagnostics {
        assert_eq!(diagnostic["code"], "V0201");
        assert_eq!(diagnostic["labels"].as_array().unwrap().len(), 1);
    }
    assert_eq!(diagnostics[0]["fixes"][0]["applicability"], "always");
    assert!(diagnostics[1]["fixes"].as_array().unwrap().is_empty());
    assert!(diagnostics[2]["fixes"].as_array().unwrap().is_empty());
}

#[test]
fn check_json_prints_one_object_per_diagnostic() {
    let files = Files::new();
    files.write("names.vibe", OLD);
    let run = vibes_in(Some(&files.0), &["check", "--json", "names.vibe"]);
    assert_eq!(run.status, Some(1), "{run:?}");
    assert_eq!(run.stderr, "check failed with 3 error(s)\n");
    let lines: Vec<serde_json::Value> = run
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let codes: Vec<&str> = lines
        .iter()
        .map(|line| line["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["V0410", "V0401", "V0407"]);
    let size = &lines[1];
    assert_eq!(size["name"], "removed-name");
    assert_eq!(size["severity"], "error");
    assert_eq!(size["message"], "`size` was removed; use `length`");
    assert_eq!(size["span"]["line"], 2);
    assert_eq!(size["span"]["column"], 11);
    assert_eq!(size["fixes"][0]["applicability"], "always");
    assert_eq!(size["fixes"][0]["edits"][0]["replacement"], "names.length");
}

#[test]
fn check_json_prints_warnings_and_codes_syntax_errors() {
    let files = Files::new();
    files.write(
        "clean.vibe",
        "names = [\"ada\", \"grace\"]\nn = names.length\nputs n if n != 0\n",
    );
    vibes_in(Some(&files.0), &["check", "--json", "clean.vibe"]).expect(0, "", "");
    // A program that compiles prints its warnings and succeeds.
    files.write("warned.vibe", "x = 1\nif x == nil\n  puts 1\nend\n");
    let run = vibes_in(Some(&files.0), &["check", "--json", "warned.vibe"]);
    assert_eq!(run.status, Some(0), "{run:?}");
    let line: serde_json::Value = serde_json::from_str(run.stdout.trim_end()).unwrap();
    assert_eq!(line["severity"], "warning");
    files.write("broken.vibe", "x = (1\n");
    let run = vibes_in(Some(&files.0), &["check", "--json", "broken.vibe"]);
    assert_eq!(run.status, Some(1), "{run:?}");
    let line: serde_json::Value = serde_json::from_str(run.stdout.trim_end()).unwrap();
    assert_eq!(line["code"], "V0001");
    assert_eq!(line["name"], "syntax");
}

#[test]
fn check_renders_removed_spellings_for_people() {
    let files = Files::new();
    files.write("names.vibe", "n = [1].size\n");
    let run = vibes_in(Some(&files.0), &["check", "names.vibe"]);
    assert_eq!(run.status, Some(1), "{run:?}");
    assert!(
        run.stdout.contains(
            "names.vibe:1:9: error[V0401]: `size` was removed; use `length`\n   |\n  1| n = [1].size\n   |         ^^^^\n   = fix: use `length`\n"
        ),
        "{}",
        run.stdout
    );
}
