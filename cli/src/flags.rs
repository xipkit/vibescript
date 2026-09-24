//! Command specifications, Go-style flag parsing and help rendering.
//!
//! Subcommands follow the Go reference's syntax: flags take one or two
//! leading hyphens, a value follows as `-name value` or `-name=value`, parsing
//! stops at the first positional argument, and `--` ends flags explicitly.
//! Every later token is kept verbatim. Errors use Go's wording, and the help
//! text reproduces the reference's layout.

use crate::compat::{self, IntError};
use std::ffi::{OsStr, OsString};

/// The value a flag accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Bool,
    String,
    /// A signed 64-bit integer in Go syntax.
    Int,
    /// An unsigned 64-bit integer in Go syntax.
    Uint,
    /// A repeatable string.
    Strings,
}

/// One flag of a command.
#[derive(Clone, Copy)]
pub struct Flag {
    /// The primary name first, then aliases.
    pub names: &'static [&'static str],
    pub kind: Kind,
    pub usage: &'static str,
    /// A default shown in help as `(default: "value")`.
    pub default: Option<&'static str>,
}

impl Flag {
    pub const fn new(name: &'static [&'static str], kind: Kind, usage: &'static str) -> Self {
        Self {
            names: name,
            kind,
            usage,
            default: None,
        }
    }
}

/// A command's name, summary, usage lines and flags.
pub struct Spec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    /// The argument placeholder after `[options]`, or explicit usage lines.
    pub arguments: &'static str,
    pub usage_lines: &'static [&'static str],
    pub flags: &'static [Flag],
}

/// Every flag occurrence in command-line order, and the positional arguments.
pub struct Parsed {
    values: Vec<(&'static str, OsString)>,
    pub positionals: Vec<OsString>,
}

/// The result of parsing a command line.
pub enum Outcome {
    /// `-h` or `--help` appeared before any error; nothing else applies.
    Help,
    Parsed(Parsed),
}

const HELP: Flag = Flag::new(&["help", "h"], Kind::Bool, "show help");

/// Parses the arguments after a command name with the reference's rules.
pub fn parse(spec: &Spec, args: &[OsString]) -> Result<Outcome, String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = compat::bytes(&args[index]);
        if &*argument == b"--" {
            return Ok(Outcome::Parsed(Parsed {
                values,
                positionals: args[index + 1..].to_vec(),
            }));
        }
        if argument.is_empty() || &*argument == b"-" || argument[0] != b'-' {
            return Ok(Outcome::Parsed(Parsed {
                values,
                positionals: args[index..].to_vec(),
            }));
        }
        let mut body = &argument[1..];
        if body.first() == Some(&b'-') {
            body = &body[1..];
        }
        if matches!(body.first(), Some(b'-' | b'=')) {
            return Err(format!(
                "bad flag syntax: {}",
                String::from_utf8_lossy(&argument)
            ));
        }
        let (name, value) = match body.iter().position(|&b| b == b'=') {
            Some(split) => (&body[..split], Some(&body[split + 1..])),
            None => (body, None),
        };
        let name_text = String::from_utf8_lossy(name);
        let Some(flag) = spec
            .flags
            .iter()
            .chain([&HELP])
            .find(|flag| flag.names.contains(&&*name_text))
        else {
            return Err(format!("flag provided but not defined: -{name_text}"));
        };
        let primary = flag.names[0];
        if flag.kind == Kind::Bool {
            if primary == "help" {
                if value.is_some() {
                    return Err("help flag does not accept a value".to_owned());
                }
                return Ok(Outcome::Help);
            }
            let enabled = match value {
                None => true,
                Some(value) => std::str::from_utf8(value)
                    .ok()
                    .and_then(compat::parse_bool)
                    .ok_or_else(|| {
                        format!(
                            "invalid boolean value {} for -{name_text}: parse error",
                            compat::quote(value)
                        )
                    })?,
            };
            values.push((
                primary,
                OsString::from(if enabled { "true" } else { "false" }),
            ));
            index += 1;
            continue;
        }
        let value = match value {
            Some(value) => compat::os_string(value),
            None => {
                let Some(next) = args.get(index + 1) else {
                    return Err(format!("flag needs an argument: -{name_text}"));
                };
                index += 1;
                next.clone()
            }
        };
        if matches!(flag.kind, Kind::Int | Kind::Uint) {
            let bytes = compat::bytes(&value);
            let text = std::str::from_utf8(&bytes).map_err(|_| IntError::Syntax);
            let checked = match flag.kind {
                Kind::Int => text.and_then(compat::parse_int).map(drop),
                _ => text.and_then(compat::parse_uint).map(drop),
            };
            if let Err(error) = checked {
                return Err(format!(
                    "invalid value {} for flag -{name_text}: {}",
                    compat::quote(&bytes),
                    error.reason()
                ));
            }
        }
        values.push((primary, value));
        index += 1;
    }
    Ok(Outcome::Parsed(Parsed {
        values,
        positionals: Vec::new(),
    }))
}

impl Parsed {
    /// Reports whether the flag appeared.
    pub fn is_set(&self, name: &str) -> bool {
        self.values.iter().any(|(flag, _)| *flag == name)
    }

    /// The last value given for a flag.
    pub fn value(&self, name: &str) -> Option<&OsStr> {
        self.values
            .iter()
            .rev()
            .find(|(flag, _)| *flag == name)
            .map(|(_, value)| value.as_os_str())
    }

    /// The last value of a string flag as UTF-8, replacing invalid bytes.
    pub fn string(&self, name: &str) -> Option<String> {
        self.value(name)
            .map(|value| value.to_string_lossy().into_owned())
    }

    /// Every value given for a repeatable flag, in order.
    pub fn strings(&self, name: &str) -> Vec<OsString> {
        self.values
            .iter()
            .filter(|(flag, _)| *flag == name)
            .map(|(_, value)| value.clone())
            .collect()
    }

    /// The last value of a boolean flag, or false.
    pub fn bool(&self, name: &str) -> bool {
        self.value(name).is_some_and(|value| value == "true")
    }

    /// The last value of an integer flag, already validated by [`parse`].
    pub fn int(&self, name: &str) -> Option<i64> {
        self.value(name)
            .and_then(|value| value.to_str())
            .and_then(|value| compat::parse_int(value).ok())
    }

    /// The last value of an unsigned integer flag, already validated by [`parse`].
    pub fn uint(&self, name: &str) -> Option<u64> {
        self.value(name)
            .and_then(|value| value.to_str())
            .and_then(|value| compat::parse_uint(value).ok())
    }
}

/// Renders a command's help as the reference's `urfave/cli` template does.
pub fn help(spec: &Spec) -> String {
    let mut text = format!("NAME:\n   vibes {} - {}\n\nUSAGE:\n", spec.name, spec.usage);
    if spec.usage_lines.is_empty() {
        text.push_str(&format!("   vibes {} [options]", spec.name));
        if !spec.arguments.is_empty() {
            text.push(' ');
            text.push_str(spec.arguments);
        }
        text.push('\n');
    } else {
        for line in spec.usage_lines {
            text.push_str(&format!("   {line}\n"));
        }
    }
    text.push_str("\nOPTIONS:\n");
    let rows: Vec<(String, String)> = spec
        .flags
        .iter()
        .chain([&HELP])
        .map(|flag| (flag_names(flag), flag_usage(flag)))
        .collect();
    text.push_str(&table(&rows));
    text
}

/// Renders rows as `urfave/cli` does: three spaces of indent and two spaces
/// after the widest first column.
pub fn table(rows: &[(String, String)]) -> String {
    let width = rows.iter().map(|(left, _)| left.len()).max().unwrap_or(0);
    rows.iter()
        .map(|(left, right)| format!("   {left:<width$}  {right}\n"))
        .collect()
}

fn flag_names(flag: &Flag) -> String {
    let placeholder = match flag.kind {
        Kind::Bool => "",
        Kind::String | Kind::Strings => " string",
        Kind::Int => " int",
        Kind::Uint => " uint",
    };
    let spell = |name: &str| {
        let dashes = if name.chars().count() == 1 { "-" } else { "--" };
        format!("{dashes}{name}{placeholder}")
    };
    let mut names: Vec<String> = flag.names.iter().map(|name| spell(name)).collect();
    if flag.kind == Kind::Strings {
        names[0] = format!("{} [ {} ]", names[0], names[0]);
    }
    names.join(", ")
}

fn flag_usage(flag: &Flag) -> String {
    match flag.default {
        Some(default) => format!(
            "{} (default: {})",
            flag.usage,
            compat::quote(default.as_bytes())
        ),
        None => flag.usage.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAGS: &[Flag] = &[
        Flag::new(&["w"], Kind::Bool, "write"),
        Flag::new(&["function"], Kind::String, "function"),
        Flag::new(&["module-path"], Kind::Strings, "module"),
        Flag::new(&["step-quota"], Kind::Int, "steps"),
    ];
    const SPEC: Spec = Spec {
        name: "demo",
        aliases: &[],
        usage: "demo command",
        arguments: "<path>",
        usage_lines: &[],
        flags: FLAGS,
    };

    fn parse_strs(args: &[&str]) -> Result<Outcome, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        parse(&SPEC, &args)
    }

    fn parsed(args: &[&str]) -> Parsed {
        match parse_strs(args) {
            Ok(Outcome::Parsed(parsed)) => parsed,
            Ok(Outcome::Help) => panic!("help for {args:?}"),
            Err(error) => panic!("{args:?}: {error}"),
        }
    }

    fn error(args: &[&str]) -> String {
        match parse_strs(args) {
            Err(error) => error,
            Ok(_) => panic!("{args:?} parsed"),
        }
    }

    #[test]
    fn stops_at_the_first_positional_and_keeps_the_tail_verbatim() {
        let flags = parsed(&["-w", "--function=f", "file", "-w", "--", "-x"]);
        assert!(flags.bool("w"));
        assert_eq!(flags.string("function").as_deref(), Some("f"));
        assert_eq!(flags.positionals, ["file", "-w", "--", "-x"]);
        let flags = parsed(&["--", "-w", "x"]);
        assert!(!flags.bool("w"));
        assert_eq!(flags.positionals, ["-w", "x"]);
        for first in ["", "-", " -w"] {
            assert_eq!(parsed(&[first, "-w"]).positionals, [first, "-w"]);
        }
    }

    #[test]
    fn values_follow_or_attach_and_repeat() {
        let flags = parsed(&[
            "-module-path",
            "a",
            "--module-path=b,c",
            "-step-quota",
            "-1",
            "-function",
            "-w",
        ]);
        assert_eq!(flags.strings("module-path"), ["a", "b,c"]);
        assert_eq!(flags.int("step-quota"), Some(-1));
        assert_eq!(flags.string("function").as_deref(), Some("-w"));
        assert!(!flags.bool("w"));
        let flags = parsed(&["-w=false", "-w=T"]);
        assert!(flags.bool("w"));
        assert!(!parsed(&["-w=0"]).bool("w"));
    }

    #[test]
    fn errors_use_the_reference_wording() {
        for (args, message) in [
            (&["-unknown"][..], "flag provided but not defined: -unknown"),
            (
                &["--unknown=value"],
                "flag provided but not defined: -unknown",
            ),
            (&["--function"], "flag needs an argument: -function"),
            (&["---w"], "bad flag syntax: ---w"),
            (&["--=value"], "bad flag syntax: --=value"),
            (&["-=value"], "bad flag syntax: -=value"),
            (&["-w "], "flag provided but not defined: -w "),
            (
                &["-w=true "],
                "invalid boolean value \"true \" for -w: parse error",
            ),
            (&["-w="], "invalid boolean value \"\" for -w: parse error"),
            (&["-h=false"], "help flag does not accept a value"),
            (
                &["-step-quota=nope", "-unknown"],
                "invalid value \"nope\" for flag -step-quota: parse error",
            ),
            (
                &["-step-quota", "99999999999999999999"],
                "invalid value \"99999999999999999999\" for flag -step-quota: value out of range",
            ),
            (&["-1"], "flag provided but not defined: -1"),
        ] {
            assert_eq!(error(args), message, "{args:?}");
        }
    }

    #[test]
    fn bare_help_short_circuits_later_flags_but_not_earlier_errors() {
        assert!(matches!(parse_strs(&["-h", "-bogus"]), Ok(Outcome::Help)));
        assert!(matches!(parse_strs(&["-w", "--help"]), Ok(Outcome::Help)));
        assert!(parse_strs(&["-w=bogus", "--help"]).is_err());
    }

    #[test]
    fn help_matches_the_reference_layout() {
        let pad = |left: &str, right: &str| format!("   {left:<45}  {right}\n");
        let expected = "NAME:\n   vibes demo - demo command\n\nUSAGE:\n   vibes demo [options] <path>\n\nOPTIONS:\n"
            .to_owned()
            + &pad("-w", "write")
            + &pad("--function string", "function")
            + &pad("--module-path string [ --module-path string ]", "module")
            + &pad("--step-quota int", "steps")
            + &pad("--help, -h", "show help");
        assert_eq!(help(&SPEC), expected);
    }
}
