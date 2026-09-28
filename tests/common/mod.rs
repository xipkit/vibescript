//! Helpers shared by integration tests. Each test crate uses a subset.
#![allow(dead_code)]

/// Distinguishes this process's fixture directories from those of other test
/// runs. WASI has no process IDs, so it uses the current time instead.
pub fn process_id() -> u128 {
    #[cfg(not(target_os = "wasi"))]
    return std::process::id().into();
    #[cfg(target_os = "wasi")]
    return std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
}

/// Runs `work` with a scope for concurrent calls, like [`std::thread::scope`].
#[cfg(not(target_os = "wasi"))]
pub fn scope<'env, T>(
    work: impl for<'scope> FnOnce(&'scope std::thread::Scope<'scope, 'env>) -> T,
) -> T {
    std::thread::scope(work)
}

/// WASI has no threads, so each spawned closure runs to completion when it is
/// spawned. Tests still check that calls sharing a script stay isolated, but
/// not that they can overlap.
#[cfg(target_os = "wasi")]
pub fn scope<T>(work: impl FnOnce(&Scope) -> T) -> T {
    work(&Scope)
}

#[cfg(target_os = "wasi")]
pub struct Scope;

#[cfg(target_os = "wasi")]
impl Scope {
    pub fn spawn<T>(&self, work: impl FnOnce() -> T) -> Finished<T> {
        Finished(work())
    }
}

/// The result of a closure that has already run.
#[cfg(target_os = "wasi")]
pub struct Finished<T>(T);

#[cfg(target_os = "wasi")]
impl<T> Finished<T> {
    pub fn join(self) -> std::thread::Result<T> {
        Ok(self.0)
    }
}

/// Whether `error` is the type checker's refusal, on WASI, of syntax taller
/// than it descends into there: WASI's default stack cannot hold its
/// recursion to the parser's full depth. Elsewhere it is never refused.
pub fn too_tall_for_wasi(error: &vibescript::Error) -> bool {
    cfg!(target_os = "wasi")
        && error.diagnostics().iter().any(|diagnostic| {
            diagnostic
                .message
                .starts_with("syntax nesting too deep to type check")
        })
}

/// The codes of the static diagnostics a failed compilation reports, such
/// as `V0201`, in source order.
pub fn codes(error: &vibescript::Error) -> Vec<String> {
    error
        .diagnostics()
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect()
}

/// An engine for a fixture case, or `None` for a case whose program must
/// fail to compile with the static diagnostic it records, which this checks.
pub fn fixture_engine(
    static_error: Option<&serde_json::Value>,
    source: &str,
    name: &str,
) -> Option<vibescript::Engine> {
    let engine = vibescript::Engine::new();
    let Some(expected) = static_error else {
        return Some(engine);
    };
    let error = engine
        .compile(source)
        .err()
        .unwrap_or_else(|| panic!("{name}: compiled with static types"));
    let first = error
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.is_error())
        .unwrap_or_else(|| panic!("{name}: {error}"));
    assert_eq!(first.code.to_string(), expected["code"], "{name}");
    None
}
