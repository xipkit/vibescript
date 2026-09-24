//! `vibes test`: discover `*_test.vibe` files and run their `test_` functions.
//!
//! Discovery and execution come from `vibescript_tools::test_runner`; this
//! command resolves module paths, reads and compiles each file, and renders
//! the reference's report and exit status.

use crate::{
    compat,
    flags::{self, Flag, Kind, Outcome, Spec},
    output::Sink,
    profiles, render, run, signal, source,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
use vibescript::{Script, StatementKind};
use vibescript_tools::test_runner::{self, DiscoverError, Filter, SuiteError, TestFailure};

const FLAGS: [Flag; 6] = [
    Flag::new(
        &["run"],
        Kind::String,
        "run only test functions matching this regular expression",
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
    name: "test",
    aliases: &[],
    usage: "discover and run Vibescript tests",
    arguments: "[path...]",
    usage_lines: &[],
    flags: &FLAGS,
};

#[derive(Default)]
struct Summary {
    passed: usize,
    failed: usize,
}

/// Runs `vibes test` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    let limits = profiles::resolve(&flags).map_err(|error| format!("vibes test: {error}"))?;
    let filter = match flags.string("run").filter(|pattern| !pattern.is_empty()) {
        Some(pattern) => Some(
            Filter::new(&pattern)
                .map_err(|error| format!("vibes test: invalid -run pattern: {error}"))?,
        ),
        None => None,
    };
    let roots: Vec<PathBuf> = if flags.positionals.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        flags.positionals.iter().map(PathBuf::from).collect()
    };
    let files = test_runner::discover(&roots).map_err(discovery_error)?;
    if files.is_empty() {
        let roots: Vec<_> = roots
            .iter()
            .map(|root| root.display().to_string())
            .collect();
        return Err(format!(
            "vibes test: no *_test.vibe files found under {}",
            roots.join(", ")
        ));
    }
    let module_paths = flags.strings("module-path");
    let options = test_runner::Options {
        filter,
        call: run::options(limits),
    };
    let out = Sink::Stdout;
    let mut summary = Summary::default();
    for file in &files {
        if signal::stop_requested() {
            return Err("vibes test: context canceled".to_owned());
        }
        run_file(file, &module_paths, &options, &mut summary, &out)
            .map_err(|error| format!("vibes test: {error}"))?;
    }
    out.write(
        format!(
            "{} test(s) across {} file(s): {} passed, {} failed\n",
            summary.passed + summary.failed,
            files.len(),
            summary.passed,
            summary.failed
        )
        .as_bytes(),
    )
    .map_err(|error| format!("vibes test: write summary: {error}"))?;
    if summary.failed > 0 {
        return Err(format!("vibes test: {} test(s) failed", summary.failed));
    }
    Ok(())
}

fn discovery_error(error: DiscoverError) -> String {
    let quote = |path: &Path| compat::quote(&compat::bytes(path.as_os_str()));
    match error {
        DiscoverError::Access { root, error } => format!(
            "vibes test: access {}: {}",
            quote(&root),
            compat::path_error("stat", &root, &error)
        ),
        DiscoverError::NotTestFile { root } => {
            format!("vibes test: {} is not a *_test.vibe file", quote(&root))
        }
        DiscoverError::Walk {
            root,
            directory,
            error,
        } => format!(
            "vibes test: walk {}: {}",
            quote(&root),
            compat::path_error("open", &directory, &error)
        ),
    }
}

/// The reference's name for the statement kind its compiler rejects at the top level.
fn statement_type(kind: StatementKind) -> &'static str {
    match kind {
        StatementKind::Expression => "*ast.ExprStmt",
        StatementKind::Assignment => "*ast.AssignStmt",
        StatementKind::If => "*ast.IfStmt",
        StatementKind::While => "*ast.WhileStmt",
        StatementKind::Until => "*ast.UntilStmt",
        StatementKind::For => "*ast.ForStmt",
        StatementKind::Return => "*ast.ReturnStmt",
        StatementKind::Raise => "*ast.RaiseStmt",
        StatementKind::Break => "*ast.BreakStmt",
        StatementKind::Next => "*ast.NextStmt",
        StatementKind::Retry => "*ast.RetryStmt",
        StatementKind::Begin => "*ast.TryStmt",
    }
}

/// Runs one file's tests and prints its report lines.
fn run_file(
    file: &Path,
    module_paths: &[OsString],
    options: &test_runner::Options,
    summary: &mut Summary,
    out: &Sink,
) -> Result<(), String> {
    let display = file.display().to_string();
    let fail = |summary: &mut Summary, name: &str, error: &str| {
        summary.failed += 1;
        out.write(format!("--- FAIL: {display} :: {name}\n{}\n", indent(error)).as_bytes())
            .map_err(|error| format!("write test failure: {error}"))
    };
    let script = match load(file, module_paths) {
        Ok(script) => script,
        Err((stage, error)) => return fail(summary, stage, &error),
    };
    let failed = summary.failed;
    let mut written = Ok(());
    let count = test_runner::run_each(&script, options, |outcome| {
        if written.is_err() {
            return;
        }
        written = match &outcome.failure {
            None => {
                summary.passed += 1;
                Ok(())
            }
            Some(TestFailure::RequiresArguments) => fail(
                summary,
                &outcome.name,
                "test functions must not require parameters",
            ),
            Some(TestFailure::Error(error)) => {
                fail(summary, &outcome.name, &render::error(error, None))
            }
        };
    });
    let count = match count {
        Ok(count) => count,
        Err(SuiteError::TopLevelStatement(kind)) => {
            let message = format!("unsupported top-level statement {}", statement_type(kind));
            return fail(summary, "(compile)", &message);
        }
        Err(SuiteError::Outline(error)) => {
            return fail(summary, "(compile)", &render::error(&error, None));
        }
        Err(SuiteError::Filter(error)) => return Err(format!("invalid -run pattern: {error}")),
        Err(SuiteError::Cancelled) => return Err("context canceled".to_owned()),
    };
    written?;
    if count == 0 {
        return out
            .write(format!("ok   {display} (no test functions)\n").as_bytes())
            .map_err(|error| format!("write test result: {error}"));
    }
    if summary.failed == failed {
        out.write(format!("ok   {display} ({count} test(s))\n").as_bytes())
            .map_err(|error| format!("write test result: {error}"))?;
    }
    Ok(())
}

/// Compiles a test file with its directory first on the module path.
fn load(file: &Path, module_paths: &[OsString]) -> Result<Script, (&'static str, String)> {
    let absolute = compat::absolute(file).map_err(|error| ("(resolve)", compat::reason(&error)))?;
    let directory = absolute.parent().unwrap_or(Path::new("/"));
    let module_dirs =
        source::module_paths(directory, module_paths).map_err(|error| ("(module paths)", error))?;
    let engine = run::engine(&module_dirs, &Sink::Stdout, &Sink::Stderr)
        .map_err(|error| ("(engine)", error))?;
    let text = source::read(file).map_err(|error| ("(read)", error))?;
    engine
        .compile(&text)
        .map_err(|error| ("(compile)", render::error(&error, None)))
}

/// Indents every line of a failure message by four spaces.
fn indent(text: &str) -> String {
    text.trim_end_matches('\n')
        .split('\n')
        .map(|line| format!("    {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}
