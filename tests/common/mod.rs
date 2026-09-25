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

/// An engine for tests of the gradual checker (`Script::check` and its
/// relatives), which reads the ADR-004 language until it is removed, so it
/// does not type check statically even when the build forces static types.
pub fn gradual_engine() -> vibescript::Engine {
    let mut engine = vibescript::Engine::new();
    engine.set_static_types(false);
    engine
}

/// An engine that type checks statically whatever the build's default, for
/// tests of compile-time diagnostics.
pub fn static_engine() -> vibescript::Engine {
    let mut engine = vibescript::Engine::new();
    engine.set_static_types(true);
    engine
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
