//! `vibes repl`: the interactive Vibescript REPL, ported from the Go CLI.
//!
//! The session itself, with evaluation, commands, completion and history, is
//! the library's [`vibescript_tools::repl::ReplSession`]. [`model`] adds the
//! input line, panels and screen layout. With a terminal on both stdin and
//! stdout, [`terminal`] draws it full screen; otherwise, and always under
//! WASI, [`lines`] reads one input per line so scripts and tests can drive it.

// WASI has only line mode, which leaves the terminal's editing and layout
// code unused there.
#[cfg_attr(target_os = "wasi", allow(dead_code))]
mod editor;
mod lines;
#[cfg_attr(target_os = "wasi", allow(dead_code))]
mod model;
#[cfg_attr(target_os = "wasi", allow(dead_code))]
mod render;
#[cfg(not(target_os = "wasi"))]
mod terminal;
#[cfg(test)]
mod tests;

use crate::{
    flags::{self, Outcome, Spec},
    profiles,
};
use std::{
    ffi::OsString,
    io::{self, IsTerminal},
    process::ExitCode,
};
use vibescript::Limits;
use vibescript_tools::repl::ReplOptions;

/// The reference's `vibes repl` command: its summary and the quota flags it
/// shares with `run` and `test`.
pub const SPEC: Spec = Spec {
    name: "repl",
    aliases: &[],
    usage: "start the interactive Vibescript REPL",
    arguments: "",
    usage_lines: &[],
    flags: &profiles::FLAGS,
};

/// A parsed `vibes repl` command line.
#[derive(Debug)]
pub enum Arguments {
    Help,
    Run(Limits),
}

/// Parses the arguments after `repl` with the Go CLI's flag syntax and
/// resolves the quota profile, as the reference does before reading input.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let args: Vec<OsString> = args.into_iter().collect();
    let flags = match flags::parse(&SPEC, &args)? {
        Outcome::Help => return Ok(Arguments::Help),
        Outcome::Parsed(flags) => flags,
    };
    if !flags.positionals.is_empty() {
        return Err("vibes repl: does not accept positional arguments".to_owned());
    }
    profiles::resolve(&flags)
        .map(Arguments::Run)
        .map_err(|error| format!("vibes repl: {error}"))
}

/// Runs `vibes repl` with the arguments that follow `repl` and returns the
/// process status: 0 when the REPL quits normally, 1 for invalid arguments
/// or a terminal failure.
pub fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let limits = match parse(args) {
        Ok(Arguments::Help) => {
            return match crate::print_help(&SPEC) {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(Arguments::Run(limits)) => limits,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let mut model = model::Model::new(ReplOptions {
        limits,
        ..ReplOptions::default()
    });
    match start(&mut model) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("repl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start(model: &mut model::Model) -> io::Result<()> {
    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    #[cfg(not(target_os = "wasi"))]
    if interactive {
        return terminal::run(model);
    }
    let colors = if interactive || io::stdout().is_terminal() {
        render::Colors::detect()
    } else {
        render::Colors::None
    };
    lines::run(model, io::stdin().lock(), io::stdout().lock(), colors)
}
