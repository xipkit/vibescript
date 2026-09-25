//! `vibes check`, ported from the Go reference's check_command_test.go.
//!
//! The issues come from this library's checker, whose messages differ from the
//! reference's; these tests assert the command's contract and report format.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes};

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
            "def create_user(name: string)\n  name\nend\n\nbody = JSON.parse(\"{}\")\ncreate_user(body[\"name\"])",
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
        assert_eq!(
            run.stdout.lines().count(),
            issues,
            "{source}: {}",
            run.stdout
        );
        for line in run.stdout.lines() {
            assert!(line.starts_with(&format!("{path}:")), "{line}");
            assert!(line.ends_with(')'), "{line}");
        }
        assert_eq!(run.stderr, format!("check failed with {issues} issue(s)\n"));
    }
    let path = files.write("helper.vibe", "def helper -> int\n  \"text\"\nend");
    vibes(&["check", &path]).expect(
        1,
        &format!("{path}:2:3: Return value: expected int, got string (helper)\n"),
        "check failed with 1 issue(s)\n",
    );
}

#[test]
fn analysis_has_no_default_quota() {
    let source: String = (0..80)
        .map(|i| {
            format!(
                "def helper{i}(items)\n  totals = {{ count: 0, names: [] }}\n  items.each do |item|\n    totals[:count] = totals[:count] + 1\n    totals[:names] = totals[:names] + [item[:name]]\n  end\n  \"#{{totals[:count]}}: \" + totals[:names].join(\", \")\nend\n\n"
            )
        })
        .collect();
    let files = Files::new();
    let path = files.write("large.vibe", &source);
    vibes(&["check", &path]).expect(0, "No issues found\n", "");
    vibes(&["check", "--steps", "1000000", &path]).expect(1, "", "step quota exceeded (1000000)\n");
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
    let helper = files.path("modules/helpers.vibe");
    vibes(&["check", "-module-path", &modules, &script]).expect(
        1,
        &format!("{helper}:2:3: Return value: expected int, got string (bad)\n"),
        "check failed with 1 issue(s)\n",
    );
    let only = files.write("script/only.vibe", "require \"helpers\"");
    vibes(&["check", "--module-path", &modules, &only]).expect(
        1,
        &format!("{helper}:2:3: Return value: expected int, got string (bad)\n"),
        "check failed with 1 issue(s)\n",
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
fn static_mode_reports_every_type_error_with_its_code() {
    let files = Files::new();
    let path = files.write(
        "clean.vibe",
        "def add(a: int, b: int) -> int\n  a + b\nend\n",
    );
    vibes(&["check", "--static", &path]).expect(0, "No issues found\n", "");
    let source = "count = 1\ncount = \"one\"\nhalf = 7 / 2\n";
    let path = files.write("broken.vibe", source);
    let run = vibes(&["check", "--static", &path]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with(&format!("{path}:2:9: error[V0102]: `count` is int")),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(&format!("{path}:3:10: error[V0109]: ")),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("   = fix: use floor division `//`\n"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "check failed with 2 error(s)\n");
}
