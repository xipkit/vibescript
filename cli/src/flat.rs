//! The flat form: `vibes [OPTIONS] FILE` and `vibes [OPTIONS] -e SOURCE`.
//!
//! This is the command line that predates the Go-compatible commands. It
//! prints results as JSON and takes JSON call arguments. The dispatcher
//! selects it when the first argument is one of its options or names a
//! script file; see `vibes help flat`.
//!
//! The library owns every semantic decision. This module validates the whole
//! command line before reading the source, routes inputs through the public
//! call API, and renders failures with the input filename or the `<eval>`
//! label of inline source.

use std::{
    borrow::Cow,
    ffi::OsString,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};
use vibescript::{
    CallOptions, Engine, Error, ErrorKind, ModuleConfig, Outcome, Script, Stats, Value, parse_json,
    stringify_json,
};

/// The usage text printed by `vibes help flat` and by `--help` in the flat form.
pub const HELP: &str = "\
Usage: vibes [OPTIONS] FILE
       vibes [OPTIONS] -e SOURCE
       vibes [OPTIONS] --function NAME [--arg JSON]... [--kwarg NAME=JSON]... FILE

The flat form type checks FILE or the inline SOURCE, runs its top-level
statements or calls one of its functions with JSON arguments, and prints the
final value as JSON on stdout. Script output from puts, print and p goes to
stdout; warn goes to stderr. It applies when the first argument is one of the
options below or names a script file: an existing file, or a path containing a
separator or ending in .vibe. A first argument that names a command, such as
run or check, always selects that command instead.

Options:
  -e, --eval SOURCE  Use the inline SOURCE instead of FILE, exactly once and
                     never together with FILE. Reports name it <eval>, and
                     require searches the working directory first.
  --function NAME    Call NAME instead of running the top-level statements.
  --module-path DIR  Add a module search directory (repeatable). The input
                     file's directory, or the working directory for -e, is
                     searched first.
  --arg JSON         Append a positional argument. Requires --function.
  --kwarg NAME=JSON  Add a keyword argument. A repeated NAME binds its last
                     value; every value is still checked. Requires --function.
  --steps N          Step quota; 0 disables it (default 1000000).
  --memory N         Memory quota in bytes; 0 disables it (default 16777216).
  --recursion N      Maximum call depth (default 256).
  --timeout-ms N     Deadline in milliseconds, measured from option parsing.
  --stats            Print execution counters on stderr.
  --                 Treat the remaining argument as FILE.
  -h, --help         Print this help.

Options may appear anywhere around FILE and their values are taken verbatim.
A source with type errors does not run; its diagnostics are printed on stderr.
Use vibes check to report them without running anything.

Exit status: 0 on success, 1 when reading, compiling or execution fails, 2 for
usage errors.
";

/// The name reports use for inline source supplied by `-e` or `--eval`.
pub const EVAL_LABEL: &str = "<eval>";

/// The action selected by the command line.
#[derive(Debug)]
pub enum Command {
    /// Print the flat form's usage text on stdout.
    Help,
    /// Read, compile and run one call or the top-level statements.
    Run(Box<Invocation>),
}

/// Where the main source comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Input {
    /// A source file, read only after the whole command line is validated.
    File(PathBuf),
    /// Inline source from `-e` or `--eval`, reported as [`EVAL_LABEL`].
    Inline(String),
}

impl Input {
    /// The name that reports and failures use for this input.
    pub fn label(&self) -> Cow<'_, str> {
        match self {
            Self::File(file) => Cow::Owned(file.display().to_string()),
            Self::Inline(_) => Cow::Borrowed(EVAL_LABEL),
        }
    }

    /// The first module root: the file's directory, or the working directory
    /// for inline source.
    fn root(&self) -> &Path {
        match self {
            Self::File(file) => file
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
            Self::Inline(_) => Path::new("."),
        }
    }
}

/// A validated command line whose JSON values and numbers are already parsed.
#[derive(Debug)]
pub struct Invocation {
    pub input: Input,
    /// Additional module roots in command-line order, after the input root.
    pub module_paths: Vec<PathBuf>,
    pub function: Option<String>,
    /// Positional arguments in command-line order.
    pub arguments: Vec<Value>,
    /// Keyword arguments in command-line order, including repeated names.
    pub keywords: Vec<(String, Value)>,
    pub options: CallOptions,
    pub stats: bool,
}

/// A failure message and the exit status it maps to.
#[derive(Debug)]
pub enum Failure {
    /// The command line was malformed; nothing was read or executed.
    Usage(String),
    /// Reading, compiling or executing the script failed.
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

/// Parses the flat form's arguments, starting with the first one.
///
/// `--` ends option parsing. Options may appear in any order around FILE, and
/// every option value is consumed verbatim, so `--arg -1` is valid JSON and
/// `-e -7` is a snippet. The source is either FILE or one `-e`/`--eval` SOURCE,
/// never both. Positional and keyword inputs keep their command-line order,
/// including repeated keyword names: the library checks every supplied value
/// and binds the last one, exactly as `Script::call_with_keywords` does. All
/// JSON and numeric values, and every option combination, are validated here,
/// before any file or module directory is read.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, Failure> {
    let mut args = args.into_iter().peekable();
    let mut input = None;
    let mut module_paths = Vec::new();
    let mut function = None;
    let mut arguments = Vec::new();
    let mut keywords = Vec::new();
    let mut options = CallOptions::default();
    let mut stats = false;
    let mut only_files = false;
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy().into_owned();
        if only_files || !text.starts_with('-') {
            select_input(&mut input, Input::File(arg.into()))?;
            continue;
        }
        match text.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "--" => only_files = true,
            "-e" | "--eval" => {
                select_input(
                    &mut input,
                    Input::Inline(value(&mut args, &text, "SOURCE")?),
                )?;
            }
            "--function" => function = Some(value(&mut args, "--function", "NAME")?),
            "--module-path" => module_paths.push(
                args.next()
                    .map(PathBuf::from)
                    .ok_or_else(|| usage("--module-path requires DIR"))?,
            ),
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
            _ => return Err(usage(format!("unknown option {text}"))),
        }
    }
    let Some(input) = input else {
        return Err(usage(
            "expected source file or -e SOURCE; use vibes help flat",
        ));
    };
    if function.is_none() {
        if !arguments.is_empty() {
            return Err(usage("--arg requires --function"));
        }
        if !keywords.is_empty() {
            return Err(usage("--kwarg requires --function"));
        }
    }
    Ok(Command::Run(Box::new(Invocation {
        input,
        module_paths,
        function,
        arguments,
        keywords,
        options,
        stats,
    })))
}

fn select_input(current: &mut Option<Input>, requested: Input) -> Result<(), Failure> {
    match (current.as_ref(), &requested) {
        (None, _) => {}
        (Some(Input::File(_)), Input::File(_)) => return Err(usage("expected one source file")),
        (Some(Input::Inline(_)), Input::Inline(_)) => {
            return Err(usage(
                "expected one inline source; -e or --eval was given twice",
            ));
        }
        (Some(_), _) => return Err(usage("expected FILE or -e SOURCE, not both")),
    }
    *current = Some(requested);
    Ok(())
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

/// Compiles the input, then executes the named call or the top-level
/// statements.
///
/// Output writers are attached before compilation so that `puts`, `print`,
/// `p` and `warn` reach the process streams during execution. Results are
/// printed as JSON on stdout. Diagnostics, counters and errors go to stderr
/// through the returned [`Failure`] or `--stats` line.
pub fn run(invocation: Invocation) -> Result<(), Failure> {
    let Invocation {
        input,
        module_paths,
        function,
        arguments,
        keywords,
        options,
        stats,
    } = invocation;
    let options = CallOptions {
        cancellation: crate::signal::token(),
        ..options
    };
    let script = load(&input, &module_paths)?;
    let outcome = match function {
        Some(name) => script.call_with_keywords(&name, &arguments, &keywords, options)?,
        None => script.run(options)?,
    };
    print_outcome(&outcome, stats)
}

/// Reads a file or takes the inline source, and compiles it with the process
/// streams attached. Inline source never touches the file system except
/// through the configured module roots.
fn load(input: &Input, extra_paths: &[PathBuf]) -> Result<Script, Failure> {
    let source = match input {
        Input::File(file) => Cow::Owned(fs::read_to_string(file).map_err(|error| {
            Failure::Failed(format!("cannot read {}: {error}", file.display()))
        })?),
        Input::Inline(source) => Cow::Borrowed(source.as_str()),
    };
    let mut engine = Engine::new();
    engine.set_module_config(ModuleConfig {
        paths: module_paths(implicit_root(input), extra_paths)?,
        ..ModuleConfig::default()
    })?;
    engine.set_output_writer(|_, bytes| forward(io::stdout().lock(), bytes));
    engine.set_error_writer(|_, bytes| forward(io::stderr().lock(), bytes));
    engine
        .compile(&source)
        .map_err(|error| Failure::Failed(compile_failure(&input.label(), &source, &error)))
}

/// The input's own module root. A WASI guest has a working directory only when
/// the host exposes one, so inline source there may run without it.
fn implicit_root(input: &Input) -> Option<&Path> {
    let root = input.root();
    if cfg!(target_os = "wasi") && matches!(input, Input::Inline(_)) && !root.is_dir() {
        return None;
    }
    Some(root)
}

fn module_paths(root: Option<&Path>, extras: &[PathBuf]) -> Result<Vec<PathBuf>, Failure> {
    let mut paths = Vec::new();
    for path in root.into_iter().chain(extras.iter().map(PathBuf::as_path)) {
        let canonical = canonical(path).map_err(|error| {
            Failure::Failed(format!(
                "cannot open module path {}: {error}",
                path.display()
            ))
        })?;
        if !canonical.is_dir() {
            return Err(Failure::Failed(format!(
                "module path {} is not a directory",
                path.display()
            )));
        }
        if !paths.contains(&canonical) {
            paths.push(canonical);
        }
    }
    Ok(paths)
}

#[cfg(not(target_os = "wasi"))]
fn canonical(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

/// WASI's `realpath` fails beneath a preopen whose guest ancestors the host
/// does not expose. The library resolves links when it opens each root, so
/// duplicates are only collapsed by their absolute spelling here.
#[cfg(target_os = "wasi")]
fn canonical(path: &Path) -> io::Result<PathBuf> {
    std::path::absolute(path)
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

/// Renders a compile failure with the input label. Located parse errors use
/// the library's position and code frame, type errors list every static
/// diagnostic, and other failures keep their display.
fn compile_failure(label: &str, source: &str, error: &Error) -> String {
    match &error.diagnostic {
        Some(diagnostic) if error.kind == ErrorKind::Syntax => format!(
            "{label}:{}:{}: parse error: {}\n{}",
            diagnostic.position.line,
            diagnostic.position.column,
            error.message,
            diagnostic.code_frame
        ),
        _ if !error.diagnostics().is_empty() => {
            crate::run::compile_failure(error, source, label, None)
        }
        _ => format!("{label}: {error}"),
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

    fn usage_error(args: &[&str]) -> String {
        match parsed(args) {
            Err(Failure::Usage(message)) => message,
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn module_path_options_preserve_non_utf8_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let root = OsString::from_vec(b"modules-\xff".to_vec());
        let args = [
            OsString::from("main.vibe"),
            OsString::from("--module-path"),
            root.clone(),
            OsString::from("--module-path"),
            OsString::from("fallback"),
        ];
        let paths = match parse(args).unwrap() {
            Command::Run(invocation) => invocation.module_paths,
            other => panic!("{other:?}"),
        };
        assert_eq!(paths, [PathBuf::from(root), PathBuf::from("fallback")]);
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
            "--stats",
        ]);
        assert_eq!(invocation.input, Input::File(PathBuf::from("f.vibe")));
        assert_eq!(invocation.function.as_deref(), Some("run"));
        let arguments: Vec<_> = invocation.arguments.iter().map(Value::as_int).collect();
        assert_eq!(arguments, [Some(7), Some(-1)]);
        let keywords: Vec<_> = invocation
            .keywords
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_int()))
            .collect();
        assert_eq!(keywords, [("b", Some(3)), ("b", Some(5)), ("x", Some(9))]);
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
        assert_eq!(invocation.input, Input::File(PathBuf::from("-x")));
        assert_eq!(invocation.options.limits.steps, None);
        assert_eq!(invocation.options.limits.memory_bytes, None);
        assert_eq!(invocation.options.limits.recursion, 3);
        assert_eq!(usage_error(&["f", "-x"]), "unknown option -x");
        // The gradual checker's modes are gone from the command line.
        assert_eq!(usage_error(&["f", "--check"]), "unknown option --check");
        assert_eq!(usage_error(&["f", "--checked"]), "unknown option --checked");
        assert_eq!(usage_error(&["--", "a", "b"]), "expected one source file");
        assert!(matches!(
            parsed(&["--bogus", "--help"]),
            Err(Failure::Usage(_))
        ));
        assert!(matches!(parsed(&["--help", "--bogus"]), Ok(Command::Help)));
        assert_eq!(
            invocation_input(&["--stats", "check"]),
            Input::File(PathBuf::from("check"))
        );
    }

    fn invocation_input(args: &[&str]) -> Input {
        invocation(args).input
    }

    #[test]
    fn malformed_inputs_are_rejected_before_any_file_is_read() {
        for (args, message) in [
            (
                &[][..],
                "expected source file or -e SOURCE; use vibes help flat",
            ),
            (&["f", "--function"], "--function requires NAME"),
            (&["f", "--arg", "1"], "--arg requires --function"),
            (&["f", "--kwarg", "x=1"], "--kwarg requires --function"),
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
    }

    #[test]
    fn inline_source_replaces_the_file() {
        let inline = invocation(&["-e", "-7", "--stats"]);
        assert_eq!(inline.input, Input::Inline("-7".to_owned()));
        assert!(inline.stats);
        let inline = invocation(&["--function", "run", "--eval", "--stats"]);
        assert_eq!(inline.input, Input::Inline("--stats".to_owned()));
        assert!(!inline.stats);
        let whole = invocation(&["-e", ""]);
        assert_eq!(whole.input, Input::Inline(String::new()));
        assert_eq!(whole.function, None);
        assert_eq!(
            invocation(&["--stats", "--", "-e"]).input,
            Input::File(PathBuf::from("-e"))
        );
        assert_eq!(Input::Inline("7".to_owned()).label(), EVAL_LABEL);
        assert_eq!(Input::Inline("7".to_owned()).root(), Path::new("."));
        assert_eq!(Input::File(PathBuf::from("f")).root(), Path::new("."));
        assert_eq!(Input::File(PathBuf::from("dir/f")).root(), Path::new("dir"));
    }

    #[test]
    fn inline_source_combinations_are_rejected_before_any_file_is_read() {
        for (args, message) in [
            (&["-e"][..], "-e requires SOURCE"),
            (&["--eval"], "--eval requires SOURCE"),
            (
                &["-e", "1", "-e", "2"],
                "expected one inline source; -e or --eval was given twice",
            ),
            (&["f", "-e", "1"], "expected FILE or -e SOURCE, not both"),
            (&["-e", "1", "f"], "expected FILE or -e SOURCE, not both"),
            (
                &["-e", "1", "--", "-e"],
                "expected FILE or -e SOURCE, not both",
            ),
            (&["-e", "1", "--arg", "1"], "--arg requires --function"),
            (
                &["-e", "1", "--kwarg", "x=1"],
                "--kwarg requires --function",
            ),
        ] {
            assert_eq!(usage_error(args), message, "{args:?}");
        }
        assert!(matches!(parsed(&["-e", "1", "--help"]), Ok(Command::Help)));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let args = [
                OsString::from("-e"),
                OsString::from_vec(b"puts 1\xff".to_vec()),
            ];
            match parse(args) {
                Err(Failure::Usage(message)) => {
                    assert_eq!(message, "-e value is not valid UTF-8");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn type_errors_list_every_diagnostic_with_the_input_label() {
        let source = "def run -> int\n  \"é\"\nend\n";
        let error = Engine::new().compile(source).err().unwrap();
        let text = compile_failure("dir/x.vibe", source, &error);
        assert!(
            text.starts_with("compile failed with 1 diagnostic(s)\ndir/x.vibe:2:3: error[V0101]: "),
            "{text}"
        );
    }
}
