//! `vibes guide`: print the bundled Markdown language guide.

use crate::flags::{self, Outcome, Spec};
use std::ffi::OsString;

pub const SPEC: Spec = Spec {
    name: "guide",
    aliases: &[],
    usage: "print the language guide as Markdown",
    arguments: "",
    usage_lines: &[],
    flags: &[],
};

/// Runs `vibes guide` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    if !flags.positionals.is_empty() {
        return Err("vibes guide: does not accept positional arguments".to_owned());
    }
    crate::output::Sink::Stdout.write(vibescript::guide().as_bytes())
}
