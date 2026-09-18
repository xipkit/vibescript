//! Command-line parsing, checking and rendering for the `vibes` binary.
//!
//! The library owns every semantic decision. This module validates the whole
//! command line before reading the source, routes inputs through the public
//! call and check APIs, and renders reports with the input filename.

use std::{
    ffi::OsString,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};
use vibescript::{
    CallOptions, CheckDiagnostic, CheckReport, CheckedOutcome, Engine, Error, ErrorKind, Outcome,
    Script, Stats, Value, parse_json, stringify_json,
};

/// The usage text printed by `--help`.
pub const HELP: &str = "\
Usage: vibes [OPTIONS] FILE
       vibes [OPTIONS] --function NAME [--arg JSON]... [--kwarg NAME=JSON]... FILE
       vibes check [OPTIONS] [--function NAME] FILE

Runs the top-level statements of FILE, or calls one of its functions with JSON
arguments, and prints the final value as JSON on stdout. Script output from
puts, print and p goes to stdout; warn goes to stderr.

Options:
  --function NAME    Call NAME instead of running the top-level statements.
  --arg JSON         Append a positional argument. Requires --function.
  --kwarg NAME=JSON  Add a keyword argument. A repeated NAME binds its last
                     value; every value is still checked. Requires --function.
  --check            Analyze the call selected by --function, --arg and --kwarg
                     without executing any script or host code. Success prints
                     nothing. Known errors and incomplete analysis are printed
                     on stderr and exit with status 1. Requires --function.
  --checked          Run the same analysis, then execute the call only when it
                     is clean. A rejected call prints the report instead of a
                     result. Requires --function.
  --steps N          Step quota; 0 disables it (default 1000000).
  --memory N         Memory quota in bytes; 0 disables it (default 16777216).
  --recursion N      Maximum call depth (default 256).
  --timeout-ms N     Deadline in milliseconds, measured from option parsing.
  --stats            Print counters on stderr: analysis counters after --check
                     or a rejected --checked call, execution counters otherwise.
  --                 Treat the remaining argument as FILE.
  -h, --help         Print this help.
  --version          Print the version.

Commands:
  check FILE         Check the whole file without executing anything: the
                     top-level statements, then every function and method
                     declaration, including unused ones, for its declared
                     parameter types and defaults.
  check --function NAME FILE
                     Check one declaration the same way, without a concrete
                     call. NAME may be a function, Class#method,
                     Namespace.method or Class.new. See vibes check --help.

--check and --checked cover exactly one call: the named function with the
supplied values, and whatever that call reaches. They do not check unused
functions or the file as a whole. Selecting __main__ checks the top-level
statements; other named calls omit them. The check command covers the whole
file or one declaration for its declared parameter types rather than supplied
values, and is recognized only as the first argument.
No clean result proves that the script is type safe, and analysis that the
checker cannot finish is reported as incomplete rather than assumed clean.

Exit status: 0 on success or a clean check, 1 when reading, parsing, checking
or execution fails, 2 for usage errors.
";

/// The usage text printed by `vibes check --help`.
pub const CHECK_HELP: &str = "\
Usage: vibes check [OPTIONS] FILE
       vibes check [OPTIONS] --function NAME FILE

Checks FILE without executing any script or host code, defaults or
initializers, and prints no result value. Without --function, the whole file
is checked: the top-level statements in source order, then every function and
method declaration, including unused ones, for its declared parameter types
and defaults. With --function, only the selected declaration and whatever it
reaches are checked. Annotated parameters enter with their declared types,
unannotated parameters stay dynamic and optional defaults are analyzed; no
concrete call with supplied values is involved.

A clean check prints nothing and exits with status 0. Known errors and
analysis that the checker cannot finish are printed on stderr as separate
error and incomplete entries and exit with status 1; incomplete analysis is
never assumed clean. Required files that the checker cannot analyze are
reported as incomplete rather than loaded or executed.

Options:
  --function NAME    Check one declaration instead of the whole file: a
                     top-level function, Class#method for an instance method,
                     Namespace.method for a static or module method, or
                     Class.new for a constructor.
  --steps N          Analysis step quota; 0 disables it (default 1000000).
  --memory N         Analysis memory quota in bytes; 0 disables it
                     (default 16777216).
  --recursion N      Runtime call-depth setting (default 256). Checking uses
                     step and memory quotas for recursive call analysis.
  --timeout-ms N     Deadline in milliseconds, measured from option parsing.
  --stats            Print analysis counters on stderr.
  --                 Treat the remaining argument as FILE.
  -h, --help         Print this help.
  --version          Print the version.

vibes check takes no concrete arguments, so --arg, --kwarg, --check and
--checked are usage errors here. To check one call with supplied values use
vibes FILE --function NAME [--arg JSON]... --check, and to execute that call
only when its check is clean use --checked instead. The check command is
recognized only as the first argument; vibes -- check runs a file named check.

Exit status: 0 for a clean check, 1 when reading, parsing or analysis fails or
the report has errors or incomplete paths, 2 for usage errors.
";

/// The action selected by the command line.
#[derive(Debug)]
pub enum Command {
    /// Print the given usage text on stdout.
    Help(&'static str),
    /// Print the package version on stdout.
    Version,
    /// Read, compile and run or check one call or the top-level statements.
    Run(Box<Invocation>),
    /// Read, compile and check the whole file or one declaration.
    Check(Box<Analysis>),
}

/// How the selected call is treated after compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Execute the top-level statements or the named call without analysis.
    Execute,
    /// Analyze without executing and print only the report.
    Check,
    /// Analyze the named call, then execute it only when the report is clean.
    Checked,
}

impl Mode {
    fn flag(self) -> &'static str {
        match self {
            Self::Execute => "",
            Self::Check => "--check",
            Self::Checked => "--checked",
        }
    }
}

/// A validated command line whose JSON values and numbers are already parsed.
#[derive(Debug)]
pub struct Invocation {
    pub file: PathBuf,
    pub function: Option<String>,
    /// Positional arguments in command-line order.
    pub arguments: Vec<Value>,
    /// Keyword arguments in command-line order, including repeated names.
    pub keywords: Vec<(String, Value)>,
    pub options: CallOptions,
    pub mode: Mode,
    pub stats: bool,
}

/// A validated `vibes check` command line. It carries no concrete arguments.
#[derive(Debug)]
pub struct Analysis {
    pub file: PathBuf,
    /// The declaration selector, or `None` for the whole file.
    pub function: Option<String>,
    pub options: CallOptions,
    pub stats: bool,
}

/// What a report describes, for its summary line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope<'a> {
    /// One concrete call of the named function with the supplied values.
    Call(&'a str),
    /// One declaration checked for its declared parameter types and defaults.
    Declaration(&'a str),
    /// The top-level statements and every declaration in the file.
    File,
}

impl fmt::Display for Scope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Call(name) => write!(f, "check of {name}"),
            Self::Declaration(name) => {
                write!(f, "check of {name} for its declared parameter types")
            }
            Self::File => f.write_str("check of the whole file"),
        }
    }
}

/// A failure message and the exit status it maps to.
#[derive(Debug)]
pub enum Failure {
    /// The command line was malformed; nothing was read, checked or executed.
    Usage(String),
    /// Reading, compiling, checking or executing the script failed.
    Failed(String),
}

impl Failure {
    /// Returns the process exit status: 2 for usage errors and 1 otherwise.
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::Usage(_) => ExitCode::from(2),
            Self::Failed(_) => ExitCode::FAILURE,
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::Failed(message) => f.write_str(message),
        }
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

fn usage(message: impl Into<String>) -> Failure {
    Failure::Usage(message.into())
}

/// Parses the process arguments that follow the program name.
///
/// A first argument of `check` selects the check command; anywhere else,
/// `check` is an ordinary file name, and `--` always ends option parsing.
/// Options may appear in any order around FILE, and every option value is
/// consumed verbatim, so `--arg -1` is valid JSON. Positional and keyword
/// inputs keep their command-line order, including repeated keyword names:
/// the library checks every supplied value and binds the last one, exactly as
/// `Script::call_with_keywords` does. All JSON and numeric values, and every
/// option combination, are validated here, before any file is read.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, Failure> {
    let mut args = args.into_iter().peekable();
    let subcommand = args.next_if(|arg| arg.as_os_str() == "check").is_some();
    let help = if subcommand { CHECK_HELP } else { HELP };
    let mut file = None;
    let mut function = None;
    let mut arguments = Vec::new();
    let mut keywords = Vec::new();
    let mut options = CallOptions::default();
    let mut mode = Mode::Execute;
    let mut stats = false;
    let mut only_files = false;
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().into_owned();
        if only_files || !text.starts_with('-') {
            select_file(&mut file, arg.into())?;
            continue;
        }
        match text.as_str() {
            "--help" | "-h" => return Ok(Command::Help(help)),
            "--version" => return Ok(Command::Version),
            "--" => only_files = true,
            "--function" => function = Some(value(&mut args, "--function", "NAME")?),
            "--arg" | "--kwarg" | "--check" | "--checked" if subcommand => {
                return Err(usage(format!(
                    "vibes check does not accept {text}; it takes no concrete call. \
                     Use vibes FILE --function NAME [--arg JSON]... --check or --checked"
                )));
            }
            "--arg" => arguments.push(json("--arg", &value(&mut args, "--arg", "JSON")?)?),
            "--kwarg" => keywords.push(keyword(&value(&mut args, "--kwarg", "NAME=JSON")?)?),
            "--steps" => {
                let steps: u64 = number("--steps", &value(&mut args, "--steps", "N")?)?;
                options.limits.steps = (steps != 0).then_some(steps);
            }
            "--memory" => {
                let bytes: usize = number("--memory", &value(&mut args, "--memory", "N")?)?;
                options.limits.memory_bytes = (bytes != 0).then_some(bytes);
            }
            "--recursion" => {
                options.limits.recursion =
                    number("--recursion", &value(&mut args, "--recursion", "N")?)?;
            }
            "--timeout-ms" => {
                let millis: u64 = number("--timeout-ms", &value(&mut args, "--timeout-ms", "N")?)?;
                options.deadline = Some(
                    Instant::now()
                        .checked_add(Duration::from_millis(millis))
                        .ok_or_else(|| usage("timeout outside supported range"))?,
                );
            }
            "--stats" => stats = true,
            "--check" => mode = select_mode(mode, Mode::Check)?,
            "--checked" => mode = select_mode(mode, Mode::Checked)?,
            _ => return Err(usage(format!("unknown option {text}"))),
        }
    }
    let Some(file) = file else {
        return Err(usage(if subcommand {
            "expected source file; use vibes check --help"
        } else {
            "expected source file; use --help"
        }));
    };
    if subcommand {
        return Ok(Command::Check(Box::new(Analysis {
            file,
            function,
            options,
            stats,
        })));
    }
    if function.is_none() {
        if mode != Mode::Execute {
            return Err(usage(format!("{} requires --function", mode.flag())));
        }
        if !arguments.is_empty() {
            return Err(usage("--arg requires --function"));
        }
        if !keywords.is_empty() {
            return Err(usage("--kwarg requires --function"));
        }
    }
    Ok(Command::Run(Box::new(Invocation {
        file,
        function,
        arguments,
        keywords,
        options,
        mode,
        stats,
    })))
}

fn select_file(file: &mut Option<PathBuf>, path: PathBuf) -> Result<(), Failure> {
    if file.replace(path).is_some() {
        return Err(usage("expected one source file"));
    }
    Ok(())
}

fn select_mode(current: Mode, requested: Mode) -> Result<Mode, Failure> {
    if current == Mode::Execute || current == requested {
        return Ok(requested);
    }
    Err(usage("--check and --checked are mutually exclusive"))
}

fn value(
    args: &mut impl Iterator<Item = OsString>,
    option: &str,
    placeholder: &str,
) -> Result<String, Failure> {
    let Some(raw) = args.next() else {
        return Err(usage(format!("{option} requires {placeholder}")));
    };
    raw.into_string()
        .map_err(|_| usage(format!("{option} value is not valid UTF-8")))
}

fn number<T: std::str::FromStr>(option: &str, raw: &str) -> Result<T, Failure>
where
    T::Err: fmt::Display,
{
    raw.parse()
        .map_err(|error| usage(format!("invalid {option} value {raw:?}: {error}")))
}

fn json(option: &str, raw: &str) -> Result<Value, Failure> {
    parse_json(raw.as_bytes(), CallOptions::default())
        .map(|outcome| outcome.value)
        .map_err(|error| usage(format!("invalid JSON for {option}: {error}")))
}

fn keyword(raw: &str) -> Result<(String, Value), Failure> {
    let Some((name, text)) = raw.split_once('=') else {
        return Err(usage(format!("--kwarg requires NAME=JSON, got {raw:?}")));
    };
    if name.is_empty() {
        return Err(usage(format!(
            "--kwarg requires a nonempty NAME before '=', got {raw:?}"
        )));
    }
    Ok((name.to_owned(), json(&format!("--kwarg {name}"), text)?))
}

/// Reads and compiles the file, then executes or checks it as requested.
///
/// Output writers are attached before compilation so that `puts`, `print`,
/// `p` and `warn` reach the process streams during execution; analysis never
/// invokes them. Results are printed as JSON on stdout. Reports, counters and
/// errors go to stderr through the returned [`Failure`] or `--stats` line.
pub fn run(invocation: Invocation) -> Result<(), Failure> {
    let Invocation {
        file,
        function,
        arguments,
        keywords,
        options,
        mode,
        stats,
    } = invocation;
    let script = load(&file)?;
    let Some(name) = function else {
        return print_outcome(&script.run(options)?, stats);
    };
    let scope = Scope::Call(&name);
    match mode {
        Mode::Execute => print_outcome(
            &script.call_with_keywords(&name, &arguments, &keywords, options)?,
            stats,
        ),
        Mode::Check => {
            let report = script.check_call_with_keywords(&name, &arguments, &keywords, &options)?;
            accept(&file, scope, &report, mode, stats)
        }
        Mode::Checked => {
            match script.checked_call_with_keywords(&name, &arguments, &keywords, options)? {
                CheckedOutcome::Executed(outcome) => print_outcome(&outcome, stats),
                CheckedOutcome::Rejected(report) => {
                    Err(rejection(&file, scope, &report, mode, stats))
                }
            }
        }
    }
}

/// Reads and compiles the file, then checks the whole file or one declaration.
///
/// Nothing is executed: no top-level statement, default, initializer, host
/// callback or output writer runs, and no value is printed. A clean report
/// prints nothing; otherwise the report is returned as a [`Failure`]. The
/// `--stats` line always shows analysis counters.
pub fn check(analysis: Analysis) -> Result<(), Failure> {
    let Analysis {
        file,
        function,
        options,
        stats,
    } = analysis;
    let script = load(&file)?;
    let (report, scope) = match &function {
        None => (script.check(&options)?, Scope::File),
        Some(name) => (
            script.check_function(name, &options)?,
            Scope::Declaration(name),
        ),
    };
    accept(&file, scope, &report, Mode::Check, stats)
}

/// Reads and compiles one source file with the process streams attached.
fn load(file: &Path) -> Result<Script, Failure> {
    let source = fs::read_to_string(file)
        .map_err(|error| Failure::Failed(format!("cannot read {}: {error}", file.display())))?;
    let mut engine = Engine::new();
    engine.set_output_writer(|_, bytes| forward(io::stdout().lock(), bytes));
    engine.set_error_writer(|_, bytes| forward(io::stderr().lock(), bytes));
    engine
        .compile(&source)
        .map_err(|error| Failure::Failed(compile_failure(file, &error)))
}

fn forward(mut stream: impl Write, bytes: &[u8]) -> vibescript::Result<()> {
    stream
        .write_all(bytes)
        .map_err(|error| Error::new(ErrorKind::Host, error.to_string()))
}

fn print_outcome(outcome: &Outcome, stats: bool) -> Result<(), Failure> {
    let encoded = stringify_json(&outcome.value, CallOptions::default())?;
    let mut out = io::stdout().lock();
    out.write_all(
        encoded
            .value
            .as_bytes()
            .expect("stringify_json returns a string value"),
    )?;
    out.write_all(b"\n")?;
    if stats {
        eprintln!("{}", stats_line(&outcome.stats));
    }
    Ok(())
}

fn stats_line(stats: &Stats) -> String {
    format!(
        "steps={} peak_bytes={} retained_bytes={}",
        stats.steps, stats.peak_memory_bytes, stats.retained_memory_bytes
    )
}

/// Finishes an analysis-only mode: a clean report prints at most its counters.
fn accept(
    file: &Path,
    scope: Scope<'_>,
    report: &CheckReport,
    mode: Mode,
    stats: bool,
) -> Result<(), Failure> {
    if !report.is_clean() {
        return Err(rejection(file, scope, report, mode, stats));
    }
    if stats {
        eprintln!("{}", stats_line(&report.stats));
    }
    Ok(())
}

fn rejection(
    file: &Path,
    scope: Scope<'_>,
    report: &CheckReport,
    mode: Mode,
    stats: bool,
) -> Failure {
    let mut text = render_report(file, scope, report, mode);
    if stats {
        text.push('\n');
        text.push_str(&stats_line(&report.stats));
    }
    Failure::Failed(text)
}

/// Renders a rejected report followed by a one-line summary.
///
/// Known contradictions are rendered as `error` entries and unfinished
/// analysis as `incomplete` entries, in the report's source order. Each entry
/// names the input file (or the required module that owns the diagnostic),
/// the one-based line and column, the containing function, the message and
/// the library's code frame. The summary names the checked [`Scope`], counts
/// both kinds separately and, in [`Mode::Checked`], states that nothing was
/// executed.
pub fn render_report(file: &Path, scope: Scope<'_>, report: &CheckReport, mode: Mode) -> String {
    let mut text = String::new();
    for diagnostic in &report.diagnostics {
        entry(&mut text, file, "error", diagnostic);
    }
    for diagnostic in &report.incomplete {
        entry(&mut text, file, "incomplete", diagnostic);
    }
    text.push_str(&format!("{}: {scope} found ", file.display()));
    let errors = report.diagnostics.len();
    let incomplete = report.incomplete.len();
    if errors > 0 {
        text.push_str(&count(errors, "error", "errors"));
    }
    if errors > 0 && incomplete > 0 {
        text.push_str(" and ");
    }
    if incomplete > 0 {
        text.push_str(&count(incomplete, "incomplete path", "incomplete paths"));
    }
    if mode == Mode::Checked {
        text.push_str("; nothing was executed");
    }
    text
}

fn entry(text: &mut String, file: &Path, kind: &str, diagnostic: &CheckDiagnostic) {
    match &diagnostic.filename {
        Some(name) => text.push_str(&module_filename(name)),
        None => text.push_str(&file.display().to_string()),
    }
    text.push_str(&format!(
        ":{}:{}: {kind} in {}: {}\n{}\n",
        diagnostic.position.line,
        diagnostic.position.column,
        diagnostic.function,
        diagnostic.message,
        diagnostic.code_frame
    ));
}

/// Renders module filenames with control-character escapes and lossy UTF-8 replacement.
fn module_filename(name: &[u8]) -> String {
    let mut text = String::new();
    for ch in String::from_utf8_lossy(name).chars() {
        if ch.is_control() || ch == '\\' {
            text.extend(ch.escape_default());
        } else {
            text.push(ch);
        }
    }
    text
}

fn count(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// Renders a compile failure with the input filename. Located parse errors use
/// the library's position and code frame; other failures keep their display.
fn compile_failure(file: &Path, error: &Error) -> String {
    match &error.diagnostic {
        Some(diagnostic) if error.kind == ErrorKind::Syntax => format!(
            "{}:{}:{}: parse error: {}\n{}",
            file.display(),
            diagnostic.position.line,
            diagnostic.position.column,
            error.message,
            diagnostic.code_frame
        ),
        _ => format!("{}: {error}", file.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Command, Failure> {
        parse(args.iter().map(OsString::from))
    }

    fn invocation(args: &[&str]) -> Invocation {
        match parsed(args) {
            Ok(Command::Run(invocation)) => *invocation,
            other => panic!("{other:?}"),
        }
    }

    fn analysis(args: &[&str]) -> Analysis {
        match parsed(args) {
            Ok(Command::Check(analysis)) => *analysis,
            other => panic!("{other:?}"),
        }
    }

    fn usage_error(args: &[&str]) -> String {
        match parsed(args) {
            Err(Failure::Usage(message)) => message,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inputs_keep_command_line_order_including_repeated_keywords() {
        let invocation = invocation(&[
            "--kwarg",
            "b=3",
            "--arg",
            "7",
            "f.vibe",
            "--function",
            "run",
            "--kwarg",
            "b=5",
            "--arg",
            "-1",
            "--kwarg",
            "x=9",
            "--checked",
            "--stats",
        ]);
        assert_eq!(invocation.file, PathBuf::from("f.vibe"));
        assert_eq!(invocation.function.as_deref(), Some("run"));
        let arguments: Vec<_> = invocation.arguments.iter().map(Value::as_int).collect();
        assert_eq!(arguments, [Some(7), Some(-1)]);
        let keywords: Vec<_> = invocation
            .keywords
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_int()))
            .collect();
        assert_eq!(keywords, [("b", Some(3)), ("b", Some(5)), ("x", Some(9))]);
        assert_eq!(invocation.mode, Mode::Checked);
        assert!(invocation.stats);
        assert_eq!(invocation.options.limits.steps, Some(1_000_000));
        assert_eq!(invocation.options.limits.memory_bytes, Some(16 << 20));
        assert_eq!(invocation.options.limits.recursion, 256);
    }

    #[test]
    fn quotas_and_terminator_follow_the_documented_rules() {
        let invocation = invocation(&[
            "--steps",
            "0",
            "--memory",
            "0",
            "--recursion",
            "3",
            "--",
            "-x",
        ]);
        assert_eq!(invocation.file, PathBuf::from("-x"));
        assert_eq!(invocation.options.limits.steps, None);
        assert_eq!(invocation.options.limits.memory_bytes, None);
        assert_eq!(invocation.options.limits.recursion, 3);
        assert_eq!(invocation.mode, Mode::Execute);
        assert_eq!(usage_error(&["-x"]), "unknown option -x");
        assert_eq!(usage_error(&["--", "a", "b"]), "expected one source file");
        assert!(matches!(
            parsed(&["--bogus", "--help"]),
            Err(Failure::Usage(_))
        ));
        assert!(matches!(
            parsed(&["--help", "--bogus"]),
            Ok(Command::Help(HELP))
        ));
    }

    #[test]
    fn malformed_inputs_are_rejected_before_any_file_is_read() {
        for (args, message) in [
            (&[][..], "expected source file; use --help"),
            (&["f", "--function"], "--function requires NAME"),
            (&["f", "--check"], "--check requires --function"),
            (&["f", "--checked"], "--checked requires --function"),
            (&["f", "--arg", "1"], "--arg requires --function"),
            (&["f", "--kwarg", "x=1"], "--kwarg requires --function"),
            (
                &["f", "--function", "run", "--check", "--checked"],
                "--check and --checked are mutually exclusive",
            ),
            (
                &["f", "--kwarg", "x"],
                "--kwarg requires NAME=JSON, got \"x\"",
            ),
            (
                &["f", "--kwarg", "=1"],
                "--kwarg requires a nonempty NAME before '=', got \"=1\"",
            ),
        ] {
            assert_eq!(usage_error(args), message, "{args:?}");
        }
        assert!(usage_error(&["f", "--arg", "{"]).starts_with("invalid JSON for --arg: "));
        assert!(usage_error(&["f", "--kwarg", "k={"]).starts_with("invalid JSON for --kwarg k: "));
        assert!(usage_error(&["f", "--steps", "x"]).starts_with("invalid --steps value \"x\": "));
        assert!(matches!(
            parsed(&["f", "--function", "run", "--check", "--check"]),
            Ok(Command::Run(_))
        ));
    }

    #[test]
    fn check_command_selects_scopes_only_as_the_first_argument() {
        let whole = analysis(&["check", "--steps", "0", "--stats", "--", "-x"]);
        assert_eq!(whole.file, PathBuf::from("-x"));
        assert_eq!(whole.function, None);
        assert_eq!(whole.options.limits.steps, None);
        assert_eq!(whole.options.limits.memory_bytes, Some(16 << 20));
        assert_eq!(whole.options.limits.recursion, 256);
        assert!(whole.stats);
        let declaration = analysis(&["check", "--function", "C#read", "check", "--recursion", "3"]);
        assert_eq!(declaration.file, PathBuf::from("check"));
        assert_eq!(declaration.function.as_deref(), Some("C#read"));
        assert_eq!(declaration.options.limits.recursion, 3);
        assert!(!declaration.stats);
        let literal = invocation(&["--", "check"]);
        assert_eq!(literal.file, PathBuf::from("check"));
        assert_eq!(literal.mode, Mode::Execute);
        let literal = invocation(&["--stats", "check"]);
        assert_eq!(literal.file, PathBuf::from("check"));
        assert!(literal.stats);
        assert_eq!(usage_error(&["f", "check"]), "expected one source file");
        assert!(matches!(
            parsed(&["check", "--help", "--bogus"]),
            Ok(Command::Help(CHECK_HELP))
        ));
        assert!(matches!(
            parsed(&["check", "-h"]),
            Ok(Command::Help(CHECK_HELP))
        ));
        assert!(matches!(
            parsed(&["check", "--version"]),
            Ok(Command::Version)
        ));
    }

    #[test]
    fn check_command_rejects_concrete_call_options_before_any_file_is_read() {
        for option in ["--arg", "--kwarg", "--check", "--checked"] {
            let message = usage_error(&["check", "f", option, "1"]);
            assert!(
                message.starts_with(&format!("vibes check does not accept {option}; ")),
                "{message}"
            );
            assert_eq!(usage_error(&["check", option]), message);
        }
        for (args, message) in [
            (
                &["check"][..],
                "expected source file; use vibes check --help",
            ),
            (&["check", "--function"], "--function requires NAME"),
            (&["check", "a", "b"], "expected one source file"),
            (&["check", "f", "--bogus"], "unknown option --bogus"),
        ] {
            assert_eq!(usage_error(args), message, "{args:?}");
        }
        assert!(
            usage_error(&["check", "f", "--memory", "-1"])
                .starts_with("invalid --memory value \"-1\": ")
        );
    }

    #[test]
    fn reports_render_errors_and_incomplete_paths_with_the_input_filename() {
        let script = Engine::new()
            .compile("def run() -> int\n  \"é\"\nend")
            .unwrap();
        let report = script
            .check_call("run", &[], &CallOptions::default())
            .unwrap();
        let text = render_report(
            Path::new("dir/x.vibe"),
            Scope::Call("run"),
            &report,
            Mode::Checked,
        );
        assert_eq!(
            text,
            "dir/x.vibe:2:3: error in run: Return value: expected int, got string\n  \
             --> line 2, column 3\n 2 |   \"é\"\n   |   ^\n\
             dir/x.vibe: check of run found 1 error; nothing was executed"
        );
        let report = script.check(&CallOptions::default()).unwrap();
        let text = render_report(Path::new("x.vibe"), Scope::File, &report, Mode::Check);
        assert!(
            text.ends_with("\nx.vibe: check of the whole file found 1 error"),
            "{text}"
        );
        let report = script
            .check_function("run", &CallOptions::default())
            .unwrap();
        let text = render_report(
            Path::new("x.vibe"),
            Scope::Declaration("run"),
            &report,
            Mode::Check,
        );
        assert!(
            text.ends_with("\nx.vibe: check of run for its declared parameter types found 1 error"),
            "{text}"
        );
        assert_eq!(count(2, "error", "errors"), "2 errors");
        assert_eq!(
            module_filename(b"pkg/a\n\\\xff.vibe"),
            "pkg/a\\n\\\\\u{fffd}.vibe"
        );
    }
}
