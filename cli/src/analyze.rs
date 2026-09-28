//! `vibes analyze`: report lint findings from `vibescript_tools::analyze`.

use crate::{
    compat,
    flags::{self, Outcome, Spec},
    output::Sink,
    render, run, source,
};
use std::{ffi::OsString, path::Path};
use vibescript_tools::analyze;

pub const SPEC: Spec = Spec {
    name: "analyze",
    aliases: &[],
    usage: "analyze a script for lint issues",
    arguments: "<script>",
    usage_lines: &[],
    flags: &[],
};

/// Runs `vibes analyze` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    let script = match flags.positionals.as_slice() {
        [] => return Err("vibes analyze: script path required".to_owned()),
        [script] => script,
        _ => return Err("vibes analyze: expected a single script path".to_owned()),
    };
    let path = compat::absolute(Path::new(script))
        .map_err(|error| format!("resolve script path: {}", compat::reason(&error)))?;
    let engine = run::engine(&[], &Sink::Stdout, &Sink::Stderr)?;
    let text = source::read(&path).map_err(|error| format!("read script: {error}"))?;
    let failed = |error| format!("analysis compile failed: {}", render::error(&error, None));
    let findings = engine
        .compile(&text)
        .and_then(|script| analyze::analyze_script(&script))
        .map_err(failed)?;
    let out = Sink::Stdout;
    if findings.is_empty() {
        return out
            .write(b"No issues found\n")
            .map_err(|error| format!("write analysis output: {error}"));
    }
    let mut report = String::new();
    for finding in &findings {
        report.push_str(&format!(
            "{}:{}:{}: {} ({})\n",
            path.display(),
            finding.position.line.max(1),
            finding.position.column.max(1),
            finding.message,
            finding.function
        ));
    }
    out.write(report.as_bytes())
        .map_err(|error| format!("write analysis output: {error}"))?;
    Err(format!("analysis found {} issue(s)", findings.len()))
}
