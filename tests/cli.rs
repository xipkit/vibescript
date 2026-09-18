use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

const VIBES: &str = env!("CARGO_BIN_EXE_vibes");
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A unique temporary directory of script files, removed when the test finishes.
struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let path = base.join(format!(
                "cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create {}: {error}", path.display()),
            }
        }
    }

    fn write(&self, name: &str, source: &str) -> String {
        let path = self.0.join(name);
        fs::write(&path, source).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn missing(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let result = fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn expect(&self, status: i32, stdout: &str, stderr: &str) {
        assert_eq!(self.status, Some(status), "{}", self.stderr);
        assert_eq!(self.stdout, stdout);
        assert_eq!(self.stderr, stderr);
    }
}

fn vibes_in(dir: Option<&Path>, args: &[&str]) -> Run {
    let mut command = Command::new(VIBES);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command.output().unwrap();
    Run {
        status: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn vibes(args: &[&str]) -> Run {
    vibes_in(None, args)
}

fn assert_stats_line(line: &str) {
    let fields: Vec<_> = line.split(' ').collect();
    assert_eq!(fields.len(), 3, "{line:?}");
    for (field, prefix) in fields
        .iter()
        .zip(["steps=", "peak_bytes=", "retained_bytes="])
    {
        let value = field
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("{line:?}"));
        value.parse::<u64>().unwrap_or_else(|_| panic!("{line:?}"));
    }
}

const ADD: &str = "puts \"top\"\ndef run(x:int) -> int\n  puts \"ran\"\n  x + 1\nend\n";
const ADD_FRAME: &str = "  --> line 2, column 1\n 2 | def run(x:int) -> int\n   | ^\n";
const INCOMPLETE: &str = "def run\n  puts \"ran\"\n  require(\"x\")\nend\n";
const INCOMPLETE_FRAME: &str = "  --> line 3, column 3\n 3 |   require(\"x\")\n   |   ^\n";

#[test]
fn runs_top_level_statements_and_prints_the_final_value_as_json() {
    let files = Files::new();
    let file = files.write(
        "top.vibe",
        "puts \"hi\"\nwarn \"careful\"\n{a: [1, 2], b: \"x\"}\n",
    );
    vibes(&[&file]).expect(0, "hi\n{\"a\":[1,2],\"b\":\"x\"}\n", "careful\n");
    let run = vibes(&[&file, "--stats"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "hi\n{\"a\":[1,2],\"b\":\"x\"}\n");
    let (warning, stats) = run.stderr.split_once('\n').unwrap();
    assert_eq!(warning, "careful");
    assert_stats_line(stats.trim_end_matches('\n'));
}

#[test]
fn calls_a_function_with_positional_arguments_in_order() {
    let files = Files::new();
    let file = files.write("pair.vibe", "puts \"top\"\ndef pair(a, b)\n  [a, b]\nend\n");
    vibes(&[&file, "--function", "pair", "--arg", "1", "--arg", "\"x\""]).expect(
        0,
        "[1,\"x\"]\n",
        "",
    );
    vibes(&["--function", "pair", "--arg", "-1", &file, "--arg", "[2]"]).expect(
        0,
        "[-1,[2]]\n",
        "",
    );
    let run = vibes(&[
        &file,
        "--function",
        "pair",
        "--arg",
        "1",
        "--arg",
        "2",
        "--stats",
    ]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "[1,2]\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
}

#[test]
fn binds_keyword_arguments_by_name_with_the_last_duplicate_winning() {
    let files = Files::new();
    let file = files.write("kw.vibe", "def run(a:int,b:2,**rest);[a,b,rest[:x]];end\n");
    let inputs = [
        "--function",
        "run",
        "--kwarg",
        "b=3",
        "--arg",
        "7",
        "--kwarg",
        "b=5",
        "--kwarg",
        "x=9",
    ];
    for (mode, stdout) in [
        (None, "[7,5,9]\n"),
        (Some("--check"), ""),
        (Some("--checked"), "[7,5,9]\n"),
    ] {
        let mut args = vec![file.as_str()];
        args.extend(inputs);
        args.extend(mode);
        vibes(&args).expect(0, stdout, "");
    }
    vibes(&[
        &file,
        "--function",
        "run",
        "--kwarg",
        "a=1",
        "--kwarg",
        "x=null",
    ])
    .expect(0, "[1,2,null]\n", "");
}

#[test]
fn clean_check_prints_nothing_and_runs_no_script_code() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&[&file, "--function", "run", "--check", "--arg", "41"]).expect(0, "", "");
    vibes(&["--check", "--arg", "41", "--function", "run", &file]).expect(0, "", "");
    let run = vibes(&[
        &file,
        "--function",
        "run",
        "--check",
        "--arg",
        "41",
        "--stats",
    ]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
}

#[test]
fn checked_call_executes_only_a_clean_call() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&[&file, "--function", "run", "--checked", "--arg", "41"]).expect(0, "ran\n42\n", "");
    let run = vibes(&[
        &file,
        "--function",
        "run",
        "--checked",
        "--arg",
        "41",
        "--stats",
    ]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "ran\n42\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    let run = vibes(&[&file, "--function", "run", "--checked", "--arg", "\"bad\""]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let (first, rest) = run.stderr.split_once('\n').unwrap();
    let prefix = format!("{file}:2:1: error in run: ");
    assert!(first.starts_with(&prefix), "{first}");
    assert!(first.contains("expected int, got string"), "{first}");
    assert_eq!(
        rest,
        format!("{ADD_FRAME}{file}: check of run found 1 error; nothing was executed\n")
    );
    let run = vibes(&[
        &file,
        "--function",
        "run",
        "--checked",
        "--arg",
        "\"bad\"",
        "--stats",
    ]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let summary = format!("{file}: check of run found 1 error; nothing was executed\n");
    let (report, stats) = run.stderr.rsplit_once(&summary).unwrap();
    assert!(report.starts_with(&prefix), "{report}");
    assert_stats_line(stats.trim_end_matches('\n'));
}

#[test]
fn checked_conversion_errors_precede_all_script_output() {
    let files = Files::new();
    let file = files.write(
        "conversion.vibe",
        "class C\n  def to_s -> int\n    puts 'converted'\n    false\n  end\nend\ndef run\n  puts 'started'\n  \"#{C.new}\"\nend\n",
    );
    for mode in ["--check", "--checked"] {
        let result = vibes(&[&file, "--function", "run", mode]);
        assert_eq!(result.status, Some(1), "{}", result.stderr);
        assert_eq!(result.stdout, "");
        assert!(
            result
                .stderr
                .contains("Return value: expected int, got bool"),
            "{}",
            result.stderr
        );
        assert!(!result.stderr.contains("incomplete"), "{}", result.stderr);
    }
    let result = vibes(&[&file, "--function", "run"]);
    assert_eq!(result.status, Some(1));
    assert_eq!(result.stdout, "started\nconverted\n");
}

#[test]
fn check_reports_known_errors_with_position_and_code_frame() {
    let files = Files::new();
    let file = files.write("ret.vibe", "def run() -> int\n  \"é\"\nend\n");
    let report = format!(
        "{file}:2:3: error in run: Return value: expected int, got string\n  \
         --> line 2, column 3\n 2 |   \"é\"\n   |   ^\n\
         {file}: check of run found 1 error\n"
    );
    vibes(&[&file, "--function", "run", "--check"]).expect(1, "", &report);
    let file = files.write("add.vibe", ADD);
    let run = vibes(&[&file, "--function", "run", "--check", "--arg", "\"bad\""]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let (first, rest) = run.stderr.split_once('\n').unwrap();
    assert!(
        first.starts_with(&format!("{file}:2:1: error in run: ")),
        "{first}"
    );
    assert!(first.contains("expected int, got string"), "{first}");
    assert_eq!(
        rest,
        format!("{ADD_FRAME}{file}: check of run found 1 error\n")
    );
}

#[test]
fn check_reports_incomplete_analysis_distinctly_and_never_executes() {
    let files = Files::new();
    let file = files.write("incomplete.vibe", INCOMPLETE);
    let run = vibes(&[&file, "--function", "run", "--check"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let (first, rest) = run.stderr.split_once('\n').unwrap();
    assert!(
        first.starts_with(&format!("{file}:3:3: incomplete in run: ")),
        "{first}"
    );
    assert!(!first.contains("error"), "{first}");
    assert_eq!(
        rest,
        format!("{INCOMPLETE_FRAME}{file}: check of run found 1 incomplete path\n")
    );
    let run = vibes(&[&file, "--function", "run", "--checked", "--stats"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let (first, rest) = run.stderr.split_once('\n').unwrap();
    assert!(
        first.starts_with(&format!("{file}:3:3: incomplete in run: ")),
        "{first}"
    );
    let summary = format!("{file}: check of run found 1 incomplete path; nothing was executed\n");
    let (frame, stats) = rest.split_once(&summary).unwrap();
    assert_eq!(frame, INCOMPLETE_FRAME);
    assert_stats_line(stats.trim_end_matches('\n'));
}

#[test]
fn source_read_and_parse_errors_carry_the_filename_and_exit_nonzero() {
    let files = Files::new();
    let missing = files.missing("missing.vibe");
    for args in [
        vec![missing.as_str()],
        vec![missing.as_str(), "--function", "run", "--check"],
    ] {
        let run = vibes(&args);
        assert_eq!(run.status, Some(1));
        assert_eq!(run.stdout, "");
        assert!(
            run.stderr.starts_with(&format!("cannot read {missing}: ")),
            "{}",
            run.stderr
        );
    }
    let binary = files.0.join("binary.vibe");
    fs::write(&binary, [0xff, b'\n']).unwrap();
    let binary = binary.to_str().unwrap();
    vibes(&[binary]).expect(
        1,
        "",
        &format!("cannot read {binary}: stream did not contain valid UTF-8\n"),
    );
    let broken = files.write("broken.vibe", "def run(\n");
    let report = format!(
        "{broken}:2:1: parse error: expected name\n  --> line 2, column 1\n 2 | \n   | ^\n"
    );
    vibes(&[&broken]).expect(1, "", &report);
    vibes(&[&broken, "--function", "run", "--checked"]).expect(1, "", &report);
}

#[test]
fn usage_errors_exit_with_status_two_without_reading_the_file() {
    let files = Files::new();
    let file = files.missing("missing.vibe");
    let other = files.missing("other.vibe");
    for (args, message) in [
        (vec![], "expected source file; use --help"),
        (
            vec![file.as_str(), other.as_str()],
            "expected one source file",
        ),
        (vec![file.as_str(), "--bogus"], "unknown option --bogus"),
        (vec![file.as_str(), "-x"], "unknown option -x"),
        (
            vec![file.as_str(), "--function"],
            "--function requires NAME",
        ),
        (vec![file.as_str(), "--arg"], "--arg requires JSON"),
        (vec![file.as_str(), "--kwarg"], "--kwarg requires NAME=JSON"),
        (vec![file.as_str(), "--steps"], "--steps requires N"),
        (
            vec![file.as_str(), "--arg", "1"],
            "--arg requires --function",
        ),
        (
            vec![file.as_str(), "--kwarg", "x=1"],
            "--kwarg requires --function",
        ),
        (
            vec![file.as_str(), "--check"],
            "--check requires --function",
        ),
        (
            vec![file.as_str(), "--checked"],
            "--checked requires --function",
        ),
        (
            vec![file.as_str(), "--function", "run", "--check", "--checked"],
            "--check and --checked are mutually exclusive",
        ),
        (
            vec![file.as_str(), "--function", "run", "--checked", "--check"],
            "--check and --checked are mutually exclusive",
        ),
        (
            vec![file.as_str(), "--function", "run", "--kwarg", "x"],
            "--kwarg requires NAME=JSON, got \"x\"",
        ),
        (
            vec![file.as_str(), "--function", "run", "--kwarg", "=1"],
            "--kwarg requires a nonempty NAME before '=', got \"=1\"",
        ),
        (
            vec![file.as_str(), "--timeout-ms", "-1"],
            "invalid --timeout-ms value \"-1\": ",
        ),
        (
            vec![file.as_str(), "--steps", "abc"],
            "invalid --steps value \"abc\": ",
        ),
        (
            vec![file.as_str(), "--memory", ""],
            "invalid --memory value \"\": ",
        ),
        (
            vec![file.as_str(), "--recursion", "1.5"],
            "invalid --recursion value \"1.5\": ",
        ),
        (
            vec![file.as_str(), "--function", "run", "--arg", "{"],
            "invalid JSON for --arg: ",
        ),
        (
            vec![file.as_str(), "--function", "run", "--kwarg", "x={"],
            "invalid JSON for --kwarg x: ",
        ),
        (
            vec![
                file.as_str(),
                "--function",
                "run",
                "--arg",
                "1",
                "--arg",
                "nope",
            ],
            "invalid JSON for --arg: ",
        ),
    ] {
        let run = vibes(&args);
        assert_eq!(run.status, Some(2), "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(run.stderr.ends_with('\n'), "{args:?}: {}", run.stderr);
        let stderr = run.stderr.trim_end_matches('\n');
        if message.ends_with(": ") {
            assert!(stderr.starts_with(message), "{args:?}: {stderr}");
            assert!(stderr.len() > message.len(), "{args:?}: {stderr}");
        } else {
            assert_eq!(stderr, message, "{args:?}");
        }
    }
}

#[test]
fn help_explains_the_exact_call_scope_and_version_prints() {
    for flag in ["--help", "-h"] {
        let run = vibes(&[flag]);
        assert_eq!(run.status, Some(0));
        assert_eq!(run.stderr, "");
        for needle in [
            "Usage: vibes [OPTIONS] FILE",
            "--kwarg NAME=JSON",
            "--check ",
            "--checked ",
            "Checking covers exactly one call",
            "It does not check unused functions",
            "reported as\nincomplete rather than assumed clean",
            "Exit status: 0 on success or a clean check, 1 when",
        ] {
            assert!(
                run.stdout.contains(needle),
                "{needle:?} missing from:\n{}",
                run.stdout
            );
        }
        assert!(run.stdout.ends_with('\n'));
    }
    vibes(&["--version"]).expect(
        0,
        &format!("vibescript.rs {}\n", env!("CARGO_PKG_VERSION")),
        "",
    );
    vibes(&["--help", "--bogus"]).expect(0, &vibes(&["-h"]).stdout, "");
    vibes(&["--bogus", "--help"]).expect(2, "", "unknown option --bogus\n");
}

#[test]
fn limits_deadlines_and_unknown_functions_fail_with_nonzero_status() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&[
        &file,
        "--function",
        "run",
        "--check",
        "--arg",
        "1",
        "--steps",
        "1",
    ])
    .expect(1, "", "step quota exceeded\n");
    vibes(&[
        &file,
        "--function",
        "run",
        "--checked",
        "--arg",
        "1",
        "--memory",
        "1",
    ])
    .expect(1, "", "memory quota exceeded\n");
    vibes(&[
        &file,
        "--function",
        "run",
        "--checked",
        "--arg",
        "1",
        "--timeout-ms",
        "0",
    ])
    .expect(1, "", "execution deadline exceeded\n");
    vibes(&[
        &file,
        "--function",
        "run",
        "--arg",
        "1",
        "--timeout-ms",
        "0",
    ])
    .expect(1, "", "execution deadline exceeded\n");
    vibes(&[&file, "--function", "nope", "--check"]).expect(1, "", "unknown function nope\n");
    vibes(&[&file, "--function", "nope", "--checked"]).expect(1, "", "unknown function nope\n");
    vibes(&[&file, "--function", "nope"]).expect(1, "", "unknown function nope\n");
    let loop_file = files.write(
        "loop.vibe",
        "def run(n)\n  i = 0\n  while i < n\n    i += 1\n  end\n  i\nend\n",
    );
    for mode in [None, Some("--checked")] {
        let mut args = vec![
            loop_file.as_str(),
            "--function",
            "run",
            "--arg",
            "100000",
            "--steps",
            "2000",
        ];
        args.extend(mode);
        let run = vibes(&args);
        assert_eq!(run.status, Some(1), "{mode:?}");
        assert_eq!(run.stdout, "", "{mode:?}");
        assert!(
            run.stderr
                .starts_with("step quota exceeded\n  --> line 4, column 5\n"),
            "{mode:?}: {}",
            run.stderr
        );
    }
    let recursive = files.write("rec.vibe", "def f(n)\n  f(n + 1)\nend\n");
    let run = vibes(&[
        &recursive,
        "--function",
        "f",
        "--arg",
        "0",
        "--recursion",
        "3",
    ]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with("recursion limit exceeded\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn double_dash_ends_option_parsing() {
    let files = Files::new();
    files.write("-dash.vibe", "7\n");
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["--", "-dash.vibe"]).expect(0, "7\n", "");
    let run = vibes_in(dir, &["--stats", "--", "-dash.vibe"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "7\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    vibes_in(dir, &["-dash.vibe"]).expect(2, "", "unknown option -dash.vibe\n");
    vibes_in(dir, &["--", "-dash.vibe", "--stats"]).expect(2, "", "expected one source file\n");
}
