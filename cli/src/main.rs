//! The `vibes` command line: Go-compatible commands (`run`, `check`, `fmt`,
//! `analyze`, `test`, `lsp`, `repl`, `help`), `prelude`, `fix`,
//! and the flat form that runs a file directly.

mod analyze;
mod check;
mod compat;
mod fix;
mod flags;
mod flat;
mod format;
mod output;
mod prelude;
mod profiles;
mod render;
mod repl;
mod root;
mod run;
mod signal;
mod source;
mod testing;
mod watch;

use std::process::ExitCode;

fn main() -> ExitCode {
    signal::install();
    root::main(std::env::args_os().skip(1).collect())
}

/// Prints a command's help on stdout.
fn print_help(spec: &flags::Spec) -> Result<(), String> {
    root::print(&flags::help(spec))
}
