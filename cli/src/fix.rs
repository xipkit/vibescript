//! `vibes fix`: applies every machine-applicable fix of the static
//! language's diagnostics (ADR-008) with `vibescript_tools::fix`, rechecking
//! until none applies.
//!
//! Each fix prints as `path:line:column: fixed V0401: message`, and each
//! diagnostic left as `path:line:column: error[V0405]: message`. With
//! `--dry-run` the files stay as they are and the changes print as a
//! unified diff.

use crate::{
    compat,
    flags::{self, Flag, Kind, Outcome, Spec},
    output::Sink,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
use vibescript::{Engine, diagnostic::Diagnostic};
use vibescript_tools::{diff::unified_diff, fix};

const FLAGS: [Flag; 1] = [Flag::new(
    &["dry-run"],
    Kind::Bool,
    "print the changes as a diff instead of writing them",
)];

pub const SPEC: Spec = Spec {
    name: "fix",
    aliases: &[],
    usage: "apply the fixes of removed spellings and other compile diagnostics",
    arguments: "<file or directory>...",
    usage_lines: &[],
    flags: &FLAGS,
};

/// Runs `vibes fix` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, &flags_first(args))? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    if flags.positionals.is_empty() {
        return Err("vibes fix: file or directory required".to_owned());
    }
    let dry_run = flags.bool("dry-run");
    let mut files = Vec::new();
    for positional in &flags.positionals {
        let path = PathBuf::from(positional);
        collect(&path, &path, &mut files)
            .map_err(|error| format!("collect {}: {error}", path.display()))?;
    }
    let engine = Engine::new();
    // A syntax error that carries a coded diagnostic, such as a hash
    // argument without parentheses, may have a fix too.
    let check = |text: &str| match engine.type_check(text) {
        Ok(checked) => Ok(checked.diagnostics),
        Err(error) if !error.diagnostics().is_empty() => Ok(error.diagnostics().to_vec()),
        Err(error) => Err(error),
    };
    let mut report = String::new();
    let mut diff = String::new();
    let (mut fixes, mut changed_files, mut errors) = (0, 0, 0);
    for (path, label) in &files {
        let source = std::fs::read_to_string(path)
            .map_err(|error| format!("read {}: {}", path.display(), compat::reason(&error)))?;
        let fixed = match fix::fix(&source, check) {
            Ok(fixed) => fixed,
            Err(error) => {
                report.push_str(&format!("{label}: does not parse: {error}\n"));
                errors += 1;
                continue;
            }
        };
        for applied in &fixed.applied {
            let diagnostic = &applied.diagnostic;
            report.push_str(&format!(
                "{label}:{}:{}: fixed {}: {}\n",
                applied.line, applied.column, diagnostic.code, diagnostic.message
            ));
        }
        for diagnostic in &fixed.remaining {
            report.push_str(&remaining(label, diagnostic, &fixed.source));
            errors += usize::from(diagnostic.is_error());
        }
        fixes += fixed.applied.len();
        if !fixed.changed() {
            continue;
        }
        changed_files += 1;
        if dry_run {
            diff.push_str(&unified_diff(label, &source, &fixed.source));
        } else {
            std::fs::write(path, &fixed.source)
                .map_err(|error| format!("write {}: {}", path.display(), compat::reason(&error)))?;
        }
    }
    Sink::Stdout
        .write(format!("{diff}{report}").as_bytes())
        .map_err(|error| format!("write report: {error}"))?;
    let verb = if dry_run { "would fix" } else { "fixed" };
    eprintln!("vibes fix: {verb} {fixes} issue(s) in {changed_files} file(s)");
    if errors > 0 {
        return Err(format!("vibes fix: {errors} error(s) remain"));
    }
    Ok(())
}

/// `path:line:column: error[V0405]: message`, for a diagnostic no fix
/// repaired.
fn remaining(label: &str, diagnostic: &Diagnostic, source: &str) -> String {
    let source = diagnostic.source.as_deref().unwrap_or(source);
    let label = diagnostic
        .file
        .as_deref()
        .map(String::from_utf8_lossy)
        .unwrap_or_else(|| label.into());
    let position = diagnostic.span.position(source);
    let place = format!("{label}:{}:{}", position.line, position.column);
    format!("{place}: {diagnostic}\n")
}

/// Lists the `.vibe` files under `path` in order, with their names relative to `root`.
fn collect(root: &Path, path: &Path, out: &mut Vec<(PathBuf, String)>) -> std::io::Result<()> {
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()?;
        entries.sort();
        for entry in entries {
            if entry.is_dir() || entry.extension().is_some_and(|e| e == "vibe") {
                collect(root, &entry, out)?;
            }
        }
        return Ok(());
    }
    let label = if root.is_dir() {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    } else {
        path.to_string_lossy().into_owned()
    };
    out.push((path.to_owned(), label));
    Ok(())
}

/// Moves flags ahead of the paths, so they may follow them as well.
fn flags_first(args: &[OsString]) -> Vec<OsString> {
    let (flags, paths): (Vec<OsString>, Vec<OsString>) = args
        .iter()
        .take_while(|arg| *arg != "--")
        .cloned()
        .partition(|arg| {
            let text = arg.to_string_lossy();
            text.starts_with('-') && text.len() > 1
        });
    let mut out = flags;
    out.push(OsString::from("--"));
    out.extend(paths);
    if let Some(split) = args.iter().position(|arg| arg == "--") {
        out.extend(args[split + 1..].iter().cloned());
    }
    out
}
