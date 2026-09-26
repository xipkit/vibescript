//! Command selection, root help and the `help` command.
//!
//! The first argument alone selects what runs, in this order:
//!
//! 1. `-h` or `--help` prints the root help, and a first argument the Go
//!    reference rejects outright (`--`, surrounding whitespace, `-help`,
//!    `--h` or a valued help flag) reports an unknown command.
//! 2. A command name runs that command with Go-style flags.
//! 3. `--version` prints the version.
//! 4. A flat-form option or a script path runs the flat form (`vibes help flat`).
//! 5. Anything else is an undefined flag or an unknown command, as in the reference.

use crate::{
    analyze, check, compat, fix,
    flags::{self, Outcome, Spec},
    flat, format, migrate, prelude, repl, run, signal, testing,
};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::Path,
    process::ExitCode,
};

const HELP_SPEC: Spec = Spec {
    name: "help",
    aliases: &["h"],
    usage: "Shows a list of commands or help for one command",
    arguments: "[command]",
    usage_lines: &[],
    flags: &[],
};

const LSP_SPEC: Spec = Spec {
    name: "lsp",
    aliases: &[],
    usage: "start the language server over stdio",
    arguments: "",
    usage_lines: &[],
    flags: &[],
};

/// Every command, in the order the root help lists them.
const COMMANDS: [&Spec; 11] = [
    &run::SPEC,
    &check::SPEC,
    &format::SPEC,
    &analyze::SPEC,
    &testing::SPEC,
    &LSP_SPEC,
    &repl::SPEC,
    &prelude::SPEC,
    &migrate::SPEC,
    &fix::SPEC,
    &HELP_SPEC,
];

/// Options that select the flat form when they come first.
const FLAT_OPTIONS: [&str; 11] = [
    "-e",
    "--eval",
    "--function",
    "--module-path",
    "--arg",
    "--kwarg",
    "--steps",
    "--memory",
    "--recursion",
    "--timeout-ms",
    "--stats",
];

/// Runs the command line that follows the program name.
pub fn main(args: Vec<OsString>) -> ExitCode {
    let Some(first) = args.first() else {
        return root_error("command required");
    };
    let bytes = compat::bytes(first);
    let text = String::from_utf8_lossy(&bytes);
    if text == "-h" || text == "--help" {
        return finish(print(&root_help()));
    }
    if text == "--" || compat::trim_space(&text) != text || unsupported_help(&text) {
        return root_error(&format!("unknown command {}", compat::quote(&bytes)));
    }
    if let Some(spec) = command(&text) {
        if spec.name == "repl" {
            return repl::run(args.into_iter().skip(1));
        }
        return finish(run_command(spec.name, &args[1..]));
    }
    if text == "--version" {
        return finish(print(&format!(
            "vibescript.rs {}\n",
            env!("CARGO_PKG_VERSION")
        )));
    }
    if flat_claims(first, &bytes) {
        return flat_main(args);
    }
    match undefined_flag(&bytes) {
        Some(name) => finish(Err(format!("flag provided but not defined: -{name}"))),
        None => root_error(&format!("unknown command {}", compat::quote(&bytes))),
    }
}

fn command(name: &str) -> Option<&'static Spec> {
    COMMANDS
        .into_iter()
        .find(|spec| spec.name == name || spec.aliases.contains(&name))
}

fn run_command(name: &str, args: &[OsString]) -> Result<(), String> {
    match name {
        "run" => run::command(args),
        "check" => check::command(args),
        "fmt" => format::command(args),
        "analyze" => analyze::command(args),
        "test" => testing::command(args),
        "lsp" => lsp(args),
        "prelude" => prelude::command(args),
        "migrate" => migrate::command(args),
        "fix" => fix::command(args),
        _ => help(args),
    }
}

/// Reports an error on stderr after the root help, and exits with status 1.
fn root_error(message: &str) -> ExitCode {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(root_help().as_bytes());
    let _ = writeln!(stderr, "{message}");
    ExitCode::FAILURE
}

/// Prints an error, if any, and maps it to status 1.
fn finish(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = writeln!(io::stderr().lock(), "{message}");
            ExitCode::FAILURE
        }
    }
}

/// Help spellings the reference refuses as a first argument.
fn unsupported_help(text: &str) -> bool {
    text == "-help"
        || text == "--h"
        || ["-h=", "--h=", "-help=", "--help="]
            .iter()
            .any(|prefix| text.starts_with(prefix))
}

/// Whether the first argument selects the flat form: one of its options, or
/// a script path that exists or is spelled like one.
fn flat_claims(first: &OsStr, bytes: &[u8]) -> bool {
    if bytes.first() == Some(&b'-') {
        return FLAT_OPTIONS.iter().any(|option| option.as_bytes() == bytes);
    }
    !bytes.is_empty()
        && (Path::new(first).is_file()
            || bytes.ends_with(b".vibe")
            || String::from_utf8_lossy(bytes)
                .chars()
                .any(std::path::is_separator))
}

/// The name the reference reports for a root argument it parses as a flag:
/// anything after `--`, or after `-` when a letter follows.
fn undefined_flag(bytes: &[u8]) -> Option<String> {
    let body = if let Some(body) = bytes.strip_prefix(b"--") {
        body
    } else {
        let body = bytes.strip_prefix(b"-")?;
        let first = String::from_utf8_lossy(body).chars().next()?;
        if !first.is_alphabetic() {
            return None;
        }
        body
    };
    let name = body.split(|&b| b == b'=').next().unwrap_or_default();
    Some(String::from_utf8_lossy(name).into_owned())
}

fn flat_main(args: Vec<OsString>) -> ExitCode {
    let result = match flat::parse(args) {
        Ok(flat::Command::Help) => {
            return finish(print(flat::HELP));
        }
        Ok(flat::Command::Run(invocation)) => flat::run(*invocation),
        Err(failure) => Err(failure),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            let _ = writeln!(io::stderr().lock(), "{failure}");
            failure.exit_code()
        }
    }
}

/// Renders the root help as the reference does.
fn root_help() -> String {
    let rows: Vec<(String, String)> = COMMANDS
        .iter()
        .map(|spec| {
            let names: Vec<&str> = std::iter::once(spec.name)
                .chain(spec.aliases.iter().copied())
                .collect();
            (names.join(", "), spec.usage.to_owned())
        })
        .collect();
    format!(
        "NAME:\n   vibes - run Vibescript programs and development tools\n\n\
         USAGE:\n   vibes [global options] [command [command options]]\n\n\
         COMMANDS:\n{}\nGLOBAL OPTIONS:\n{}",
        flags::table(&rows),
        flags::table(&[("--help, -h".to_owned(), "show help".to_owned())])
    )
}

/// Writes help text on stdout.
pub fn print(text: &str) -> Result<(), String> {
    let mut out = io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|error| format!("write help: {}", compat::reason(&error)))
}

/// `vibes help [command]`: the root help, one command's help, or the flat form's.
fn help(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&HELP_SPEC, args)? {
        Outcome::Help => return print(&flags::help(&HELP_SPEC)),
        Outcome::Parsed(flags) => flags,
    };
    match flags.positionals.as_slice() {
        [] => print(&root_help()),
        [topic] => {
            let topic = topic.to_string_lossy();
            if topic == "flat" {
                return print(flat::HELP);
            }
            match command(&topic) {
                Some(spec) => print(&flags::help(spec)),
                None => Err(format!("No help topic for '{topic}'")),
            }
        }
        _ => Err("vibes help: expected at most one command".to_owned()),
    }
}

/// `vibes lsp`: serves the language server over stdin and stdout until the
/// client exits or closes its input, or an interrupt arrives.
fn lsp(args: &[OsString]) -> Result<(), String> {
    let flags = match flags::parse(&LSP_SPEC, args)? {
        Outcome::Help => return print(&flags::help(&LSP_SPEC)),
        Outcome::Parsed(flags) => flags,
    };
    if !flags.positionals.is_empty() {
        return Err("vibes lsp: does not accept positional arguments".to_owned());
    }
    let output = io::BufWriter::new(io::stdout().lock());
    let mut server = vibescript_tools::lsp::Server::new();
    vibescript_tools::lsp::serve(&mut server, io::stdin(), output, &signal::token())
        .map_err(|error| error.to_string())
}
