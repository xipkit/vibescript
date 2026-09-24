//! `vibes fmt`: canonical formatting of `.vibe` files.
//!
//! The formatter itself lives in `vibescript_tools::format`; this command
//! discovers files, prints, checks or rewrites them, and reports as the
//! reference does. It works on bytes, so invalid UTF-8 passes through unchanged.

mod files;

use crate::{
    compat,
    flags::{self, Flag, Kind, Outcome, Spec},
};
use std::{
    ffi::OsString,
    io::{self, Write},
};
use vibescript_tools::format::format_bytes;

const FLAGS: [Flag; 2] = [
    Flag::new(
        &["w"],
        Kind::Bool,
        "write results to source files instead of stdout",
    ),
    Flag::new(
        &["check"],
        Kind::Bool,
        "fail if any source file needs formatting",
    ),
];

pub const SPEC: Spec = Spec {
    name: "fmt",
    aliases: &[],
    usage: "canonically format Vibescript source files",
    arguments: "<path>...",
    usage_lines: &[],
    flags: &FLAGS,
};

/// Runs `vibes fmt` with the arguments after the command name.
pub fn command(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&SPEC, args)? {
        Outcome::Help => return crate::print_help(&SPEC),
        Outcome::Parsed(flags) => flags,
    };
    if flags.positionals.is_empty() {
        return Err("vibes fmt: path required".to_owned());
    }
    let (write, check) = (flags.bool("w"), flags.bool("check"));
    let mut inputs =
        files::collect(&flags.positionals).map_err(|error| format!("collect files: {error}"))?;
    let mut changed = 0;
    for index in 0..inputs.files.len() {
        let path = inputs.files[index].path.clone();
        let (original, info) = inputs
            .read(index)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let formatted = format_bytes(&original);
        let differs = formatted != original;
        if differs {
            changed += 1;
        }
        if write && differs {
            inputs
                .write(index, &info, &formatted)
                .map_err(|error| format!("write {}: {error}", path.display()))?;
        } else if !write && !check {
            let mut out = io::stdout().lock();
            out.write_all(&formatted)
                .and_then(|()| out.flush())
                .map_err(|error| format!("write formatted output: {}", compat::reason(&error)))?;
        }
    }
    if check && changed > 0 {
        return Err(format!("vibes fmt: {changed} file(s) need formatting"));
    }
    Ok(())
}
