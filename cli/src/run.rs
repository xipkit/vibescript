//! `vibes run`: execute a script file or an inline snippet.

use crate::{
    check, compat,
    flags::{self, Flag, Kind, Outcome, Spec},
    output::Sink,
    profiles, render, signal, source, watch,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
use vibescript::tooling::{self, ItemKind};
use vibescript::{CallOptions, Engine, Error, ErrorKind, Limits, ModuleConfig, Script, Value};

const FLAGS: [Flag; 10] = [
    Flag::new(
        &["function"],
        Kind::String,
        "function to invoke; without it, top-level statements run when present, otherwise run",
    ),
    Flag::new(
        &["check"],
        Kind::Bool,
        "compile and validate static contracts without executing",
    ),
    Flag::new(
        &["static"],
        Kind::Bool,
        "type check statically (ADR-007) and refuse a script with type errors",
    ),
    Flag::new(
        &["e"],
        Kind::String,
        "evaluate an inline snippet instead of a script file",
    ),
    Flag::new(
        &["watch"],
        Kind::Bool,
        "re-run whenever the script or its modules change",
    ),
    Flag::new(
        &["module-path"],
        Kind::Strings,
        "add a module search directory (repeatable)",
    ),
    profiles::FLAGS[0],
    profiles::FLAGS[1],
    profiles::FLAGS[2],
    profiles::FLAGS[3],
];

pub const SPEC: Spec = Spec {
    name: "run",
    aliases: &[],
    usage: "execute a script file or inline snippet",
    arguments: "<script> [args...]",
    usage_lines: &[
        "vibes run [options] <script> [args...]",
        "vibes run [options] -e SNIPPET",
    ],
    flags: &FLAGS,
};

/// The top-level entrypoint's name in the reference, where snippets compile it.
const SCRIPT_ENTRYPOINT: &str = "<script>";

/// Everything needed to execute a script file once, so single runs and
/// watch-mode re-runs share one code path.
pub struct Invocation {
    /// The absolute script path.
    pub script: PathBuf,
    /// The `-function` value, if given.
    pub function: Option<String>,
    pub check: bool,
    /// Whether to type check statically before running (`--static`).
    pub static_types: bool,
    pub module_dirs: Vec<PathBuf>,
    pub arguments: Vec<Value>,
    pub limits: Limits,
}

/// Runs `vibes run` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    let limits = profiles::resolve(&flags).map_err(|error| format!("vibes run: {error}"))?;
    let module_paths = flags.strings("module-path");
    let check = flags.bool("check");
    // The gradual checker of `--check` reads the ADR-004 language.
    let static_types = flags.bool("static") || (vibescript::STATIC_TYPES_BY_DEFAULT && !check);
    if let Some(snippet) = flags.value("e") {
        if flags.bool("watch") {
            return Err("vibes run: -e cannot be combined with -watch".to_owned());
        }
        if flags.is_set("function") {
            return Err("vibes run: -e cannot be combined with -function".to_owned());
        }
        if !flags.positionals.is_empty() {
            return Err("vibes run: -e does not accept positional arguments".to_owned());
        }
        let snippet = String::from_utf8_lossy(&compat::bytes(snippet)).into_owned();
        return evaluate(&snippet, &module_paths, limits, check, static_types);
    }
    let Some((script, arguments)) = flags.positionals.split_first() else {
        return Err("vibes run: script path required".to_owned());
    };
    let script = compat::absolute(Path::new(script))
        .map_err(|error| format!("resolve script path: {}", compat::reason(&error)))?;
    let directory = script.parent().unwrap_or(Path::new("/")).to_owned();
    let module_dirs = source::module_paths(&directory, &module_paths)
        .map_err(|error| format!("compute module paths: {error}"))?;
    let invocation = Invocation {
        script,
        function: flags.string("function"),
        check,
        static_types,
        module_dirs,
        arguments: arguments
            .iter()
            .map(|argument| Value::bytes(compat::bytes(argument).into_owned()))
            .collect(),
        limits,
    };
    if flags.bool("watch") {
        return watch::watch(
            &invocation,
            watch::DEFAULT_INTERVAL,
            &signal::stop_requested,
            &Sink::Stdout,
            &Sink::Stderr,
        );
    }
    execute(&invocation, &Sink::Stdout, &Sink::Stderr)
}

/// Creates an engine whose `puts`, `print` and `p` write to `out` and whose
/// `warn` writes to `err`.
pub fn engine(module_dirs: &[PathBuf], out: &Sink, err: &Sink) -> Result<Engine, String> {
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: module_dirs.to_vec(),
            ..ModuleConfig::default()
        })
        .map_err(|error| format!("create engine: {error}"))?;
    let (out, err) = (out.clone(), err.clone());
    engine.set_output_writer(move |_, bytes| forward(&out, bytes));
    engine.set_error_writer(move |_, bytes| forward(&err, bytes));
    Ok(engine)
}

fn forward(sink: &Sink, bytes: &[u8]) -> vibescript::Result<()> {
    sink.write(bytes)
        .map_err(|error| Error::new(ErrorKind::Host, error))
}

/// Compiles and runs, or checks, one script file.
pub fn execute(invocation: &Invocation, out: &Sink, err: &Sink) -> Result<(), String> {
    let mut engine = engine(&invocation.module_dirs, out, err)?;
    engine.set_static_types(invocation.static_types);
    let source =
        source::read(&invocation.script).map_err(|error| format!("read script: {error}"))?;
    let script = engine.compile(&source).map_err(|error| {
        compile_failure(
            &error,
            &source,
            &invocation.script.display().to_string(),
            None,
        )
    })?;
    let function = match invocation.function.as_deref() {
        Some(SCRIPT_ENTRYPOINT) => "__main__",
        Some(function) => function,
        None if has_top_level_statements(&script)? => "__main__",
        None => "run",
    };
    if invocation.static_types && function != "__main__" {
        let found = engine
            .check_entry_arguments(&source, function, invocation.arguments.len())
            .map_err(|error| format!("compile failed: {}", render::error(&error, None)))?;
        if !found.is_empty() {
            let label = invocation.script.display().to_string();
            let mut text = format!("compile failed with {} diagnostic(s)", found.len());
            for diagnostic in &found {
                text.push('\n');
                text.push_str(&render::diagnostic(diagnostic, &source, &label));
            }
            text.truncate(text.trim_end().len());
            return Err(text);
        }
    }
    let options = options(invocation.limits.clone());
    if invocation.check {
        return check::call(&script, function, &invocation.arguments, &options);
    }
    let outcome = script
        .call(function, &invocation.arguments, options)
        .map_err(|error| format!("execution failed: {}", render::error(&error, None)))?;
    print_result(&outcome.value, out)
}

/// The message for a failed compilation: every static diagnostic, rendered
/// against the source and headed by `label`, or the error itself.
pub fn compile_failure(error: &Error, source: &str, label: &str, snippet: Option<&str>) -> String {
    if error.diagnostics().is_empty() {
        return format!("compile failed: {}", render::error(error, snippet));
    }
    let mut text = format!(
        "compile failed with {} diagnostic(s)",
        error.diagnostics().len()
    );
    for diagnostic in error.diagnostics() {
        text.push('\n');
        text.push_str(&render::diagnostic(diagnostic, source, label));
    }
    text.truncate(text.trim_end().len());
    text
}

fn has_top_level_statements(script: &Script) -> Result<bool, String> {
    tooling::outline(script.source())
        .map(|outline| {
            outline
                .items
                .iter()
                .any(|item| matches!(item.kind, ItemKind::Statement(_)))
        })
        .map_err(|error| format!("compile failed: {}", render::error(&error, None)))
}

/// Call options with the given limits and the process interrupt token.
pub fn options(limits: Limits) -> CallOptions {
    CallOptions {
        limits,
        cancellation: signal::token(),
        ..CallOptions::default()
    }
}

/// Evaluates `-e`: a snippet whose top-level statements run, with the
/// working directory as its first module root.
fn evaluate(
    snippet: &str,
    module_paths: &[OsString],
    limits: Limits,
    check: bool,
    static_types: bool,
) -> Result<(), String> {
    if compat::trim_space(snippet).is_empty() {
        return Err("vibes run: -e requires a non-empty snippet".to_owned());
    }
    let directory = compat::working_directory()
        .map_err(|error| format!("resolve working directory: {}", compat::reason(&error)))?;
    let module_dirs = source::module_paths(&directory, module_paths)
        .map_err(|error| format!("compute module paths: {error}"))?;
    let mut engine = engine(&module_dirs, &Sink::Stdout, &Sink::Stderr)?;
    engine.set_static_types(static_types);
    let script = engine
        .compile(snippet)
        .map_err(|error| compile_failure(&error, snippet, "<eval>", Some(snippet)))?;
    let options = options(limits);
    if check {
        return check::snippet(&script, &options);
    }
    let outcome = script
        .run(options)
        .map_err(|error| format!("execution failed: {}", render::error(&error, Some(snippet))))?;
    print_result(&outcome.value, &Sink::Stdout)
}

/// Prints a non-nil result in the reference's string form.
fn print_result(value: &Value, out: &Sink) -> Result<(), String> {
    if value.type_name() == "nil" {
        return Ok(());
    }
    let mut rendered = render::value(value).map_err(|failure| failure.to_string())?;
    rendered.push(b'\n');
    out.write(&rendered)
        .map_err(|error| format!("write result: {error}"))
}
