//! Output destinations shared by script writers and command messages.

use crate::compat;
use std::{
    io::{self, Write},
    sync::Arc,
};

/// Where a command writes: a process stream, or a buffer in tests.
#[derive(Clone)]
pub enum Sink {
    Stdout,
    Stderr,
    /// Only the watch tests, which WASI cannot run, capture output.
    #[cfg_attr(not(all(test, not(target_os = "wasi"))), allow(dead_code))]
    Buffer(Arc<Captured>),
}

/// Output captured by a test, which can wait for what it expects to appear.
pub struct Captured {
    bytes: std::sync::Mutex<Vec<u8>>,
    written: std::sync::Condvar,
}

impl Captured {
    fn append(&self, bytes: &[u8]) {
        self.bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(bytes);
        self.written.notify_all();
    }

    /// What has been written so far, as text.
    #[cfg(all(test, not(target_os = "wasi")))]
    pub fn text(&self) -> String {
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Waits until `expected` holds for the text written so far, or until
    /// `timeout` passes, and reports which.
    #[cfg(all(test, not(target_os = "wasi")))]
    pub fn wait_for(&self, timeout: std::time::Duration, expected: impl Fn(&str) -> bool) -> bool {
        let bytes = self
            .bytes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_bytes, waited) = self
            .written
            .wait_timeout_while(bytes, timeout, |bytes| {
                !expected(&String::from_utf8_lossy(bytes))
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !waited.timed_out()
    }
}

impl Sink {
    /// Writes and flushes all bytes, reporting failures in Go's wording.
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        let result = match self {
            Self::Stdout => {
                let mut out = io::stdout().lock();
                out.write_all(bytes).and_then(|()| out.flush())
            }
            Self::Stderr => {
                let mut out = io::stderr().lock();
                out.write_all(bytes).and_then(|()| out.flush())
            }
            Self::Buffer(captured) => {
                captured.append(bytes);
                Ok(())
            }
        };
        result.map_err(|error| compat::reason(&error))
    }

    /// Writes a line, ignoring failures, for status and error reports.
    pub fn line(&self, text: &str) {
        let _ = self.write(format!("{text}\n").as_bytes());
    }

    /// A new buffer sink and a handle to read it.
    #[cfg(all(test, not(target_os = "wasi")))]
    pub fn buffer() -> (Self, Arc<Captured>) {
        let captured = Arc::new(Captured {
            bytes: std::sync::Mutex::new(Vec::new()),
            written: std::sync::Condvar::new(),
        });
        (Self::Buffer(captured.clone()), captured)
    }
}
