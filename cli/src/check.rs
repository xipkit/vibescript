//! `vibes check` and the `run -check` report formats.
//!
//! Issues print in the reference's `path:line:column: message (function)`
//! form. The library reports known errors and incomplete analysis separately;
//! incomplete entries are issues too, marked `incomplete:`, since a check
//! that could not finish is never clean.

use crate::{
    compat,
    flags::{self, Flag, Kind, Outcome, Spec},
    output::Sink,
    render, run, source,
};
use std::{
    ffi::OsString,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use vibescript::{CallOptions, CheckDiagnostic, CheckReport, Script, Stats, Value};

const FLAGS: [Flag; 9] = [
    Flag::new(
        &["module-path"],
        Kind::Strings,
        "add a module search directory (repeatable)",
    ),
    Flag::new(
        &["function"],
        Kind::String,
        "check one declaration instead: a function, Class#method, Namespace.method or Class.new",
    ),
    Flag::new(
        &["eval", "e"],
        Kind::String,
        "check an inline snippet instead of a script file",
    ),
    Flag::new(
        &["steps"],
        Kind::Uint,
        "analysis step quota; 0 disables it (default unlimited)",
    ),
    Flag::new(
        &["memory"],
        Kind::Uint,
        "analysis memory quota in bytes; 0 disables it (default unlimited)",
    ),
    Flag::new(
        &["recursion"],
        Kind::Uint,
        "call-depth setting (default 256)",
    ),
    Flag::new(
        &["timeout-ms"],
        Kind::Uint,
        "analysis deadline in milliseconds",
    ),
    Flag::new(&["stats"], Kind::Bool, "print analysis counters on stderr"),
    Flag::new(
        &["static"],
        Kind::Bool,
        "type check statically (ADR-007) instead of running the gradual checker",
    ),
];

pub const SPEC: Spec = Spec {
    name: "check",
    aliases: &[],
    usage: "statically check a script without executing it",
    arguments: "<script>",
    usage_lines: &[],
    flags: &FLAGS,
};

/// The label inline source carries in reports.
const EVAL_LABEL: &str = "<eval>";

/// One reported issue.
struct Issue {
    /// A required module's file, or `None` for the checked input itself.
    module: Option<Vec<u8>>,
    line: usize,
    column: usize,
    message: String,
    function: String,
}

/// Runs `vibes check` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    // Like the reference's check, analysis has no step or memory quota unless
    // one is requested. Its cost grows linearly with the source.
    let mut options = CallOptions::default();
    options.limits.steps = None;
    options.limits.memory_bytes = None;
    if let Some(steps) = flags.uint("steps") {
        options.limits.steps = (steps != 0).then_some(steps);
    }
    if let Some(memory) = flags.uint("memory") {
        options.limits.memory_bytes =
            (memory != 0).then(|| usize::try_from(memory).unwrap_or(usize::MAX));
    }
    if let Some(recursion) = flags.uint("recursion") {
        options.limits.recursion = usize::try_from(recursion).unwrap_or(usize::MAX);
    }
    if let Some(millis) = flags.uint("timeout-ms") {
        options.deadline = Some(
            Instant::now()
                .checked_add(Duration::from_millis(millis))
                .ok_or("vibes check: timeout outside supported range")?,
        );
    }
    let module_paths = flags.strings("module-path");
    let (label, directory, text) = match flags.value("eval") {
        Some(snippet) => {
            if !flags.positionals.is_empty() {
                return Err("vibes check: -e does not accept positional arguments".to_owned());
            }
            let directory = compat::working_directory().map_err(|error| {
                format!("resolve working directory: {}", compat::reason(&error))
            })?;
            let snippet = String::from_utf8_lossy(&compat::bytes(snippet)).into_owned();
            (EVAL_LABEL.to_owned(), directory, Ok(snippet))
        }
        None => {
            let script = match flags.positionals.as_slice() {
                [] => return Err("vibes check: script path required".to_owned()),
                [script] => script,
                _ => return Err("vibes check: expected a single script path".to_owned()),
            };
            let script = compat::absolute(Path::new(script))
                .map_err(|error| format!("resolve script path: {}", compat::reason(&error)))?;
            let directory = script.parent().unwrap_or(Path::new("/")).to_owned();
            (script.display().to_string(), directory, Err(script))
        }
    };
    let module_dirs = source::module_paths(&directory, &module_paths)
        .map_err(|error| format!("compute module paths: {error}"))?;
    let mut engine = run::engine(&module_dirs, &Sink::Stdout, &Sink::Stderr)?;
    let (text, snippet) = match text {
        Ok(snippet) => (snippet, true),
        Err(script) => (
            source::read(&script).map_err(|error| format!("read script: {error}"))?,
            false,
        ),
    };
    if flags.bool("static") {
        engine.set_static_types(true);
        return static_check(&engine, &text, &label, snippet);
    }
    let script = engine.compile(&text).map_err(|error| {
        format!(
            "compile failed: {}",
            render::error(&error, snippet.then_some(text.as_str()))
        )
    })?;
    let report = match flags.string("function") {
        Some(name) => script.check_function(&name, &options),
        None => script.check(&options),
    }
    .map_err(|error| error.to_string())?;
    let issues = issues(&report);
    let mut out = io::stdout().lock();
    let written = if issues.is_empty() {
        writeln!(out, "No issues found")
    } else {
        issues.iter().try_for_each(|issue| {
            let path = match &issue.module {
                Some(module) => module_path(module, &module_dirs),
                None => label.clone(),
            };
            writeln!(out, "{path}:{}", location(issue))
        })
    };
    written
        .and_then(|()| out.flush())
        .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    if flags.bool("stats") {
        eprintln!("{}", stats_line(&report.stats));
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!("check failed with {} issue(s)", issues.len()))
    }
}

/// `vibes check --static`: compiles with the static checker and prints every
/// diagnostic, or `No issues found`.
fn static_check(
    engine: &vibescript::Engine,
    text: &str,
    label: &str,
    snippet: bool,
) -> Result<(), String> {
    let error = match engine.compile(text) {
        Ok(_) => {
            println!("No issues found");
            return Ok(());
        }
        Err(error) => error,
    };
    if error.diagnostics().is_empty() {
        return Err(format!(
            "compile failed: {}",
            render::error(&error, snippet.then_some(text))
        ));
    }
    let mut out = io::stdout().lock();
    for diagnostic in error.diagnostics() {
        write!(out, "{}", render::diagnostic(diagnostic, text, label))
            .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    }
    out.flush()
        .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    let errors = error.diagnostics().iter().filter(|d| d.is_error()).count();
    Err(format!("check failed with {errors} error(s)"))
}

/// `line:column: message (function)`, without the function when it is empty.
fn location(issue: &Issue) -> String {
    let mut text = format!(
        "{}:{}: {}",
        issue.line.max(1),
        issue.column.max(1),
        issue.message
    );
    if !issue.function.is_empty() {
        text.push_str(&format!(" ({})", issue.function));
    }
    text
}

fn issues(report: &CheckReport) -> Vec<Issue> {
    let entry = |diagnostic: &CheckDiagnostic, incomplete: bool| Issue {
        module: diagnostic.filename.as_deref().map(<[u8]>::to_vec),
        line: diagnostic.position.line,
        column: diagnostic.position.column,
        message: if incomplete {
            format!("incomplete: {}", diagnostic.message)
        } else {
            diagnostic.message.clone()
        },
        function: match diagnostic.function.as_str() {
            "__main__" => "<script>".to_owned(),
            function => function.to_owned(),
        },
    };
    report
        .diagnostics
        .iter()
        .map(|diagnostic| entry(diagnostic, false))
        .chain(
            report
                .incomplete
                .iter()
                .map(|diagnostic| entry(diagnostic, true)),
        )
        .collect()
}

/// Names a required module by its resolved path, as the reference reports it.
fn module_path(module: &[u8], module_dirs: &[PathBuf]) -> String {
    let relative = compat::os_string(module);
    for directory in module_dirs {
        let candidate = directory.join(&relative);
        if candidate.is_file() {
            return std::fs::canonicalize(&candidate)
                .unwrap_or(candidate)
                .display()
                .to_string();
        }
    }
    String::from_utf8_lossy(module).into_owned()
}

fn stats_line(stats: &Stats) -> String {
    format!(
        "steps={} peak_bytes={} retained_bytes={}",
        stats.steps, stats.peak_memory_bytes, stats.retained_memory_bytes
    )
}

/// `run -check` for a file: the call `run` would make, with its arguments.
pub fn call(
    script: &Script,
    function: &str,
    arguments: &[Value],
    options: &CallOptions,
) -> Result<(), String> {
    let report = script
        .check_call(function, arguments, options)
        .map_err(|error| error.to_string())?;
    failure(issues(&report))
}

/// `run -check -e`: the whole snippet, sorted by position then function.
pub fn snippet(script: &Script, options: &CallOptions) -> Result<(), String> {
    let report = script.check(options).map_err(|error| error.to_string())?;
    let mut issues = issues(&report);
    issues.sort_by(|a, b| (a.line, a.column, &a.function).cmp(&(b.line, b.column, &b.function)));
    failure(issues)
}

fn failure(issues: Vec<Issue>) -> Result<(), String> {
    match issues.as_slice() {
        [] => Ok(()),
        [issue] => Err(format!("check failed: {}", location(issue))),
        issues => {
            let mut text = format!("check failed with {} issue(s):", issues.len());
            for issue in issues {
                text.push_str("\n  ");
                text.push_str(&location(issue));
            }
            Err(text)
        }
    }
}
