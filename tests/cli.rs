use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

const VIBES: &str = env!("CARGO_BIN_EXE_vibes");
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn checks_initializer_captures_before_allowing_script_output() {
    let files = Files::new();
    let path = files.write(
        "ambient.vibe",
        "x=[1];module M;puts 'effect';Result=x+x.push(2);end;[x,M::Result]",
    );
    vibes(&[&path, "--function", "__main__", "--check"]).expect(0, "", "");
    vibes(&[&path, "--function", "__main__", "--checked"]).expect(
        0,
        "effect\n[[1,2],[1,1,2]]\n",
        "",
    );
    let path = files.write(
        "invalid-ambient.vibe",
        "x=false;module M;puts 'effect';C=x+1;end;M::C",
    );
    let rejected = vibes(&[&path, "--function", "__main__", "--checked"]);
    assert_eq!(rejected.status, Some(1));
    assert!(rejected.stdout.is_empty());
    assert!(rejected.stderr.contains(&path));
    assert!(rejected.stderr.contains("nothing was executed"));
}

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
const INCOMPLETE: &str = "def run\n  puts \"ran\"\n  require(JSON.parse('null'))\nend\n";
const INCOMPLETE_FRAME: &str =
    "  --> line 3, column 3\n 3 |   require(JSON.parse('null'))\n   |   ^\n";

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
fn checks_and_executes_the_total_example_with_bounded_array_reads() {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/total.vibe");
    let file = file.to_str().unwrap();
    for (input, output) in [
        ("[10,20,30]", "{\"total\":60,\"count\":3}\n"),
        ("[]", "{\"total\":0,\"count\":0}\n"),
        ("[10,\"bad\",30]", "{\"total\":\"10bad30\",\"count\":3}\n"),
    ] {
        vibes(&[file, "--function", "total", "--arg", input, "--check"]).expect(0, "", "");
        vibes(&[file, "--function", "total", "--arg", input, "--checked"]).expect(0, output, "");
    }
    let result = vibes(&[
        file,
        "--function",
        "total",
        "--arg",
        "[10,false,30]",
        "--checked",
    ]);
    assert_eq!(result.status, Some(1));
    assert_eq!(result.stdout, "");
    assert!(
        result.stderr.contains("does not accept"),
        "{}",
        result.stderr
    );
    assert!(!result.stderr.contains("incomplete"), "{}", result.stderr);
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
            "vibes check [OPTIONS] [--function NAME] FILE",
            "--kwarg NAME=JSON",
            "--check ",
            "--checked ",
            "Commands:\n  check FILE",
            "check --function NAME FILE",
            "See vibes check --help",
            "--check and --checked cover exactly one call",
            "They do not check unused\nfunctions",
            "recognized only as the first argument",
            "reported as incomplete rather than assumed clean",
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
fn check_help_describes_both_scopes_and_the_command_position() {
    let help = vibes(&["check", "--help"]);
    assert_eq!(help.status, Some(0));
    assert_eq!(help.stderr, "");
    assert!(help.stdout.ends_with('\n'));
    assert_ne!(help.stdout, vibes(&["--help"]).stdout);
    for needle in [
        "Usage: vibes check [OPTIONS] FILE",
        "vibes check [OPTIONS] --function NAME FILE",
        "including unused ones",
        "declared parameter types\nand defaults",
        "no\nconcrete call with supplied values",
        "never assumed clean",
        "Required files that the checker cannot analyze are\nreported as incomplete",
        "Class#method",
        "Namespace.method",
        "Class.new",
        "--steps N",
        "--memory N",
        "--recursion N",
        "--timeout-ms N",
        "--stats ",
        "--arg, --kwarg, --check and\n--checked are usage errors",
        "recognized only as the first argument; vibes -- check runs a file named check",
        "Exit status: 0 for a clean check, 1 when",
    ] {
        assert!(
            help.stdout.contains(needle),
            "{needle:?} missing from:\n{}",
            help.stdout
        );
    }
    for args in [
        vec!["check", "-h"],
        vec!["check", "--help", "--bogus"],
        vec!["check", "--help", "--arg"],
        vec!["check", "--function", "run", "--help"],
    ] {
        vibes(&args).expect(0, &help.stdout, "");
    }
    vibes(&["check", "--version"]).expect(
        0,
        &format!("vibescript.rs {}\n", env!("CARGO_PKG_VERSION")),
        "",
    );
    vibes(&["check", "--bogus", "--help"]).expect(2, "", "unknown option --bogus\n");
    let run = vibes(&["check", "--arg", "--help"]);
    assert_eq!(run.status, Some(2));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with("vibes check does not accept --arg;"),
        "{}",
        run.stderr
    );
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
            "10000",
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

const UNUSED: &str = "7\ndef unused(n:string) -> int\n  n\nend\nclass C\n  private def bad -> bool\n    7\n  end\nend\n";
const METHODS: &str = "class C\n  def initialize(@n:int) -> int\n    \"bad\"\n  end\n  private def read -> int\n    \"bad\"\n  end\n  def self.read -> int\n    7\n  end\nend\nmodule M\n  module N\n    def self.answer -> int\n      7\n    end\n  end\nend\ndef unused -> int\n  false\nend\n";

#[test]
fn check_command_reports_unused_declarations_that_exact_calls_omit() {
    let files = Files::new();
    let file = files.write("unused.vibe", UNUSED);
    vibes(&[&file]).expect(0, "null\n", "");
    vibes(&[&file, "--function", "__main__", "--check"]).expect(0, "", "");
    vibes(&[&file, "--function", "__main__", "--checked"]).expect(0, "null\n", "");
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let lines: Vec<_> = run.stderr.lines().collect();
    assert!(
        lines[0].starts_with(&format!("{file}:3:3: error in unused: ")),
        "{}",
        run.stderr
    );
    assert!(
        lines[0].contains("expected int, got string"),
        "{}",
        run.stderr
    );
    assert_eq!(
        &lines[1..4],
        ["  --> line 3, column 3", " 3 |   n", "   |   ^"]
    );
    assert!(
        lines[4].starts_with(&format!("{file}:7:5: error in bad: ")),
        "{}",
        run.stderr
    );
    assert!(
        lines[4].contains("expected bool, got int"),
        "{}",
        run.stderr
    );
    assert_eq!(
        lines.last().copied(),
        Some(format!("{file}: check of the whole file found 2 errors").as_str())
    );
    assert!(!run.stderr.contains("__main__"), "{}", run.stderr);
    assert!(!run.stderr.contains("incomplete"), "{}", run.stderr);
    assert!(!run.stderr.contains("executed"), "{}", run.stderr);
    assert_eq!(vibes(&["check", "--", &file]).stderr, run.stderr);
    assert_eq!(vibes(&["--", &file, "check"]).status, Some(2));
}

#[test]
fn check_command_uses_top_level_state_and_never_runs_script_output() {
    let files = Files::new();
    let file = files.write(
        "state.vibe",
        "puts \"top\"\nwarn \"careful\"\nx = 7\nmodule M\n  puts \"init\"\n  K = x\n  def self.value -> int\n    puts \"value\"\n    K\n  end\nend\nM.value\n",
    );
    vibes(&["check", &file]).expect(0, "", "");
    let run = vibes(&["check", "--function", "M.value", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with(&format!("{file}:6:7: error in ")),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains(" 6 |   K = x\n"), "{}", run.stderr);
    assert!(
        run.stderr.ends_with(&format!(
            "{file}: check of M.value for its declared parameter types found 1 error\n"
        )),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("incomplete"), "{}", run.stderr);
    vibes(&[&file]).expect(0, "top\ninit\nvalue\n7\n", "careful\n");
    let file = files.write(
        "bad-state.vibe",
        "x = \"bad\"\nmodule M\n  K = x\n  def self.value -> int\n    K\n  end\nend\n",
    );
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .contains(&format!("{file}:5:5: error in value: ")),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .ends_with(&format!("{file}: check of the whole file found 1 error\n")),
        "{}",
        run.stderr
    );
}

#[test]
fn check_function_checks_declared_parameter_types_and_defaults_without_a_call() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&["check", "--function", "run", &file]).expect(0, "", "");
    vibes(&["check", &file, "--function", "run"]).expect(0, "", "");
    let run = vibes(&["check", "--function", "run", &file, "--stats"]);
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    let file = files.write("default.vibe", "def run(x:int=false) -> int\n  x\nend\n");
    let run = vibes(&["check", "--function", "run", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    let (first, rest) = run.stderr.split_once('\n').unwrap();
    assert!(
        first.starts_with(&format!("{file}:1:1: error in run: ")),
        "{first}"
    );
    assert!(first.contains("expected int, got bool"), "{first}");
    assert_eq!(
        rest,
        format!(
            "  --> line 1, column 1\n 1 | def run(x:int=false) -> int\n   | ^\n\
             {file}: check of run for its declared parameter types found 1 error\n"
        )
    );
    vibes(&[&file, "--function", "run", "--arg", "7", "--check"]).expect(0, "", "");
    vibes(&[&file, "--function", "run", "--arg", "7", "--checked"]).expect(0, "7\n", "");
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stderr
            .ends_with(&format!("{file}: check of the whole file found 1 error\n")),
        "{}",
        run.stderr
    );
    let file = files.write("typed.vibe", "def run(x:int) -> int\n  x + false\nend\n");
    let run = vibes(&["check", "--function", "run", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stderr
            .starts_with(&format!("{file}:2:5: error in run: ")),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("does not accept"), "{}", run.stderr);
    assert!(!run.stderr.contains("incomplete"), "{}", run.stderr);
    let file = files.write("untyped.vibe", "def run(x)\n  x + false\nend\n");
    vibes(&["check", "--function", "run", &file]).expect(0, "", "");
    vibes(&["check", &file]).expect(0, "", "");
}

#[test]
fn check_function_selects_methods_and_constructors() {
    let files = Files::new();
    let file = files.write("methods.vibe", METHODS);
    for name in ["C.new", "C.read", "M::N.answer"] {
        vibes(&["check", "--function", name, &file]).expect(0, "", "");
    }
    for (name, function, line) in [("C#initialize", "initialize", 3), ("C#read", "read", 6)] {
        let run = vibes(&["check", "--function", name, &file]);
        assert_eq!(run.status, Some(1), "{name}: {}", run.stderr);
        assert_eq!(run.stdout, "");
        let (first, rest) = run.stderr.split_once('\n').unwrap();
        assert!(
            first.starts_with(&format!("{file}:{line}:5: error in {function}: ")),
            "{first}"
        );
        assert!(first.contains("expected int, got string"), "{first}");
        assert!(
            rest.ends_with(&format!(
                "{file}: check of {name} for its declared parameter types found 1 error\n"
            )),
            "{rest}"
        );
        assert!(rest.contains(" |     \"bad\"\n"), "{rest}");
    }
    vibes(&["check", "--function", "C#missing", &file]).expect(
        1,
        "",
        "unknown function C#missing\n",
    );
    vibes(&["check", "--function", "nope", &file]).expect(1, "", "unknown function nope\n");
    vibes(&[&file, "--function", "C#read", "--check"]).expect(1, "", "unknown function C#read\n");
    vibes(&[&file, "--function", "C.read", "--checked"]).expect(1, "", "unknown function C.read\n");
    vibes(&[&file, "--function", "C.read"]).expect(1, "", "unknown function C.read\n");
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    for (line, function) in [(3, "initialize"), (6, "read"), (20, "unused")] {
        assert!(
            run.stderr.contains(&format!(
                "{file}:{line}:{}: error in {function}: ",
                if line == 20 { 3 } else { 5 }
            )),
            "{}",
            run.stderr
        );
    }
    assert!(
        run.stderr
            .ends_with(&format!("{file}: check of the whole file found 3 errors\n")),
        "{}",
        run.stderr
    );
}

#[test]
fn check_command_rejects_call_options_before_reading_the_file() {
    let files = Files::new();
    let file = files.missing("missing.vibe");
    let other = files.missing("other.vibe");
    let broken = files.write("broken.vibe", "def run(\n");
    let rejected = |option: &str| format!("vibes check does not accept {option};");
    for (args, prefix) in [
        (
            vec!["check"],
            "expected source file; use vibes check --help".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--arg", "1"],
            rejected("--arg"),
        ),
        (
            vec!["check", "--kwarg", "x=1", file.as_str()],
            rejected("--kwarg"),
        ),
        (vec!["check", file.as_str(), "--check"], rejected("--check")),
        (
            vec!["check", "--checked", file.as_str()],
            rejected("--checked"),
        ),
        (
            vec!["check", "--function", "run", file.as_str(), "--arg", "1"],
            rejected("--arg"),
        ),
        (
            vec!["check", broken.as_str(), "--arg", "1"],
            rejected("--arg"),
        ),
        (
            vec!["check", broken.as_str(), "--checked"],
            rejected("--checked"),
        ),
        (vec!["check", "--arg"], rejected("--arg")),
        (
            vec!["check", file.as_str(), "--function"],
            "--function requires NAME".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--steps"],
            "--steps requires N".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--bogus"],
            "unknown option --bogus".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "-x"],
            "unknown option -x".to_owned(),
        ),
        (
            vec!["check", file.as_str(), other.as_str()],
            "expected one source file".to_owned(),
        ),
        (
            vec!["check", broken.as_str(), "--steps", "abc"],
            "invalid --steps value \"abc\": ".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--memory", "-1"],
            "invalid --memory value \"-1\": ".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--recursion", "1.5"],
            "invalid --recursion value \"1.5\": ".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--timeout-ms", "x"],
            "invalid --timeout-ms value \"x\": ".to_owned(),
        ),
    ] {
        let run = vibes(&args);
        assert_eq!(run.status, Some(2), "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(run.stderr.ends_with('\n'), "{args:?}: {}", run.stderr);
        let stderr = run.stderr.trim_end_matches('\n');
        if prefix.ends_with(": ") || prefix.ends_with(';') {
            assert!(stderr.starts_with(&prefix), "{args:?}: {stderr}");
            assert!(stderr.len() > prefix.len(), "{args:?}: {stderr}");
        } else {
            assert_eq!(stderr, prefix, "{args:?}");
        }
        assert!(!stderr.contains("cannot read"), "{args:?}: {stderr}");
        assert!(!stderr.contains("parse error"), "{args:?}: {stderr}");
    }
}

#[test]
fn check_command_reports_missing_unreadable_and_invalid_sources() {
    let files = Files::new();
    let missing = files.missing("missing.vibe");
    for args in [
        vec!["check", missing.as_str()],
        vec!["check", "--function", "run", missing.as_str()],
        vec!["check", "--stats", "--", missing.as_str()],
    ] {
        let run = vibes(&args);
        assert_eq!(run.status, Some(1), "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(
            run.stderr.starts_with(&format!("cannot read {missing}: ")),
            "{args:?}: {}",
            run.stderr
        );
        assert_eq!(run.stderr.lines().count(), 1, "{args:?}: {}", run.stderr);
    }
    let binary = files.0.join("binary.vibe");
    fs::write(&binary, [0xff, b'\n']).unwrap();
    let binary = binary.to_str().unwrap();
    vibes(&["check", binary]).expect(
        1,
        "",
        &format!("cannot read {binary}: stream did not contain valid UTF-8\n"),
    );
    let broken = files.write("broken.vibe", "def run(\n");
    let report = format!(
        "{broken}:2:1: parse error: expected name\n  --> line 2, column 1\n 2 | \n   | ^\n"
    );
    vibes(&["check", &broken]).expect(1, "", &report);
    vibes(&["check", "--function", "run", &broken]).expect(1, "", &report);
    vibes(&["check", "--stats", &broken]).expect(1, "", &report);
}

#[test]
fn check_command_reports_incomplete_analysis_distinctly_and_never_executes() {
    let files = Files::new();
    let file = files.write("incomplete.vibe", INCOMPLETE);
    let run = vibes(&["check", &file]);
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
        format!("{INCOMPLETE_FRAME}{file}: check of the whole file found 1 incomplete path\n")
    );
    let run = vibes(&["check", "--function", "run", &file, "--stats"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let (first, rest) = run.stderr.split_once('\n').unwrap();
    assert!(
        first.starts_with(&format!("{file}:3:3: incomplete in run: ")),
        "{first}"
    );
    let summary =
        format!("{file}: check of run for its declared parameter types found 1 incomplete path\n");
    let (frame, stats) = rest.split_once(&summary).unwrap();
    assert_eq!(frame, INCOMPLETE_FRAME);
    assert_stats_line(stats.trim_end_matches('\n'));
    let mixed = files.write(
        "mixed.vibe",
        "puts \"top\"\nrequire(JSON.parse('null'))\ndef bad -> int\n  puts \"bad\"\n  false\nend\n",
    );
    let run = vibes(&["check", &mixed]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let error = run
        .stderr
        .find(&format!("{mixed}:5:3: error in bad: "))
        .unwrap_or_else(|| panic!("{}", run.stderr));
    let incomplete = run
        .stderr
        .find(&format!("{mixed}:2:1: incomplete in __main__: "))
        .unwrap_or_else(|| panic!("{}", run.stderr));
    assert!(error < incomplete, "{}", run.stderr);
    assert!(
        run.stderr.ends_with(&format!(
            "{mixed}: check of the whole file found 1 error and 1 incomplete path\n"
        )),
        "{}",
        run.stderr
    );
    vibes(&[&mixed, "--function", "__main__", "--check"]).expect(
        1,
        "",
        &format!(
            "{mixed}:2:1: incomplete in __main__: Analysis of this expression is not implemented\n  \
             --> line 2, column 1\n 2 | require(JSON.parse('null'))\n   | ^\n\
             {mixed}: check of __main__ found 1 incomplete path\n"
        ),
    );
}

#[test]
fn check_command_applies_limits_deadlines_and_stats_to_analysis() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    for scope in [vec![], vec!["--function", "run"]] {
        let mut base = vec!["check", file.as_str()];
        base.extend(&scope);
        for (option, value, message) in [
            ("--steps", "1", "step quota exceeded\n"),
            ("--memory", "1", "memory quota exceeded\n"),
            ("--timeout-ms", "0", "execution deadline exceeded\n"),
        ] {
            let mut args = base.clone();
            args.extend([option, value]);
            vibes(&args).expect(1, "", message);
            args.push("--stats");
            vibes(&args).expect(1, "", message);
        }
        let mut clean = base.clone();
        clean.extend([
            "--steps",
            "0",
            "--memory",
            "0",
            "--recursion",
            "3",
            "--timeout-ms",
            "60000",
            "--stats",
        ]);
        let run = vibes(&clean);
        assert_eq!(run.status, Some(0), "{scope:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{scope:?}");
        assert_stats_line(run.stderr.trim_end_matches('\n'));
    }
    let file = files.write("unused.vibe", UNUSED);
    let run = vibes(&["check", &file, "--stats"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    let summary = format!("{file}: check of the whole file found 2 errors\n");
    let (report, stats) = run.stderr.rsplit_once(&summary).unwrap();
    assert!(
        report.starts_with(&format!("{file}:3:3: error in unused: ")),
        "{report}"
    );
    assert_stats_line(stats.trim_end_matches('\n'));
    let run = vibes(&["check", "--function", "C#bad", &file, "--stats"]);
    assert_eq!(run.status, Some(1));
    let summary =
        format!("{file}: check of C#bad for its declared parameter types found 1 error\n");
    let (report, stats) = run.stderr.rsplit_once(&summary).unwrap();
    assert!(
        report.starts_with(&format!("{file}:7:5: error in bad: ")),
        "{report}"
    );
    assert_stats_line(stats.trim_end_matches('\n'));
}

#[test]
fn check_is_a_command_only_as_the_first_argument() {
    let files = Files::new();
    files.write("check", "puts \"ran\"\n7\n");
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["--", "check"]).expect(0, "ran\n7\n", "");
    let run = vibes_in(dir, &["--stats", "check"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "ran\n7\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    vibes_in(dir, &["--function", "__main__", "--checked", "--", "check"])
        .expect(0, "ran\n7\n", "");
    vibes_in(dir, &["check", "--", "check"]).expect(0, "", "");
    vibes_in(dir, &["check", "check"]).expect(0, "", "");
    vibes_in(dir, &["check", "--function", "__main__", "check"]).expect(0, "", "");
    vibes_in(dir, &["check", "--stats", "--", "check"]).expect(0, "", &{
        let run = vibes_in(dir, &["check", "--stats", "check"]);
        assert_stats_line(run.stderr.trim_end_matches('\n'));
        run.stderr
    });
    vibes_in(dir, &["--", "check", "check"]).expect(2, "", "expected one source file\n");
    vibes_in(dir, &["check", "check", "check"]).expect(2, "", "expected one source file\n");
    vibes_in(dir, &["check", "--", "check", "--stats"]).expect(2, "", "expected one source file\n");
    let run = vibes_in(dir, &["check", "check", "--checked"]);
    assert_eq!(run.status, Some(2));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("vibes check does not accept --checked;"),
        "{}",
        run.stderr
    );
    let run = vibes_in(dir, &["check", "--", "-check"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with("cannot read -check: "),
        "{}",
        run.stderr
    );
    vibes_in(dir, &["check", "-check"]).expect(2, "", "unknown option -check\n");
}
