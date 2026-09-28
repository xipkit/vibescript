//! Interrupt handling: the first ctrl-c cancels running scripts and stops
//! watch mode; a second one terminates the process, as in the reference.

use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
use vibescript::CancellationToken;

static TOKEN: OnceLock<CancellationToken> = OnceLock::new();
static STOPPED: AtomicBool = AtomicBool::new(false);

/// Installs the interrupt handler. Platforms without signals keep the default.
pub fn install() {
    TOKEN.get_or_init(CancellationToken::new);
    #[cfg(unix)]
    {
        let handler = interrupted as extern "C" fn(libc::c_int);
        // SAFETY: the handler only performs atomic stores and restores the
        // default disposition, all of which are async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        }
    }
}

#[cfg(unix)]
extern "C" fn interrupted(_: libc::c_int) {
    STOPPED.store(true, Ordering::SeqCst);
    if let Some(token) = TOKEN.get() {
        token.cancel();
    }
    // SAFETY: restoring the default disposition is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
}

/// The token every script call observes, cancelled by an interrupt.
pub fn token() -> CancellationToken {
    TOKEN.get_or_init(CancellationToken::new).clone()
}

/// Reports whether an interrupt arrived.
pub fn stop_requested() -> bool {
    STOPPED.load(Ordering::SeqCst)
}
