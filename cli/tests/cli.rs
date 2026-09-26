// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

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
    vibes(&[&path, "--function", "__main__"]).expect(0, "effect\n[[1,2],[1,1,2]]\n", "");
    let path = files.write(
        "invalid-ambient.vibe",
        "x=false;module M;puts 'effect';C=x+1;end;M::C",
    );
    let rejected = vibes(&[&path, "--function", "__main__"]);
    assert_eq!(rejected.status, Some(1));
    assert!(rejected.stdout.is_empty());
    assert!(rejected.stderr.contains(&path));
    assert!(
        rejected
            .stderr
            .starts_with("compile failed with 1 diagnostic(s)\n"),
        "{}",
        rejected.stderr
    );
    assert!(
        rejected.stderr.contains("error[V0108]"),
        "{}",
        rejected.stderr
    );
}

/// A unique temporary directory of script files, removed when the test finishes.
struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.cache/tmp");
        fs::create_dir_all(&base).unwrap();
        // Go-style reports print lexically cleaned absolute paths, so the
        // fixtures live under a canonical directory.
        let base = fs::canonicalize(base).unwrap();
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
        fs::create_dir_all(path.parent().unwrap()).unwrap();
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

#[test]
fn sibling_modules_work_for_execution_and_checking() {
    let files = Files::new();
    let elsewhere = Files::new();
    let path = files.write(
        "main.vibe",
        "def run -> int; require(\"helper\").answer; end; run",
    );
    files.write(
        "helper.vibe",
        "puts 'helper initialized'; def answer -> int; 42; end",
    );
    for flags in [vec![], vec!["--function", "run"]] {
        let mut args = vec![path.as_str()];
        args.extend(flags);
        vibes_in(Some(&elsewhere.0), &args).expect(0, "helper initialized\n42\n", "");
    }
    // The check command reports a clean check as the Go reference does.
    vibes_in(Some(&elsewhere.0), &["check", &path]).expect(0, "No issues found\n", "");
}

#[test]
fn extra_module_paths_are_repeatable_ordered_and_relative_to_the_working_directory() {
    let files = Files::new();
    let elsewhere = Files::new();
    let path = files.write(
        "main.vibe",
        "def run -> array<int>; [require(\"priority\").value,require(\"extra\").value]; end; run",
    );
    files.write("priority.vibe", "def value -> int; 7; end");
    elsewhere.write("first/priority.vibe", "def value -> int; 99; end");
    elsewhere.write("first/extra.vibe", "def value -> int; 41; end");
    elsewhere.write("second/priority.vibe", "def value -> int; 88; end");
    elsewhere.write("second/extra.vibe", "def value -> int; 42; end");
    for (first, second, expected) in [
        ("first", "second", "[7,41]\n"),
        ("second", "first", "[7,42]\n"),
    ] {
        let flags = [
            "--module-path",
            first,
            "--module-path",
            first,
            "--module-path",
            second,
        ];
        let mut args = vec![path.as_str()];
        args.extend(flags);
        vibes_in(Some(&elsewhere.0), &args).expect(0, expected, "");
        args.extend(["--function", "run"]);
        vibes_in(Some(&elsewhere.0), &args).expect(0, expected, "");
        // Go-style commands take their flags before the script path.
        let mut args = vec!["check"];
        args.extend(flags);
        args.push(&path);
        vibes_in(Some(&elsewhere.0), &args).expect(0, "No issues found\n", "");
    }
}

#[test]
fn invalid_module_roots_fail_before_script_output_in_both_command_forms() {
    let files = Files::new();
    let path = files.write("main.vibe", "puts 'ran'; 7");
    let ordinary_file = files.write("regular-file", "data");
    for root in [files.missing("missing-directory"), ordinary_file] {
        for args in [
            vec![path.as_str(), "--module-path", root.as_str()],
            vec!["check", "--module-path", root.as_str(), path.as_str()],
            vec!["run", "--module-path", root.as_str(), path.as_str()],
        ] {
            let run = vibes(&args);
            assert_eq!(run.status, Some(1), "{}", run.stderr);
            assert!(run.stdout.is_empty());
            assert!(run.stderr.contains("module path"), "{}", run.stderr);
            assert!(run.stderr.contains(&root), "{}", run.stderr);
        }
    }
    let run = vibes(&[path.as_str(), "--module-path"]);
    assert_eq!(run.status, Some(2));
    assert!(run.stdout.is_empty());
    assert!(run.stderr.contains("--module-path requires DIR"));
    vibes(&["check", "--module-path"]).expect(1, "", "flag needs an argument: -module-path\n");
}

#[test]
fn required_files_resolve_nested_imports_within_the_script_directory() {
    let files = Files::new();
    let elsewhere = Files::new();
    let path = files.write("scripts/main.vibe", "require(\"helper\").value");
    files.write(
        "scripts/helper.vibe",
        "def value -> int; require('./nested/child').value; end",
    );
    files.write("scripts/nested/child.vibe", "def value -> int; 7; end");
    files.write("outside.vibe", "puts 'escaped'; def value -> int; 99; end");
    vibes_in(Some(&elsewhere.0), &[&path]).expect(0, "7\n", "");
    // A relative import outside the configured root is refused at compile time.
    files.write(
        "scripts/escaping.vibe",
        "def value -> bool; begin;require('../outside');false;rescue;true;end;end",
    );
    let path = files.write("scripts/denied.vibe", "require(\"escaping\").value");
    vibes_in(Some(&elsewhere.0), &[&path]).expect(
        1,
        "",
        "compile failed with 1 diagnostic(s)\n\
         escaping.vibe: error[V0201]: cannot statically resolve required module \"../outside\"\n",
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            files.0.join("outside.vibe"),
            files.0.join("scripts/leak.vibe"),
        )
        .unwrap();
        let path = files.write(
            "scripts/linked.vibe",
            "begin; require(\"leak\"); false; rescue; true; end",
        );
        let run = vibes_in(Some(&elsewhere.0), &[&path]);
        assert_eq!(run.status, Some(1));
        assert_eq!(run.stdout, "");
        assert!(
            run.stderr.contains(&format!(
                "{path}:1:8: error[V0201]: cannot statically resolve required module \"leak\"\n"
            )),
            "{}",
            run.stderr
        );
    }
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
    let file = files.write(
        "pair.vibe",
        "puts \"top\"\ndef pair(a: int, b: any) -> array<any>\n  [a, b]\nend\n",
    );
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
    let file = files.write(
        "kw.vibe",
        "def run(a:int, *, b: int = 2, **rest: hash<string, int?>) -> array<int?>;[a,b,rest[\"x\"]];end\n",
    );
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
    let mut args = vec![file.as_str()];
    args.extend(inputs);
    vibes(&args).expect(0, "[7,5,9]\n", "");
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
fn calls_execute_only_with_arguments_of_the_declared_types() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&[&file, "--function", "run", "--arg", "41"]).expect(0, "ran\n42\n", "");
    let run = vibes(&[&file, "--function", "run", "--arg", "41", "--stats"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "ran\n42\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    // JSON arguments are dynamic, so the call checks them when it starts.
    vibes(&[&file, "--function", "run", "--arg", "\"bad\""]).expect(
        1,
        "",
        &format!("argument x expected int, got string\n{ADD_FRAME}  at <script> (2:1)\n"),
    );
}

#[test]
fn checks_and_executes_the_typed_total_example() {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/total.vibe");
    let file = file.to_str().unwrap();
    for (input, output) in [
        ("[10,20,30]", "{\"total\":60,\"count\":3}\n"),
        ("[]", "{\"total\":0,\"count\":0}\n"),
    ] {
        vibes(&[file, "--function", "total", "--arg", input]).expect(0, output, "");
    }
    for input in ["[10,\"bad\",30]", "[10,false,30]"] {
        let result = vibes(&[file, "--function", "total", "--arg", input]);
        assert_eq!(result.status, Some(1));
        assert_eq!(result.stdout, "");
        assert!(
            result
                .stderr
                .starts_with("argument items expected array<int>, got array<"),
            "{}",
            result.stderr
        );
    }
}

#[test]
fn checked_conversion_errors_precede_all_script_output() {
    let files = Files::new();
    let file = files.write(
        "conversion.vibe",
        "class C\n  def to_s -> int\n    puts 'converted'\n    false\n  end\nend\ndef run\n  puts 'started'\n  \"#{C.new}\"\nend\n",
    );
    let result = vibes(&[&file, "--function", "run"]);
    assert_eq!(result.status, Some(1), "{}", result.stderr);
    assert_eq!(result.stdout, "");
    assert!(
        result.stderr.contains(&format!(
            "{file}:4:5: error[V0101]: `C#to_s` returns int, found bool\n"
        )),
        "{}",
        result.stderr
    );
}

#[test]
fn type_errors_are_reported_with_position_and_code_frame() {
    let files = Files::new();
    let file = files.write("ret.vibe", "def run() -> int\n  \"é\"\nend\n");
    let report = format!(
        "compile failed with 1 diagnostic(s)\n\
         {file}:2:3: error[V0101]: `run` returns int, found string\n   |\n  2|   \"é\"\n   |   ^^^\n   \
         = expected int, found string\n"
    );
    vibes(&[&file, "--function", "run"]).expect(1, "", &report);
    vibes(&[&file]).expect(1, "", &report);
}

#[test]
fn source_read_and_parse_errors_carry_the_filename_and_exit_nonzero() {
    let files = Files::new();
    let missing = files.missing("missing.vibe");
    for args in [
        vec![missing.as_str()],
        vec![missing.as_str(), "--function", "run"],
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
        "{broken}:2:0: parse error: expected parameter name, got end of input\n  --> line 2, column 1\n 2 | \n   | ^\n"
    );
    vibes(&[&broken]).expect(1, "", &report);
    vibes(&[&broken, "--function", "run"]).expect(1, "", &report);
}

#[test]
fn usage_errors_exit_with_status_two_without_reading_the_file() {
    let files = Files::new();
    let file = files.missing("missing.vibe");
    let other = files.missing("other.vibe");
    for (args, message) in [
        (
            vec!["--stats"],
            "expected source file or -e SOURCE; use vibes help flat",
        ),
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
fn help_explains_the_flat_form_and_version_prints() {
    // The root help is the Go reference's; the flat form has its own topic.
    for flag in ["--help", "-h"] {
        let run = vibes(&[flag]);
        assert_eq!(run.status, Some(0));
        assert_eq!(run.stderr, "");
        assert!(
            run.stdout
                .starts_with("NAME:\n   vibes - run Vibescript programs")
        );
    }
    let run = vibes(&["help", "flat"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stderr, "");
    for needle in [
        "Usage: vibes [OPTIONS] FILE",
        "vibes [OPTIONS] --function NAME [--arg JSON]... [--kwarg NAME=JSON]... FILE",
        "--kwarg NAME=JSON",
        "-e, --eval SOURCE",
        "vibes [OPTIONS] -e SOURCE",
        "The flat form type checks FILE or the inline SOURCE",
        "A first argument that names a command, such as\nrun or check, always selects that command instead.",
        "A source with type errors does not run; its diagnostics are printed on stderr.",
        "Exit status: 0 on success, 1 when reading, compiling or execution fails",
    ] {
        assert!(
            run.stdout.contains(needle),
            "{needle:?} missing from:\n{}",
            run.stdout
        );
    }
    for removed in ["--check ", "--checked "] {
        assert!(!run.stdout.contains(removed), "{removed:?}");
    }
    assert!(run.stdout.ends_with('\n'));
    let flat_help = run.stdout;
    vibes(&["-e", "1", "--help"]).expect(0, &flat_help, "");
    vibes(&["--stats", "-h"]).expect(0, &flat_help, "");
    vibes(&["--version"]).expect(
        0,
        &format!("vibescript.rs {}\n", env!("CARGO_PKG_VERSION")),
        "",
    );
    vibes(&["--help", "--bogus"]).expect(0, &vibes(&["-h"]).stdout, "");
    vibes(&["--bogus", "--help"]).expect(1, "", "flag provided but not defined: -bogus\n");
}

#[test]
fn check_help_describes_the_type_check_and_the_command_position() {
    let help = vibes(&["check", "--help"]);
    assert_eq!(help.status, Some(0));
    assert_eq!(help.stderr, "");
    assert!(help.stdout.ends_with('\n'));
    assert_ne!(help.stdout, vibes(&["--help"]).stdout);
    for needle in [
        "NAME:\n   vibes check - type check a script without executing it\n",
        "USAGE:\n   vibes check [options] <script>\n",
        "--module-path string [ --module-path string ]  add a module search directory (repeatable)\n",
        "--eval string, -e string ",
        "--json ",
        "--help, -h ",
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
        vec!["check", "--json", "--help"],
        vec!["help", "check"],
    ] {
        vibes(&args).expect(0, &help.stdout, "");
    }
    // Go-style flag parsing rejects flags the command does not define.
    vibes(&["check", "--version"]).expect(1, "", "flag provided but not defined: -version\n");
    vibes(&["check", "--bogus", "--help"]).expect(1, "", "flag provided but not defined: -bogus\n");
    vibes(&["check", "--arg", "--help"]).expect(1, "", "flag provided but not defined: -arg\n");
}

#[test]
fn limits_deadlines_and_unknown_functions_fail_with_nonzero_status() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&[&file, "--function", "run", "--arg", "1", "--steps", "1"]).expect(
        1,
        "",
        &format!("step quota exceeded (1)\n{ADD_FRAME}  at <script> (2:1)\n"),
    );
    vibes(&[&file, "--function", "run", "--arg", "1", "--memory", "1"]).expect(
        1,
        "",
        &format!("memory quota exceeded (1 bytes)\n{ADD_FRAME}  at <script> (2:1)\n"),
    );
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
    vibes(&[&file, "--function", "nope"]).expect(1, "", "function nope not found\n");
    let loop_file = files.write(
        "loop.vibe",
        "def run(n: int) -> int\n  i = 0\n  while i < n\n    i += 1\n  end\n  i\nend\n",
    );
    let run = vibes(&[
        &loop_file,
        "--function",
        "run",
        "--arg",
        "100000",
        "--steps",
        "10000",
    ]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("step quota exceeded (10000)\n  --> line 3, column 3\n"),
        "{}",
        run.stderr
    );
    let recursive = files.write("rec.vibe", "def f(n: int) -> int\n  f(n + 1)\nend\n");
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
        run.stderr
            .starts_with("recursion depth exceeded (limit 3)\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn double_dash_ends_option_parsing() {
    let files = Files::new();
    files.write("-dash.vibe", "7\n");
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["run", "--", "-dash.vibe"]).expect(0, "7\n", "");
    let run = vibes_in(dir, &["--stats", "--", "-dash.vibe"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "7\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    vibes_in(dir, &["--stats", "--", "-dash.vibe", "--stats"]).expect(
        2,
        "",
        "expected one source file\n",
    );
    // As in the Go reference, `--` cannot escape command selection, and an
    // unknown flag in the first position is an error.
    let run = vibes_in(dir, &["--", "-dash.vibe"]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr.ends_with("unknown command \"--\"\n"),
        "{}",
        run.stderr
    );
    vibes_in(dir, &["-dash.vibe"]).expect(1, "", "flag provided but not defined: -dash.vibe\n");
}

const UNUSED: &str = "7\ndef unused(n:string) -> int\n  n\nend\nclass C\n  private def bad -> bool\n    7\n  end\nend\n";
const METHODS: &str = "class C\n  def initialize(n: int) -> int\n    \"bad\"\n  end\n  private def read -> int\n    \"bad\"\n  end\n  def self.read -> int\n    7\n  end\nend\nmodule M\n  module N\n    def self.answer -> int\n      7\n    end\n  end\nend\ndef unused -> int\n  false\nend\n";

#[test]
fn check_command_reports_errors_in_unused_declarations() {
    let files = Files::new();
    let file = files.write("unused.vibe", UNUSED);
    // Every declaration is checked, so neither form runs the script.
    let run = vibes(&[&file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("compile failed with 2 diagnostic(s)\n"),
        "{}",
        run.stderr
    );
    let run = vibes(&[&file, "--function", "__main__"]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    // The check command prints each diagnostic with its source line on
    // stdout and summarizes the failure on stderr.
    let run = vibes(&["check", &file]);
    run.expect(
        1,
        &format!(
            "{file}:3:3: error[V0101]: `unused` returns int, found string\n   |\n  3|   n\n   |   ^\n   \
             = expected int, found string\n\
             {file}:7:5: error[V0101]: `C#bad` returns bool, found int\n   |\n  7|     7\n   |     ^\n   \
             = expected bool, found int\n"
        ),
        "check failed with 2 error(s)\n",
    );
    assert_eq!(vibes(&["check", "--", &file]).stdout, run.stdout);
    let run = vibes(&["--", &file, "check"]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr.ends_with("unknown command \"--\"\n"),
        "{}",
        run.stderr
    );
}

#[test]
fn check_command_uses_top_level_state_and_never_runs_script_output() {
    let files = Files::new();
    let file = files.write(
        "state.vibe",
        "puts \"top\"\nwarn \"careful\"\nx = 7\nmodule M\n  puts \"init\"\n  K = x\n  def self.value -> int\n    puts \"value\"\n    K\n  end\nend\nM.value\n",
    );
    vibes(&["check", &file]).expect(0, "No issues found\n", "");
    vibes(&[&file]).expect(0, "top\ninit\nvalue\n7\n", "careful\n");
    let file = files.write(
        "bad-state.vibe",
        "x = \"bad\"\nmodule M\n  K = x\n  def self.value -> int\n    K\n  end\nend\n",
    );
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout.starts_with(&format!(
            "{file}:5:5: error[V0101]: `M.value` returns int, found string\n"
        )),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "check failed with 1 error(s)\n");
}

#[test]
fn check_checks_declared_parameter_types_and_defaults_without_a_call() {
    let files = Files::new();
    let file = files.write("add.vibe", ADD);
    vibes(&["check", &file]).expect(0, "No issues found\n", "");
    // Flags must precede the script path, as in the Go reference.
    vibes(&["check", &file, "--json"]).expect(
        1,
        "",
        "vibes check: expected a single script path\n",
    );
    let file = files.write("default.vibe", "def run(x:int=false) -> int\n  x\nend\n");
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout.starts_with(&format!(
            "{file}:1:15: error[V0101]: `x` is int, found bool\n"
        )),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "check failed with 1 error(s)\n");
    // The flat form refuses the file even for a call that passes the parameter.
    let run = vibes(&[&file, "--function", "run", "--arg", "7"]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    let file = files.write("typed.vibe", "def run(x:int) -> int\n  x + false\nend\n");
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout.starts_with(&format!(
            "{file}:2:5: error[V0108]: `+` is not defined for int and bool\n"
        )),
        "{}",
        run.stdout
    );
    // A parameter must declare its type.
    let file = files.write("untyped.vibe", "def run(x)\n  x + false\nend\n");
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with(&format!("{file}:1:9: error[V0118]: ")),
        "{}",
        run.stdout
    );
}

#[test]
fn check_reports_errors_in_methods_and_constructors() {
    let files = Files::new();
    let file = files.write("methods.vibe", METHODS);
    // The flat form refuses the file before it looks up the function.
    let run = vibes(&[&file, "--function", "C.read"]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("compile failed with 3 diagnostic(s)\n"),
        "{}",
        run.stderr
    );
    let run = vibes(&["check", &file]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    for (line, function) in [(3, "C#initialize"), (6, "C#read"), (20, "unused")] {
        assert!(
            run.stdout.contains(&format!(
                "{file}:{line}:{}: error[V0101]: `{function}` returns ",
                if line == 20 { 3 } else { 5 }
            )),
            "{}",
            run.stdout
        );
    }
    assert_eq!(run.stderr, "check failed with 3 error(s)\n");
}

#[test]
fn check_command_rejects_call_options_before_reading_the_file() {
    let files = Files::new();
    let file = files.missing("missing.vibe");
    let other = files.missing("other.vibe");
    let broken = files.write("broken.vibe", "def run(\n");
    let single = "vibes check: expected a single script path";
    let undefined = |flag: &str| format!("flag provided but not defined: -{flag}");
    let invalid = |value: &str, flag: &str| {
        format!("invalid boolean value \"{value}\" for -{flag}: parse error")
    };
    // The check command follows the Go reference's flag syntax: flags come
    // before the script path, undefined flags are errors, and every usage
    // error exits with status 1 before any file is read.
    for (args, message) in [
        (
            vec!["check"],
            "vibes check: script path required".to_owned(),
        ),
        (
            vec!["check", file.as_str(), "--arg", "1"],
            single.to_owned(),
        ),
        (
            vec!["check", "--kwarg", "x=1", file.as_str()],
            undefined("kwarg"),
        ),
        (vec!["check", file.as_str(), "--check"], single.to_owned()),
        (
            vec!["check", "--checked", file.as_str()],
            undefined("checked"),
        ),
        (
            vec!["check", "--json", file.as_str(), "--arg", "1"],
            single.to_owned(),
        ),
        (
            vec!["check", broken.as_str(), "--arg", "1"],
            single.to_owned(),
        ),
        (
            vec!["check", broken.as_str(), "--checked"],
            single.to_owned(),
        ),
        (vec!["check", "--arg"], undefined("arg")),
        (
            vec!["check", file.as_str(), "--function"],
            single.to_owned(),
        ),
        (
            vec!["check", "--module-path"],
            "flag needs an argument: -module-path".to_owned(),
        ),
        // The type check runs no call, so it takes no call options.
        (vec!["check", "--function", "run"], undefined("function")),
        (vec!["check", "--steps"], undefined("steps")),
        (vec!["check", "--bogus", file.as_str()], undefined("bogus")),
        (vec!["check", "-x", file.as_str()], undefined("x")),
        (
            vec!["check", file.as_str(), other.as_str()],
            single.to_owned(),
        ),
        (
            vec!["check", "--json=abc", broken.as_str()],
            invalid("abc", "json"),
        ),
        (
            vec!["check", "--memory", "-1", file.as_str()],
            undefined("memory"),
        ),
        (
            vec!["check", "--recursion", "1.5", file.as_str()],
            undefined("recursion"),
        ),
        (
            vec!["check", "--timeout-ms", "x", file.as_str()],
            undefined("timeout-ms"),
        ),
    ] {
        let run = vibes(&args);
        assert_eq!(run.status, Some(1), "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{args:?}");
        assert_eq!(run.stderr, format!("{message}\n"), "{args:?}");
    }
}

#[test]
fn check_command_reports_missing_unreadable_and_invalid_sources() {
    let files = Files::new();
    let missing = files.missing("missing.vibe");
    for args in [
        vec!["check", missing.as_str()],
        vec!["check", "--json", missing.as_str()],
        vec!["check", "--", missing.as_str()],
    ] {
        vibes(&args).expect(
            1,
            "",
            &format!("read script: open {missing}: no such file or directory\n"),
        );
    }
    // Invalid UTF-8 is decoded with replacement characters, as the Go
    // reference's lexer does, so it is a parse error rather than a read error.
    let binary = files.0.join("binary.vibe");
    fs::write(&binary, [0xff, b'\n']).unwrap();
    let binary = binary.to_str().unwrap();
    let run = vibes(&["check", binary]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("compile failed: parse error at 1:1: "),
        "{}",
        run.stderr
    );
    let broken = files.write("broken.vibe", "def run(\n");
    let report = "compile failed: parse error at 2:0: expected parameter name, got end of input\n  --> line 2, column 1\n 2 | \n   | ^\n";
    vibes(&["check", &broken]).expect(1, "", report);
    let directory = files.0.to_str().unwrap();
    vibes(&["check", directory]).expect(
        1,
        "",
        &format!("read script: {directory} is not a regular file\n"),
    );
}

#[test]
fn check_is_a_command_only_as_the_first_argument() {
    let files = Files::new();
    files.write("check", "puts \"ran\"\n7\n");
    let dir = Some(files.0.as_path());
    let run = vibes_in(dir, &["--stats", "check"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "ran\n7\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    vibes_in(dir, &["--function", "__main__", "--", "check"]).expect(0, "ran\n7\n", "");
    vibes_in(dir, &["run", "check"]).expect(0, "ran\n7\n", "");
    vibes_in(dir, &["run", "--", "check"]).expect(0, "ran\n7\n", "");
    vibes_in(dir, &["check", "--", "check"]).expect(0, "No issues found\n", "");
    vibes_in(dir, &["check", "check"]).expect(0, "No issues found\n", "");
    vibes_in(dir, &["check", "--json", "check"]).expect(0, "", "");
    vibes_in(dir, &["check", "--json", "--", "check"]).expect(0, "", "");
    // `--` cannot escape command selection at the root, as in the Go reference.
    let run = vibes_in(dir, &["--", "check", "check"]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr.ends_with("unknown command \"--\"\n"),
        "{}",
        run.stderr
    );
    let single = "vibes check: expected a single script path\n";
    vibes_in(dir, &["check", "check", "check"]).expect(1, "", single);
    vibes_in(dir, &["check", "--", "check", "--json"]).expect(1, "", single);
    vibes_in(dir, &["check", "check", "--checked"]).expect(1, "", single);
    vibes_in(dir, &["check", "--", "-check"]).expect(
        1,
        "",
        &format!(
            "read script: open {}/-check: no such file or directory\n",
            files.0.display()
        ),
    );
    vibes_in(dir, &["check", "-check"]).expect(1, "", "flag provided but not defined: -check\n");
}

const EFFECT_UNUSED: &str = "puts \"effect\"\ndef unused(n:string) -> int\n  n\nend\n";
const RET: &str = "def run() -> int\n  \"é\"\nend";
const RET_REPORT: &str = "<eval>:2:3: error[V0101]: `run` returns int, found string\n   |\n  2|   \"é\"\n   |   ^^^\n   \
     = expected int, found string\n";

#[test]
fn inline_source_runs_calls_and_checks() {
    vibes(&["-e", "1 + 2"]).expect(0, "3\n", "");
    vibes(&["--eval", "x = 2\ny = 3\nx * y"]).expect(0, "6\n", "");
    vibes(&["-e", "puts \"hi\"\nwarn \"careful\"\n{a: [1, 2]}"]).expect(
        0,
        "hi\n{\"a\":[1,2]}\n",
        "careful\n",
    );
    let run = vibes(&["-e", "1 + 2", "--stats"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "3\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    let call = ["--function", "run", "--arg", "41", "--kwarg", "b=5"];
    let source =
        "puts \"top\"\ndef run(x:int, *, b: int = 2) -> int\n  puts \"ran\"\n  x + b\nend\n";
    for flag in ["-e", "--eval"] {
        let mut args = vec![flag, source];
        args.extend(call);
        vibes(&args).expect(0, "ran\n46\n", "");
    }
    vibes(&["check", "-e", source]).expect(0, "No issues found\n", "");
    vibes(&["check", "--eval", source]).expect(0, "No issues found\n", "");
    let run = vibes(&["check", "-e", METHODS]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stdout
            .starts_with("<eval>:3:5: error[V0101]: `C#initialize` returns int, found string\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("<eval>:6:5: error[V0101]: `C#read` returns int, found string\n"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "check failed with 3 error(s)\n");
    let run = vibes(&["-e", METHODS, "--function", "C#read"]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert!(
        run.stderr
            .starts_with("compile failed with 3 diagnostic(s)\n<eval>:3:5: error[V0101]: "),
        "{}",
        run.stderr
    );
}

#[test]
fn inline_snippets_report_errors_in_unused_declarations() {
    let report = "compile failed with 1 diagnostic(s)\n\
                  <eval>:3:3: error[V0101]: `unused` returns int, found string\n   |\n  3|   n\n   |   ^\n   \
                  = expected int, found string\n";
    vibes(&["-e", EFFECT_UNUSED]).expect(1, "", report);
    vibes(&["-e", EFFECT_UNUSED, "--function", "__main__"]).expect(1, "", report);
    vibes(&["--eval", EFFECT_UNUSED, "--stats"]).expect(1, "", report);
    let issue = "<eval>:3:3: error[V0101]: `unused` returns int, found string\n   |\n  3|   n\n   |   ^\n   \
                 = expected int, found string\n";
    vibes(&["check", "-e", EFFECT_UNUSED]).expect(1, issue, "check failed with 1 error(s)\n");
    let run = vibes(&["-e", UNUSED]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.starts_with(
            "compile failed with 2 diagnostic(s)\n<eval>:3:3: error[V0101]: `unused` returns "
        ),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains("<eval>:7:5: error[V0101]: `C#bad` returns "),
        "{}",
        run.stderr
    );
    let run = vibes(&["check", "-e", UNUSED]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stdout
            .starts_with("<eval>:3:3: error[V0101]: `unused` returns int, found string\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout
            .contains("<eval>:7:5: error[V0101]: `C#bad` returns bool, found int\n"),
        "{}",
        run.stdout
    );
    assert_eq!(run.stderr, "check failed with 2 error(s)\n");
}

#[test]
fn inline_source_diagnostics_use_the_eval_label_and_keep_module_filenames() {
    let report = format!("compile failed with 1 diagnostic(s)\n{RET_REPORT}");
    vibes(&["-e", RET, "--function", "run"]).expect(1, "", &report);
    vibes(&["-e", RET]).expect(1, "", &report);
    vibes(&["check", "-e", RET]).expect(1, RET_REPORT, "check failed with 1 error(s)\n");
    let parse_error = "<eval>:2:0: parse error: expected parameter name, got end of input\n  --> line 2, column 1\n 2 | \n   | ^\n";
    vibes(&["-e", "def run(\n"]).expect(1, "", parse_error);
    vibes(&["-e", "def run(\n", "--function", "run"]).expect(1, "", parse_error);
    // Go-style commands report a snippet that ends early as the reference does.
    vibes(&["check", "-e", "def run(\n"]).expect(
        1,
        "",
        "compile failed: parse error at 2:1: unexpected end of snippet\n  \
         --> line 2, column 1\n 2 | \n   | ^\n",
    );
    vibes(&["-e", "x = \"é\"\nputs x\n[x == \"é\", 2]"]).expect(0, "é\n[true,2]\n", "");
    let files = Files::new();
    files.write("bad.vibe", "def wrong -> int\n  false\nend\n");
    let dir = Some(files.0.as_path());
    let source = "require(\"bad\").wrong";
    // A diagnostic in a required module names the module, not <eval>.
    let module_report = "bad.vibe: error[V0101]: `wrong` returns int, found bool\n";
    for args in [
        vec!["-e", source, "--function", "__main__"],
        vec!["-e", source],
    ] {
        vibes_in(dir, &args).expect(
            1,
            "",
            &format!("compile failed with 1 diagnostic(s)\n{module_report}"),
        );
    }
    vibes_in(dir, &["check", "-e", source]).expect(
        1,
        module_report,
        "check failed with 1 error(s)\n",
    );
}

#[test]
fn inline_require_searches_the_working_directory_then_supplied_roots() {
    let files = Files::new();
    let other = Files::new();
    let elsewhere = Files::new();
    files.write(
        "helper.vibe",
        "puts 'helper initialized'; def value -> int; 7; end",
    );
    other.write("helper.vibe", "def value -> int; 99; end");
    other.write("extra.vibe", "def value -> int; 42; end");
    let source = "require(\"helper\").value";
    vibes_in(Some(&files.0), &["-e", source]).expect(0, "helper initialized\n7\n", "");
    let files_root = files.0.to_str().unwrap();
    let other_root = other.0.to_str().unwrap();
    vibes_in(Some(&files.0), &["-e", source, "--module-path", other_root]).expect(
        0,
        "helper initialized\n7\n",
        "",
    );
    vibes_in(Some(&other.0), &["-e", source, "--module-path", files_root]).expect(0, "99\n", "");
    vibes_in(
        Some(&elsewhere.0),
        &["-e", source, "--module-path", files_root],
    )
    .expect(0, "helper initialized\n7\n", "");
    let both = "[require(\"helper\").value, require(\"extra\").value]";
    vibes_in(
        Some(&elsewhere.0),
        &[
            "--module-path",
            files_root,
            "--module-path",
            other_root,
            "-e",
            both,
        ],
    )
    .expect(0, "helper initialized\n[7,42]\n", "");
    for args in [
        vec![
            "check",
            "--module-path",
            files_root,
            "--module-path",
            other_root,
            "-e",
            both,
        ],
        vec![
            "-e",
            both,
            "--module-path",
            files_root,
            "--module-path",
            other_root,
            "--function",
            "__main__",
        ],
    ] {
        let run = vibes_in(Some(&elsewhere.0), &args);
        assert_eq!(run.status, Some(0), "{args:?}: {}", run.stderr);
        assert_eq!(run.stderr, "", "{args:?}");
        let expected = if args[0] == "check" {
            "No issues found\n"
        } else {
            "helper initialized\n[7,42]\n"
        };
        assert_eq!(run.stdout, expected, "{args:?}");
    }
    let run = vibes_in(Some(&elsewhere.0), &["-e", source]);
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(!run.stderr.is_empty());
    let missing = elsewhere.missing("missing-directory");
    let ordinary = files.write("regular-file", "data");
    for root in [missing.as_str(), ordinary.as_str()] {
        for args in [
            vec!["-e", "puts 'effect'", "--module-path", root],
            vec!["check", "-e", "puts 'effect'", "--module-path", root],
            vec!["run", "-e", "puts 'effect'", "--module-path", root],
        ] {
            let run = vibes_in(Some(&files.0), &args);
            assert_eq!(run.status, Some(1), "{args:?}: {}", run.stderr);
            assert_eq!(run.stdout, "", "{args:?}");
            assert!(run.stderr.contains("module path"), "{}", run.stderr);
            assert!(run.stderr.contains(root), "{}", run.stderr);
        }
    }
}

#[test]
fn inline_usage_errors_exit_with_status_two_before_any_read_or_effect() {
    let files = Files::new();
    let broken = files.write("broken.vibe", "def run(\n");
    let missing = files.missing("missing.vibe");
    let missing_root = files.missing("missing-directory");
    let effect = "puts \"effect\"";
    for (args, message) in [
        (vec!["-e"], "-e requires SOURCE"),
        (vec!["--eval"], "--eval requires SOURCE"),
        (vec!["--stats", effect, "-e"], "-e requires SOURCE"),
        (
            vec!["-e", effect, "-e", effect],
            "expected one inline source; -e or --eval was given twice",
        ),
        (
            vec!["-e", effect, broken.as_str()],
            "expected FILE or -e SOURCE, not both",
        ),
        (
            vec![broken.as_str(), "--eval", effect],
            "expected FILE or -e SOURCE, not both",
        ),
        (
            vec!["-e", effect, "--", missing.as_str()],
            "expected FILE or -e SOURCE, not both",
        ),
        (vec!["-e", effect, "--checked"], "unknown option --checked"),
        (
            vec!["-e", effect, "--arg", "1"],
            "--arg requires --function",
        ),
        (
            vec!["-e", effect, "--kwarg", "x=1"],
            "--kwarg requires --function",
        ),
        (vec![missing.as_str(), "--check"], "unknown option --check"),
        (
            vec![
                "-e",
                effect,
                "--module-path",
                missing_root.as_str(),
                "--bogus",
            ],
            "unknown option --bogus",
        ),
        (
            vec!["-e", effect, "--function", "run", "--arg", "{"],
            "invalid JSON for --arg: ",
        ),
        (
            vec!["-e", effect, "--steps", "abc"],
            "invalid --steps value \"abc\": ",
        ),
    ] {
        let run = vibes(&args);
        assert_eq!(run.status, Some(2), "{args:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(run.stderr.ends_with('\n'), "{args:?}: {}", run.stderr);
        let stderr = run.stderr.trim_end_matches('\n');
        if message.ends_with(": ") || message.ends_with(';') {
            assert!(stderr.starts_with(message), "{args:?}: {stderr}");
            assert!(stderr.len() > message.len(), "{args:?}: {stderr}");
        } else {
            assert_eq!(stderr, message, "{args:?}");
        }
        assert!(!stderr.contains("cannot read"), "{args:?}: {stderr}");
        assert!(!stderr.contains("parse error"), "{args:?}: {stderr}");
        assert!(!stderr.contains("module path"), "{args:?}: {stderr}");
    }
    // The check command parses its flags as the Go reference does: every
    // failure exits with status 1, a repeated flag keeps its last value, and
    // flags end at the first positional argument.
    for (args, message) in [
        (vec!["check", "-e"], "flag needs an argument: -e"),
        (
            vec!["check", missing.as_str(), "-e", effect],
            "vibes check: expected a single script path",
        ),
        (
            vec!["check", "-e", effect, missing.as_str()],
            "vibes check: -e does not accept positional arguments",
        ),
        (
            vec!["check", "-e", effect, "--checked"],
            "flag provided but not defined: -checked",
        ),
        (
            vec!["check", "-e", effect, "--arg", "1"],
            "flag provided but not defined: -arg",
        ),
    ] {
        vibes(&args).expect(1, "", &format!("{message}\n"));
    }
    vibes(&["check", "--eval", "1 +", "--eval", effect]).expect(0, "No issues found\n", "");
    // A first argument that is neither a command nor a flat-form option or
    // script path is an unknown command, as in the Go reference.
    let run = vibes(&[effect, "-e"]);
    assert_eq!(run.status, Some(1));
    assert!(
        run.stderr
            .ends_with("unknown command \"puts \\\"effect\\\"\"\n"),
        "{}",
        run.stderr
    );
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let output = Command::new(VIBES)
            .arg("-e")
            .arg(OsStr::from_bytes(b"puts 1\xff"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"-e value is not valid UTF-8\n");
        // Go-style commands decode invalid UTF-8 with replacement characters.
        let output = Command::new(VIBES)
            .args(["check", "-e"])
            .arg(OsStr::from_bytes(b"puts 1\xff"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            output
                .stderr
                .starts_with(b"compile failed: parse error at 1:7: ")
        );
    }
}

/// Confirms option-like source reaches the compiler, which reports the
/// missing name.
fn assert_lookup_failure(run: &Run, name: &str) {
    assert_eq!(run.status, Some(1), "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("compile failed with 1 diagnostic(s)\n<eval>:1:"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr
            .contains(&format!(": error[V0201]: `{name}` is not a local")),
        "{}",
        run.stderr
    );
}

#[test]
fn inline_option_values_are_literal_and_double_dash_still_names_a_file() {
    vibes(&["-e", "-7"]).expect(0, "-7\n", "");
    vibes(&["--eval", "-7", "--"]).expect(0, "-7\n", "");
    assert_lookup_failure(&vibes(&["-e", "--stats"]), "stats");
    vibes(&["check", "-e", "-7"]).expect(0, "No issues found\n", "");
    assert_lookup_failure(&vibes(&["-e", "check"]), "check");
    let files = Files::new();
    files.write("-e", "puts \"file\"\n7\n");
    files.write("check", "8\n");
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["run", "--", "-e"]).expect(0, "file\n7\n", "");
    let run = vibes_in(dir, &["--stats", "--", "-e"]);
    assert_eq!(run.stdout, "file\n7\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    vibes_in(dir, &["check", "--", "-e"]).expect(0, "No issues found\n", "");
    vibes_in(dir, &["-e"]).expect(2, "", "-e requires SOURCE\n");
    assert_lookup_failure(&vibes_in(dir, &["-e", "check"]), "check");
    assert_lookup_failure(&vibes_in(dir, &["-e", "-- check"]), "check");
    vibes_in(dir, &["-e", "7", "--", "check"]).expect(
        2,
        "",
        "expected FILE or -e SOURCE, not both\n",
    );
}

#[test]
fn empty_inline_source_is_valid_in_every_scope() {
    for source in ["", "\n", "  \n\n"] {
        vibes(&["-e", source]).expect(0, "null\n", "");
        vibes(&["-e", source, "--function", "__main__"]).expect(0, "null\n", "");
        vibes(&["check", "-e", source]).expect(0, "No issues found\n", "");
        vibes(&["check", "--json", "--eval", source]).expect(0, "", "");
        vibes(&["-e", source, "--function", "run"]).expect(1, "", "function run not found\n");
    }
    let run = vibes(&["--eval", "", "--stats"]);
    assert_eq!(run.status, Some(0));
    assert_eq!(run.stdout, "null\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
}

#[test]
fn inline_source_honors_quotas_deadlines_and_stats() {
    let frame = format!("{ADD_FRAME}  at <script> (2:1)\n");
    vibes(&["-e", ADD, "--function", "run", "--arg", "1", "--steps", "1"]).expect(
        1,
        "",
        &format!("step quota exceeded (1)\n{frame}"),
    );
    vibes(&[
        "-e",
        ADD,
        "--function",
        "run",
        "--arg",
        "1",
        "--memory",
        "1",
    ])
    .expect(1, "", &format!("memory quota exceeded (1 bytes)\n{frame}"));
    vibes(&[
        "-e",
        ADD,
        "--function",
        "run",
        "--arg",
        "1",
        "--timeout-ms",
        "0",
    ])
    .expect(1, "", "execution deadline exceeded\n");
    let looping = "def run(n: int) -> int\n  i = 0\n  while i < n\n    i += 1\n  end\n  i\nend\n";
    let run = vibes(&[
        "-e",
        looping,
        "--function",
        "run",
        "--arg",
        "100000",
        "--steps",
        "10000",
    ]);
    assert_eq!(run.status, Some(1));
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("step quota exceeded (10000)\n  --> line 3, column 3\n"),
        "{}",
        run.stderr
    );
    let run = vibes(&[
        "-e",
        "def f(n: int) -> int\n  f(n + 1)\nend\n",
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
        run.stderr
            .starts_with("recursion depth exceeded (limit 3)\n"),
        "{}",
        run.stderr
    );
    let run = vibes(&["-e", ADD, "--stats"]);
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "top\nnull\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
    let run = vibes(&["-e", ADD, "--function", "run", "--arg", "1", "--stats"]);
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(run.stdout, "ran\n2\n");
    assert_stats_line(run.stderr.trim_end_matches('\n'));
}
