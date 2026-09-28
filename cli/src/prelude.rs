//! `vibes prelude`: print every builtin signature as Vibescript declarations.

use crate::flags::{self, Outcome, Spec};
use std::ffi::OsString;

pub const SPEC: Spec = Spec {
    name: "prelude",
    aliases: &[],
    usage: "print the builtin signatures as Vibescript declarations",
    arguments: "",
    usage_lines: &[],
    flags: &[],
};

/// Runs `vibes prelude` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    if !flags.positionals.is_empty() {
        return Err("vibes prelude: does not accept positional arguments".to_owned());
    }
    crate::output::Sink::Stdout.write(vibescript::signatures::prelude().as_bytes())
}
