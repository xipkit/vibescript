//! Named quota profiles and the flags that select and override them.
//!
//! The values and conventions follow the Go reference: `run`, `test` and
//! `repl` default to `xhigh`, an override of zero selects the engine default
//! (the `low` profile's value), a negative override disables the quota, and
//! a positive override is used as-is.

use crate::{
    compat,
    flags::{Flag, Kind, Parsed},
};
use vibescript::Limits;

/// A coherent bundle of the three execution quotas; `-1` means unlimited.
pub struct Profile {
    pub name: &'static str,
    pub steps: i64,
    pub memory: i64,
    pub recursion: i64,
}

/// Every profile, in ascending order of generosity.
pub const PROFILES: [Profile; 4] = [
    Profile {
        name: "low",
        steps: 1_000_000,
        memory: 16 << 20,
        recursion: 256,
    },
    Profile {
        name: "medium",
        steps: 20_000_000,
        memory: 128 << 20,
        recursion: 1_000,
    },
    Profile {
        name: "high",
        steps: 200_000_000,
        memory: 512 << 20,
        recursion: 4_000,
    },
    Profile {
        name: "xhigh",
        steps: -1,
        memory: -1,
        recursion: 10_000,
    },
];

/// The profile the execution commands select without `-profile`.
pub const DEFAULT: &str = "xhigh";

/// The flags every execution command accepts, after its own.
pub const FLAGS: [Flag; 4] = [
    Flag {
        names: &["profile"],
        kind: Kind::String,
        usage: "execution quota profile: low, medium, high, xhigh",
        default: Some(DEFAULT),
    },
    Flag::new(
        &["step-quota"],
        Kind::Int,
        "override the profile's step quota (-1 = unlimited)",
    ),
    Flag::new(
        &["memory-quota"],
        Kind::Int,
        "override the profile's memory quota in bytes (-1 = unlimited)",
    ),
    Flag::new(
        &["recursion-limit"],
        Kind::Int,
        "override the profile's recursion limit (-1 = unlimited, which can crash on infinite recursion)",
    ),
];

/// Looks up a profile by name, ignoring case and surrounding whitespace.
pub fn by_name(name: &str) -> Option<&'static Profile> {
    let name = compat::trim_space(name).to_lowercase();
    PROFILES.iter().find(|profile| profile.name == name)
}

/// Resolves `-profile` and the override flags into execution limits.
pub fn resolve(flags: &Parsed) -> Result<Limits, String> {
    let name = flags
        .string("profile")
        .unwrap_or_else(|| DEFAULT.to_owned());
    let Some(profile) = by_name(&name) else {
        let names: Vec<_> = PROFILES.iter().map(|profile| profile.name).collect();
        return Err(format!(
            "unknown quota profile {} (choose one of: {})",
            compat::quote(name.as_bytes()),
            names.join(", ")
        ));
    };
    Ok(limits(
        flags.int("step-quota").unwrap_or(profile.steps),
        flags.int("memory-quota").unwrap_or(profile.memory),
        flags.int("recursion-limit").unwrap_or(profile.recursion),
    ))
}

/// Maps Go-style quota values onto library limits.
pub fn limits(steps: i64, memory: i64, recursion: i64) -> Limits {
    let low = &PROFILES[0];
    let quota = |value: i64, default: i64| match value {
        0 => Some(default as u64),
        value if value < 0 => None,
        value => Some(value as u64),
    };
    Limits {
        steps: quota(steps, low.steps),
        memory_bytes: quota(memory, low.memory)
            .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX)),
        recursion: quota(recursion, low.recursion).map_or(usize::MAX, |depth| {
            usize::try_from(depth).unwrap_or(usize::MAX)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flags::{Outcome, Spec, parse};
    use std::ffi::OsString;

    const SPEC: Spec = Spec {
        name: "quota-test",
        aliases: &[],
        usage: "",
        arguments: "",
        usage_lines: &[],
        flags: &FLAGS,
    };

    fn resolved(args: &[&str]) -> Result<Limits, String> {
        let args: Vec<OsString> = args.iter().map(OsString::from).collect();
        match parse(&SPEC, &args)? {
            Outcome::Parsed(parsed) => resolve(&parsed),
            Outcome::Help => panic!("help"),
        }
    }

    fn triple(limits: &Limits) -> (Option<u64>, Option<usize>, usize) {
        (limits.steps, limits.memory_bytes, limits.recursion)
    }

    #[test]
    fn defaults_to_xhigh() {
        assert_eq!(triple(&resolved(&[]).unwrap()), (None, None, 10_000));
    }

    #[test]
    fn selects_profiles_by_name() {
        assert_eq!(
            triple(&resolved(&["-profile", "low"]).unwrap()),
            (Some(1_000_000), Some(16 << 20), 256)
        );
        assert_eq!(
            triple(&resolved(&["-profile", " MEDIUM "]).unwrap()),
            (Some(20_000_000), Some(128 << 20), 1_000)
        );
        assert_eq!(
            triple(&resolved(&["--profile=high"]).unwrap()),
            (Some(200_000_000), Some(512 << 20), 4_000)
        );
    }

    #[test]
    fn overrides_layer_on_the_profile() {
        assert_eq!(
            triple(
                &resolved(&[
                    "-profile",
                    "low",
                    "-step-quota",
                    "-1",
                    "-recursion-limit",
                    "5000"
                ])
                .unwrap()
            ),
            (None, Some(16 << 20), 5000)
        );
        assert_eq!(
            triple(
                &resolved(&["-step-quota=0", "-memory-quota", "0", "-recursion-limit=0"]).unwrap()
            ),
            (Some(1_000_000), Some(16 << 20), 256)
        );
        assert_eq!(
            triple(&resolved(&["-recursion-limit=-1"]).unwrap()).2,
            usize::MAX
        );
    }

    #[test]
    fn rejects_unknown_profiles() {
        assert_eq!(
            resolved(&["-profile", "gigantic"]).unwrap_err(),
            "unknown quota profile \"gigantic\" (choose one of: low, medium, high, xhigh)"
        );
    }
}
