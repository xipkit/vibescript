//! Shared state whose methods run one at a time: adamantium's `Actor`, kept
//! here with the same shape because adamantium needs Tokio unconditionally
//! and this crate keeps Tokio optional for WASI.

use std::{fmt, sync::PoisonError};

/// Shared state whose methods run one at a time.
///
/// [`with`](Self::with) runs a closure with exclusive access to the state,
/// and [`read`](Self::read) with shared access. The closure is synchronous,
/// so the state is never held across an await, and it can't enter an actor:
/// a nested `with` or `read`, on this actor or another, panics instead of
/// risking a deadlock. Those two rules keep the crate free of lock deadlocks
/// and of locks held through a cancellation. A panic inside the closure
/// leaves the state as the closure left it, and the next caller sees it.
///
/// Write the state as a plain struct with plain methods, and call them
/// through `with`; share the actor itself through an `Arc` where several
/// holders need it.
pub(crate) struct Actor<S> {
    #[allow(
        clippy::disallowed_types,
        reason = "the one lock in the crate, held only inside `with` and `read`"
    )]
    state: std::sync::Mutex<S>,
}

thread_local! {
    /// Whether the current thread is inside an actor method. A method is
    /// synchronous, so a task can't move threads while it runs one.
    static INSIDE: crate::compilation::Shared<bool> =
        const { crate::compilation::Shared::new(false) };
}

/// Marks the current thread as inside an actor method until dropped.
struct Inside;

impl Inside {
    #[allow(
        clippy::panic,
        reason = "a nested actor call is a bug in the caller, and a panic names it where a deadlock would hang"
    )]
    fn enter() -> Self {
        INSIDE.with(|inside| {
            if inside.replace(true) {
                panic!(
                    "an actor method was called inside another actor's method; \
                     an actor method can't call an actor, so call the second \
                     actor after the first returns"
                );
            }
        });
        Self
    }
}

impl Drop for Inside {
    fn drop(&mut self) {
        let _ = INSIDE.try_with(|inside| inside.replace(false));
    }
}

impl<S> Actor<S> {
    /// An actor holding `state`.
    #[allow(clippy::disallowed_types, reason = "see the state field")]
    pub const fn new(state: S) -> Self {
        Self {
            state: std::sync::Mutex::new(state),
        }
    }

    /// Runs `f` with exclusive access to the state and returns its result.
    ///
    /// Other callers wait their turn. `f` can't await, because it is a
    /// plain closure, and it can't call `with` or `read` on any actor:
    /// doing so panics with a message that says which rule was broken.
    pub fn with<R>(&self, f: impl FnOnce(&mut S) -> R) -> R {
        let _inside = Inside::enter();
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut guard)
    }

    /// Runs `f` with shared access to the state and returns its result,
    /// under the same rules as [`with`](Self::with).
    pub fn read<R>(&self, f: impl FnOnce(&S) -> R) -> R {
        let _inside = Inside::enter();
        let guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        f(&guard)
    }

    /// Whether no method of this actor is running now.
    #[cfg(test)]
    pub fn idle(&self) -> bool {
        !matches!(
            self.state.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        )
    }
}

impl<S: Default> Default for Actor<S> {
    fn default() -> Self {
        Self::new(S::default())
    }
}

impl<S: fmt::Debug> fmt::Debug for Actor<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state.try_lock() {
            Ok(state) => f.debug_tuple("Actor").field(&*state).finish(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => f
                .debug_tuple("Actor")
                .field(&*poisoned.into_inner())
                .finish(),
            Err(std::sync::TryLockError::WouldBlock) => f.write_str("Actor(<in use>)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Actor;

    #[test]
    fn methods_run_one_at_a_time_and_hand_back_results() {
        let tally = Actor::new(0_u64);
        assert_eq!(
            tally.with(|hits| {
                *hits += 2;
                *hits
            }),
            2
        );
        assert_eq!(tally.read(|hits| *hits), 2);
        assert!(tally.idle());
    }

    #[test]
    #[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
    fn a_nested_actor_call_panics_and_frees_the_thread() {
        let outer = Actor::new(());
        let inner = Actor::new(());
        let nested = std::panic::catch_unwind(|| outer.with(|()| inner.read(|()| ())));
        assert!(nested.is_err());
        assert!(outer.idle());
        outer.with(|()| ());
    }

    #[test]
    #[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
    fn a_panic_inside_a_method_leaves_the_state_for_the_next_caller() {
        let tally = Actor::new(1_u64);
        let failed = std::panic::catch_unwind(|| {
            tally.with(|hits| {
                *hits = 5;
                if *hits == 5 {
                    std::panic::panic_any("inside");
                }
            })
        });
        assert!(failed.is_err());
        assert_eq!(tally.read(|hits| *hits), 5);
        assert_eq!(format!("{tally:?}"), "Actor(5)");
    }
}
