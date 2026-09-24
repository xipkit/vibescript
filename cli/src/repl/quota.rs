//! Quota profiles and the `-profile`, `-step-quota`, `-memory-quota` and
//! `-recursion-limit` flags, with the Go CLI's semantics: a negative value
//! disables a quota, zero selects the engine default and a positive value is an
//! explicit limit.

use vibescript::Limits;

/// The profile the REPL selects when `-profile` is absent.
pub const DEFAULT_PROFILE: &str = "xhigh";

/// A coherent bundle of the three execution quotas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Profile {
    pub name: &'static str,
    pub steps: i64,
    pub memory: i64,
    pub recursion: i64,
}

/// The profiles in ascending order of generosity, matching Go's table.
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

/// Finds a profile by name, ignoring case and surrounding whitespace.
pub fn profile(name: &str) -> Option<Profile> {
    let name = name.trim().to_ascii_lowercase();
    PROFILES.into_iter().find(|profile| profile.name == name)
}

/// Parsed quota flags; an override left unset keeps the profile's value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuotaFlags {
    pub profile: String,
    pub steps: Option<i64>,
    pub memory: Option<i64>,
    pub recursion: Option<i64>,
}

impl Default for QuotaFlags {
    fn default() -> Self {
        Self {
            profile: DEFAULT_PROFILE.to_owned(),
            steps: None,
            memory: None,
            recursion: None,
        }
    }
}

impl QuotaFlags {
    /// Selects the named profile and layers the explicit overrides on top.
    pub fn resolve(&self) -> Result<Limits, String> {
        let Some(profile) = profile(&self.profile) else {
            let names: Vec<_> = PROFILES.iter().map(|profile| profile.name).collect();
            return Err(format!(
                "unknown quota profile {:?} (choose one of: {})",
                self.profile,
                names.join(", ")
            ));
        };
        let defaults = Limits::default();
        let steps = self.steps.unwrap_or(profile.steps);
        let memory = self.memory.unwrap_or(profile.memory);
        let recursion = self.recursion.unwrap_or(profile.recursion);
        Ok(Limits {
            steps: resolve(steps, defaults.steps, |n| Some(n as u64)),
            memory_bytes: resolve(memory, defaults.memory_bytes, |n| {
                Some(usize::try_from(n).unwrap_or(usize::MAX))
            }),
            recursion: resolve(recursion, defaults.recursion, |n| {
                usize::try_from(n).unwrap_or(usize::MAX)
            }),
        })
    }
}

/// Maps a Go quota value: zero keeps the default, a negative value disables the
/// quota and a positive value is converted.
fn resolve<T: Unlimited>(value: i64, default: T, convert: impl Fn(i64) -> T) -> T {
    match value {
        0 => default,
        n if n < 0 => T::unlimited(),
        n => convert(n),
    }
}

trait Unlimited {
    fn unlimited() -> Self;
}

impl<T> Unlimited for Option<T> {
    fn unlimited() -> Self {
        None
    }
}

impl Unlimited for usize {
    fn unlimited() -> Self {
        usize::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(profile: &str) -> QuotaFlags {
        QuotaFlags {
            profile: profile.to_owned(),
            ..QuotaFlags::default()
        }
    }

    #[test]
    fn profiles_match_the_go_table() {
        let low = flags("low").resolve().unwrap();
        assert_eq!(low.steps, Some(1_000_000));
        assert_eq!(low.memory_bytes, Some(16 << 20));
        assert_eq!(low.recursion, 256);
        let xhigh = QuotaFlags::default().resolve().unwrap();
        assert_eq!(xhigh.steps, None);
        assert_eq!(xhigh.memory_bytes, None);
        assert_eq!(xhigh.recursion, 10_000);
        let high = flags(" HIGH ").resolve().unwrap();
        assert_eq!(high.steps, Some(200_000_000));
        assert_eq!(high.memory_bytes, Some(512 << 20));
        assert_eq!(high.recursion, 4_000);
        assert_eq!(profile("medium").unwrap().recursion, 1_000);
    }

    #[test]
    fn overrides_layer_on_the_profile() {
        let limits = QuotaFlags {
            profile: "xhigh".to_owned(),
            steps: Some(500),
            memory: Some(0),
            recursion: Some(-1),
        }
        .resolve()
        .unwrap();
        assert_eq!(limits.steps, Some(500));
        assert_eq!(limits.memory_bytes, Limits::default().memory_bytes);
        assert_eq!(limits.recursion, usize::MAX);
        let limits = QuotaFlags {
            steps: Some(-7),
            ..flags("low")
        }
        .resolve()
        .unwrap();
        assert_eq!(limits.steps, None);
        assert_eq!(limits.memory_bytes, Some(16 << 20));
    }

    #[test]
    fn unknown_profiles_name_the_choices() {
        assert_eq!(
            flags("bogus").resolve().unwrap_err(),
            "unknown quota profile \"bogus\" (choose one of: low, medium, high, xhigh)"
        );
    }
}
