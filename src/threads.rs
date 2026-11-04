//! The crate's threads. Recursion that needs more native stack than its
//! caller has runs on a thread with a chosen stack, and tests run work on
//! threads beside their own. Every thread is joined before the function
//! that started it returns, and a panic in one resumes in the caller.
//!
//! WASI preview 1 has no threads, so there every function runs its work on
//! the calling thread, one closure after another.

use std::{io, panic::resume_unwind, thread};

/// Runs `work` on a thread with `bytes` of native stack and waits for it.
/// An error means the thread could not start and `work` did not run.
#[allow(
    clippy::disallowed_methods,
    reason = "the crate's one scoped thread: it is joined before this returns, and the synchronous core has no async runtime it could block"
)]
pub(crate) fn on_stack<T: Send>(bytes: usize, work: impl FnOnce() -> T + Send) -> io::Result<T> {
    if cfg!(target_os = "wasi") {
        return Ok(work());
    }
    thread::scope(|scope| {
        let thread = thread::Builder::new()
            .stack_size(bytes)
            .spawn_scoped(scope, work)?;
        Ok(thread.join().unwrap_or_else(|panic| resume_unwind(panic)))
    })
}

/// Runs `worker` on another thread while `body` runs on this one, then
/// waits for the worker, also when `body` panics. A body whose worker waits
/// on it should own what the worker waits on, so unwinding releases the
/// worker instead of blocking the join. On WASI the worker runs first, so
/// the two must not wait for each other there.
#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's scoped worker: it is joined before this returns, and tests have no async runtime to block"
)]
pub(crate) fn alongside<W: Send, B>(
    worker: impl FnOnce() -> W + Send,
    body: impl FnOnce() -> B,
) -> (W, B) {
    if cfg!(target_os = "wasi") {
        return (worker(), body());
    }
    thread::scope(|scope| {
        let worker = scope.spawn(worker);
        let body = body();
        (
            worker.join().unwrap_or_else(|panic| resume_unwind(panic)),
            body,
        )
    })
}

/// Runs `work` on another thread and waits for it.
#[cfg(test)]
pub(crate) fn elsewhere<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    alongside(work, || ()).0
}

/// Runs every job on its own thread and returns their results in order,
/// once all have finished. On WASI the jobs run one after another, so jobs
/// that wait for each other must not run there.
#[cfg(test)]
#[allow(
    clippy::disallowed_methods,
    reason = "a test's scoped threads: all are joined before this returns, and tests have no async runtime to block"
)]
pub(crate) fn parallel<T: Send>(
    jobs: impl IntoIterator<Item = impl FnOnce() -> T + Send>,
) -> Vec<T> {
    if cfg!(target_os = "wasi") {
        return jobs.into_iter().map(|job| job()).collect();
    }
    thread::scope(|scope| {
        let threads: Vec<_> = jobs.into_iter().map(|job| scope.spawn(job)).collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap_or_else(|panic| resume_unwind(panic)))
            .collect()
    })
}
