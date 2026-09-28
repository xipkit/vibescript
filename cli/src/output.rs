//! Output destinations shared by script writers and command messages.

use crate::compat;
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

/// Where a command writes: a process stream, or a buffer in tests.
#[derive(Clone)]
pub enum Sink {
    Stdout,
    Stderr,
    /// Only the watch tests, which WASI cannot run, capture output.
    #[cfg_attr(not(all(test, not(target_os = "wasi"))), allow(dead_code))]
    Buffer(Arc<Mutex<Vec<u8>>>),
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
            Self::Buffer(buffer) => {
                buffer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .extend_from_slice(bytes);
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
    pub fn buffer() -> (Self, Arc<Mutex<Vec<u8>>>) {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        (Self::Buffer(buffer.clone()), buffer)
    }
}
