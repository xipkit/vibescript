//! `vibes check`: compiles a script with static types (ADR-007) and prints
//! every diagnostic without executing anything.

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
};

const FLAGS: [Flag; 3] = [
    Flag::new(
        &["module-path"],
        Kind::Strings,
        "add a module search directory (repeatable)",
    ),
    Flag::new(
        &["eval", "e"],
        Kind::String,
        "check an inline snippet instead of a script file",
    ),
    Flag::new(
        &["json"],
        Kind::Bool,
        "print each diagnostic as one JSON object per line",
    ),
];

pub const SPEC: Spec = Spec {
    name: "check",
    aliases: &[],
    usage: "type check a script without executing it",
    arguments: "<script>",
    usage_lines: &[],
    flags: &FLAGS,
};

/// The label inline source carries in reports.
const EVAL_LABEL: &str = "<eval>";

/// Runs `vibes check` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
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
    let engine = run::engine(&module_dirs, &Sink::Stdout, &Sink::Stderr)?;
    let (text, snippet) = match text {
        Ok(snippet) => (snippet, true),
        Err(script) => (
            source::read(&script).map_err(|error| format!("read script: {error}"))?,
            false,
        ),
    };
    if flags.bool("json") {
        return json_check(&engine, &text, &module_dirs);
    }
    human_check(&engine, &text, &label, snippet)
}

/// `vibes check`: compiles the script and prints every diagnostic with its
/// source line and fixes, or `No issues found`.
fn human_check(
    engine: &vibescript::Engine,
    text: &str,
    label: &str,
    snippet: bool,
) -> Result<(), String> {
    let diagnostics = match engine.compile(text) {
        // A program that compiles may still have warnings.
        Ok(_) => engine
            .type_check(text)
            .map(|checked| checked.diagnostics)
            .unwrap_or_default(),
        Err(error) if error.diagnostics().is_empty() => {
            return Err(format!(
                "compile failed: {}",
                render::error(&error, snippet.then_some(text))
            ));
        }
        Err(error) => error.diagnostics().to_vec(),
    };
    if diagnostics.is_empty() {
        println!("No issues found");
        return Ok(());
    }
    let mut out = io::stdout().lock();
    for diagnostic in &diagnostics {
        write!(out, "{}", render::diagnostic(diagnostic, text, label))
            .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    }
    out.flush()
        .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    match diagnostics.iter().filter(|d| d.is_error()).count() {
        0 => Ok(()),
        errors => Err(format!("check failed with {errors} error(s)")),
    }
}

/// `vibes check --json`: compiles the script and prints each
/// diagnostic as one JSON object per line, positioned in the text of the
/// file it is in. A syntax error is a `V0001` diagnostic.
fn json_check(
    engine: &vibescript::Engine,
    text: &str,
    module_dirs: &[PathBuf],
) -> Result<(), String> {
    use vibescript::diagnostic::{Code, Diagnostic, Span};
    let diagnostics = match engine.compile(text) {
        // A program that compiles may still have warnings.
        Ok(_) => engine
            .type_check(text)
            .map(|checked| checked.diagnostics)
            .unwrap_or_default(),
        Err(error) if error.diagnostics().is_empty() => {
            if error.kind != vibescript::ErrorKind::Syntax {
                return Err(format!("compile failed: {}", render::error(&error, None)));
            }
            let at = Span::at(error.offset.unwrap_or(0));
            vec![Diagnostic::error(Code::SYNTAX, at, error.message.clone())]
        }
        Err(error) => error.diagnostics().to_vec(),
    };
    let mut out = io::stdout().lock();
    for diagnostic in &diagnostics {
        let module = diagnostic
            .file
            .as_deref()
            .map(|file| module_text(file, module_dirs));
        let line = diagnostic.to_json(module.as_deref().unwrap_or(text));
        writeln!(out, "{line}")
            .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    }
    out.flush()
        .map_err(|error| format!("write check output: {}", compat::reason(&error)))?;
    match diagnostics.iter().filter(|d| d.is_error()).count() {
        0 => Ok(()),
        errors => Err(format!("check failed with {errors} error(s)")),
    }
}

/// The text of a required file, by its root-relative name, or nothing when
/// no module directory holds it.
fn module_text(file: &[u8], module_dirs: &[PathBuf]) -> String {
    let name = String::from_utf8_lossy(file);
    module_dirs
        .iter()
        .find_map(|directory| std::fs::read_to_string(directory.join(name.as_ref())).ok())
        .unwrap_or_default()
}
