//! `vibes run`, ported from the Go reference's main_test.go and quota_test.go.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes, vibes_in};

#[test]
fn runs_files_with_the_reference_defaults() {
    let files = Files::new();
    for (name, source, args, stdout) in [
        (
            "greet.vibe",
            "def greet(name)\n  name\nend",
            vec!["-function", "greet"],
            "hello\n",
        ),
        ("run.vibe", "def run\n  \"ok\"\nend", vec![], "ok\n"),
        (
            "class.vibe",
            "class Settings\n  @@limit = 10\n\n  def self.limit\n    @@limit\n  end\nend\n\ndef run\n  Settings.limit\nend",
            vec![],
            "10\n",
        ),
        (
            "double.vibe",
            "def double(x)\n  x * 2\nend\n\ndouble(3)",
            vec![],
            "6\n",
        ),
        (
            "explicit.vibe",
            "def greet(name)\n  name\nend\n\ngreet(\"top\")",
            vec!["-function", "greet"],
            "hello\n",
        ),
        (
            "deferred.vibe",
            "class Settings\n  @@limit = 10\n\n  def self.limit\n    @@limit\n  end\nend\n\ndef run\n  Settings.limit\nend\n\n99",
            vec!["-function", "run"],
            "10\n",
        ),
    ] {
        let path = files.write(name, source);
        let mut command = vec!["run"];
        command.extend(&args);
        command.push(&path);
        if args.contains(&"greet") {
            command.push("hello");
        }
        vibes(&command).expect(0, stdout, "");
    }
    vibes(&["run"]).fails("vibes run: script path required");
    vibes(&["run", "-watch"]).fails("vibes run: script path required");
}

#[test]
fn check_validates_the_selected_invocation_without_executing() {
    let files = Files::new();
    let ok = files.write("ok.vibe", "def run\n  \"ok\"\nend");
    vibes(&["run", "-check", &ok]).expect(0, "", "");
    let top = files.write("top.vibe", "def double(x)\n  x * 2\nend\n\ndouble(3)");
    vibes(&["run", "-check", &top]).expect(0, "", "");
    let named = files.write("named.vibe", "def run(name)\n  name\nend");
    vibes(&["run", "-check", &named, "Ada"]).expect(0, "", "");
    let typed = files.write("typed.vibe", "def run(count: int)\n  count\nend");
    let run = vibes(&["run", "-check", &typed, "one"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(run.stderr.starts_with("check failed: "), "{}", run.stderr);
    assert!(run.stderr.contains("(run)"), "{}", run.stderr);
    let two = files.write(
        "two.vibe",
        "def run\n  a\n  b\nend\ndef a -> int\n  \"x\"\nend\ndef b -> int\n  \"y\"\nend\n",
    );
    let run = vibes(&["run", "-check", "-function", "a", &two]);
    run.expect(
        1,
        "",
        "check failed: 6:3: Return value: expected int, got string (a)\n",
    );
    let none = files.write("none.vibe", "def other\nend\n");
    vibes(&["run", "-check", &none]).fails("function run not found");
}

#[test]
fn inline_snippets_run_and_print_their_result() {
    for (args, stdout) in [
        (vec!["-e", "1 + 2"], "3\n"),
        (vec!["-e", "x = 2\ny = 3\nx * y"], "6\n"),
        (vec!["-e", "def helper\n  1\nend\nhelper"], "1\n"),
        (
            vec![
                "-e",
                "class Helper\n  def value\n    42\n  end\nend\nHelper.new.value",
            ],
            "42\n",
        ),
        (
            vec!["-e", "enum Status\n  Draft\nend\nStatus::Draft.name"],
            "Draft\n",
        ),
        (
            vec!["-e", "export def helper\n  \"exported\"\nend\nhelper"],
            "exported\n",
        ),
        (
            vec!["-e", "private def helper\n  \"private\"\nend\nhelper"],
            "private\n",
        ),
        (vec!["-check", "-e", "1 + 2"], ""),
        (vec!["-e", "nil"], ""),
        (vec!["-e", "puts \"side\"\nnil"], "side\n"),
    ] {
        let mut command = vec!["run"];
        command.extend(args);
        vibes(&command).expect(0, stdout, "");
    }
    for (args, message) in [
        (
            vec!["-e", "   "],
            "vibes run: -e requires a non-empty snippet",
        ),
        (
            vec!["-watch", "-e", "1"],
            "vibes run: -e cannot be combined with -watch",
        ),
        (
            vec!["-function", "main", "-e", "1"],
            "vibes run: -e cannot be combined with -function",
        ),
        (
            vec!["-e", "1", "extra"],
            "vibes run: -e does not accept positional arguments",
        ),
    ] {
        let mut command = vec!["run"];
        command.extend(args);
        vibes(&command).fails(message);
    }
}

#[test]
fn inline_checks_cover_the_whole_snippet() {
    let run = vibes(&["run", "-check", "-e", "missing_name"]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr.starts_with("check failed: 1:1: "),
        "{}",
        run.stderr
    );
    let run = vibes(&[
        "run",
        "-check",
        "-e",
        "def takes_string(value: string)\n  value\nend\n\ndef bad(value: int)\n  takes_string(value)\nend\n\n1",
    ]);
    assert_eq!(run.status, Some(1));
    assert!(run.stderr.starts_with("check failed: "), "{}", run.stderr);
    assert!(
        run.stderr.contains("expected string, got int"),
        "{}",
        run.stderr
    );
    vibes(&[
        "run",
        "-check",
        "-e",
        "def gradual(value)\n  value.whatever_member\nend\n\n3",
    ])
    .expect(0, "", "");
    let run = vibes(&[
        "run",
        "-check",
        "-e",
        "def a -> int\n  \"x\"\nend\ndef b -> int\n  \"y\"\nend\n",
    ]);
    run.expect(
        1,
        "",
        "check failed with 2 issue(s):\n  \
         2:3: Return value: expected int, got string (a)\n  \
         5:3: Return value: expected int, got string (b)\n",
    );
}

#[test]
fn snippet_errors_name_the_snippet() {
    let run = vibes(&["run", "-e", "x = 1\ny = ("]);
    assert_eq!(run.status, Some(1));
    for want in [
        "compile failed",
        "parse error at 2:",
        "y = (",
        "unexpected end of snippet",
    ] {
        assert!(run.stderr.contains(want), "{want:?}: {}", run.stderr);
    }
    assert!(!run.stderr.contains("__eval__"), "{}", run.stderr);
    let run = vibes(&["run", "-e", "x = 1\n1 / 0"]);
    assert_eq!(run.status, Some(1));
    for want in [
        "execution failed",
        "division by zero",
        "line 2",
        "at <snippet> (2:",
    ] {
        assert!(run.stderr.contains(want), "{want:?}: {}", run.stderr);
    }
    let files = Files::new();
    files.write("helper.vibe", "def boom()\n  1 / 0\nend\n");
    let dir = files.0.to_str().unwrap();
    let run = vibes(&[
        "run",
        "-module-path",
        dir,
        "-e",
        "helper = require(\"helper\")\nhelper.boom()",
    ]);
    assert_eq!(run.status, Some(1));
    for want in [
        "execution failed",
        "division by zero",
        "1 / 0",
        "at boom (2:",
    ] {
        assert!(run.stderr.contains(want), "{want:?}: {}", run.stderr);
    }
    assert!(!run.stderr.contains("helper = require"), "{}", run.stderr);
}

#[test]
fn script_output_and_results_use_the_reference_streams_and_forms() {
    let files = Files::new();
    let warn = files.write("warn.vibe", "warn \"careful\"\n\"done\"\n");
    vibes(&["run", &warn]).expect(0, "done\n", "careful\n");
    let values = files.write(
        "values.vibe",
        "[1, nil, \"a\", 1e20, 2.0, :s, {a: nil, \"c d\": [nil]}, 1..3, 10.seconds, 0.1 + 0.2, 1.0 / 0]",
    );
    vibes(&["run", &values]).expect(
        0,
        "[1, , a, 1e+20, 2, s, {a: , c d: []}, 1..3, 10s, 0.30000000000000004, Infinity]\n",
        "",
    );
    let instance = files.write("instance.vibe", "class Foo\nend\n[Foo.new, 1234567.0]");
    vibes(&["run", &instance]).expect(0, "[<Foo instance>, 1.234567e+06]\n", "");
}

#[test]
fn oversized_results_and_sources_are_refused() {
    let files = Files::new();
    let big = files.write("big.vibe", "(1..200000).map { |i| \"abcdefgh\" }");
    vibes(&["run", &big]).fails(
        "result rendering exceeds 1048576 bytes; reduce the returned value or stream it from the script",
    );
    let oversized = files.write("oversized.vibe", &"#".repeat((1 << 20) + 1));
    let message = format!(
        "read script: source exceeds maximum size ({} > 1048576 bytes)",
        (1 << 20) + 1
    );
    vibes(&["run", &oversized]).fails(&message);
    vibes(&["check", &oversized]).fails(&message);
}

#[test]
fn missing_and_irregular_scripts_report_go_style_errors() {
    let files = Files::new();
    let missing = files.path("missing.vibe");
    vibes(&["run", &missing]).fails(&format!(
        "read script: open {missing}: no such file or directory"
    ));
    vibes(&["run", files.0.to_str().unwrap()]).fails(&format!(
        "read script: {} is not a regular file",
        files.0.display()
    ));
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["run", "missing.vibe"]).fails(&format!(
        "read script: open {missing}: no such file or directory"
    ));
    vibes(&["run", "-module-path", &missing, "-e", "1"]).fails(&format!(
        "compute module paths: access module path \"{missing}\": stat {missing}: no such file or directory"
    ));
    let file = files.write("file", "x");
    vibes(&["run", "-module-path", &file, "-e", "1"]).fails(&format!(
        "compute module paths: module path \"{file}\" is not a directory"
    ));
}

#[test]
fn quota_profiles_reach_execution() {
    let files = Files::new();
    let script = files.write(
        "count.vibe",
        "\ndef count(n)\n  i = 0\n  while i < n\n    i = i + 1\n  end\n  i\nend\n\nputs count(2000000)\n",
    );
    vibes(&["run", &script]).expect(0, "2000000\n", "");
    let run = vibes(&["run", "-profile", "low", &script]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr
            .starts_with("execution failed: step quota exceeded (1000000)"),
        "{}",
        run.stderr
    );
    let run = vibes(&[
        "run",
        "-profile",
        "LOW",
        "-step-quota",
        "0x10",
        "-e",
        "x = 0\nwhile x < 100\n  x += 1\nend\nx",
    ]);
    assert!(
        run.stderr
            .starts_with("execution failed: step quota exceeded (16)"),
        "{}",
        run.stderr
    );
    vibes(&["run", "-profile", "low", "-step-quota", "-1", &script]).expect(0, "2000000\n", "");
    vibes(&["run", "-profile", " xhigh ", "-e", "1"]).expect(0, "1\n", "");
    vibes(&["run", "-profile", "gigantic", "-e", "1"]).fails(
        "vibes run: unknown quota profile \"gigantic\" (choose one of: low, medium, high, xhigh)",
    );
    let recursive = files.write("deep.vibe", "def f(n)\n  f(n + 1)\nend\nf(0)\n");
    let run = vibes(&["run", "-recursion-limit", "5", &recursive]);
    assert!(
        run.stderr
            .starts_with("execution failed: recursion depth exceeded (limit 5)"),
        "{}",
        run.stderr
    );
    let run = vibes(&["run", &recursive]);
    assert!(
        run.stderr
            .starts_with("execution failed: recursion depth exceeded (limit 10000)"),
        "{}",
        run.stderr
    );
}

#[cfg(unix)]
#[test]
fn interrupt_cancels_a_running_script() {
    use std::{io::Read, process::Stdio, time::Duration};
    let mut child = std::process::Command::new(support::VIBES)
        .args(["run", "-e", "puts \"started\"\nwhile true\nend"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut started = [0; 8];
    stdout.read_exact(&mut started).unwrap();
    assert_eq!(&started, b"started\n");
    std::thread::sleep(Duration::from_millis(50));
    // SAFETY: the pid names the child process this test spawned and still owns.
    unsafe {
        libc::kill(child.id() as libc::pid_t, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("execution failed: "), "{stderr}");
}

#[test]
fn static_mode_refuses_type_errors_and_entry_arguments_that_are_not_strings() {
    let files = Files::new();
    let path = files.write(
        "greet.vibe",
        "def run(name: string, times: int) -> string\n  name * times\nend\n",
    );
    // Without the checker the call fails when it starts.
    let run = vibes(&["run", &path, "ada", "2"]);
    assert!(
        run.stderr
            .starts_with("execution failed: argument times expected int, got string"),
        "{}",
        run.stderr
    );
    let run = vibes(&["run", "--static", &path, "ada", "2"]);
    assert_eq!(run.status, Some(1), "{}", run.stdout);
    assert!(
        run.stderr.contains(&format!(
            "{path}:1:23: error[V0101]: the command line passes strings, but `times` of `run` is int"
        )),
        "{}",
        run.stderr
    );
    let path = files.write(
        "shout.vibe",
        "def run(name: string) -> string\n  name.upcase\nend\n",
    );
    vibes(&["run", "--static", &path, "ada"]).expect(0, "ADA\n", "");
    let path = files.write("broken.vibe", "def run -> int\n  \"one\"\nend\n");
    let run = vibes(&["run", "--static", &path]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr
            .starts_with("vibes: compile failed with 1 diagnostic(s)\n")
            || run.stderr.contains("compile failed with 1 diagnostic(s)"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("error[V0101]: `run` returns int, found string"),
        "{}",
        run.stderr
    );
}
