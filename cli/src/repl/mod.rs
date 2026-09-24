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
mod quota;
#[cfg_attr(target_os = "wasi", allow(dead_code))]
mod render;
#[cfg(not(target_os = "wasi"))]
mod terminal;
#[cfg(test)]
mod tests;

use std::{
    ffi::OsString,
    io::{self, IsTerminal},
    process::ExitCode,
};
use vibescript_tools::repl::ReplOptions;

pub use quota::QuotaFlags;

/// The usage text printed by `vibes repl --help`.
pub const HELP: &str = "\
Usage: vibes repl [options]

Starts the interactive Vibescript REPL. With a terminal it runs full screen;
with piped input it evaluates one line at a time and prints each result.

Options:
  -profile NAME           execution quota profile: low, medium, high, xhigh
                          (default \"xhigh\")
  -step-quota N           override the profile's step quota (-1 = unlimited)
  -memory-quota N         override the profile's memory quota in bytes
                          (-1 = unlimited)
  -recursion-limit N      override the profile's recursion limit
                          (-1 = unlimited)
  -h, -help               show this help

Options may start with one or two dashes, and take their value as the next
argument or after '='.
";

/// A parsed `vibes repl` command line.
#[derive(Debug, Eq, PartialEq)]
pub enum Arguments {
    Help,
    Run(QuotaFlags),
}

/// Parses the arguments after `repl`, as the Go CLI's flag parser does.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Arguments, String> {
    let mut flags = QuotaFlags::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|arg| format!("invalid argument {}", arg.to_string_lossy()))?;
        if arg == "--" {
            if args.next().is_some() {
                return Err("vibes repl: does not accept positional arguments".to_owned());
            }
            break;
        }
        let Some(flag) = arg
            .strip_prefix("--")
            .or_else(|| arg.strip_prefix('-'))
            .filter(|flag| !flag.is_empty())
        else {
            return Err("vibes repl: does not accept positional arguments".to_owned());
        };
        let (name, inline) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        if matches!(name, "h" | "help") {
            return Ok(Arguments::Help);
        }
        let mut value = || match inline.clone() {
            Some(value) => Ok(value),
            None => args
                .next()
                .map(|value| value.to_string_lossy().into_owned())
                .ok_or_else(|| format!("flag needs an argument: -{name}")),
        };
        let number = |raw: String| {
            raw.parse::<i64>()
                .map_err(|_| format!("invalid value {raw:?} for flag -{name}: parse error"))
        };
        match name {
            "profile" => flags.profile = value()?,
            "step-quota" => flags.steps = Some(number(value()?)?),
            "memory-quota" => flags.memory = Some(number(value()?)?),
            "recursion-limit" => flags.recursion = Some(number(value()?)?),
            _ => return Err(format!("flag provided but not defined: -{name}")),
        }
    }
    Ok(Arguments::Run(flags))
}

/// Runs `vibes repl` with the arguments that follow `repl` and returns the
/// process status: 0 when the REPL quits normally, 1 for invalid arguments
/// or a terminal failure.
pub fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let flags = match parse(args) {
        Ok(Arguments::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Arguments::Run(flags)) => flags,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };
    let limits = match flags.resolve() {
        Ok(limits) => limits,
        Err(message) => {
            eprintln!("vibes repl: {message}");
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
