//! Output destinations shared by script writers and command messages.

use crate::compat;
use std::{
    io::{self, Write},
    sync::mpsc,
};

/// Where a command writes: a process stream, or a buffer in tests.
#[derive(Clone)]
pub enum Sink {
    Stdout,
    Stderr,
    /// Only the watch tests, which WASI cannot run, capture output: each
    /// write is sent whole to the test's [`Captured`].
    #[cfg_attr(not(all(test, not(target_os = "wasi"))), allow(dead_code))]
    Buffer(mpsc::Sender<Vec<u8>>),
}

/// Output captured by a test, which can wait for what it expects to appear.
#[cfg(all(test, not(target_os = "wasi")))]
pub struct Captured {
    written: mpsc::Receiver<Vec<u8>>,
    bytes: Vec<u8>,
}

#[cfg(all(test, not(target_os = "wasi")))]
impl Captured {
    /// What has been written so far, as text.
    pub fn text(&mut self) -> String {
        self.bytes.extend(self.written.try_iter().flatten());
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    /// Waits until `expected` holds for the text written so far, or until
    /// `timeout` passes, and reports which.
    pub fn wait_for(
        &mut self,
        timeout: std::time::Duration,
        expected: impl Fn(&str) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if expected(&self.text()) {
                return true;
            }
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return false;
            };
            match self.written.recv_timeout(left) {
                Ok(written) => self.bytes.extend(written),
                Err(mpsc::RecvTimeoutError::Timeout) => return false,
                Err(mpsc::RecvTimeoutError::Disconnected) => return expected(&self.text()),
            }
        }
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
                // A test that stopped reading has finished with the output.
                if !bytes.is_empty() {
                    let _ = captured.send(bytes.to_vec());
                }
                Ok(())
            }
        };
        result.map_err(|error| compat::reason(&error))
    }

    /// Writes a line, ignoring failures, for status and error reports.
    pub fn line(&self, text: &str) {
        let _ = self.write(format!("{text}\n").as_bytes());
    }

    /// A new buffer sink and the capture that reads it.
    #[cfg(all(test, not(target_os = "wasi")))]
    pub fn buffer() -> (Self, Captured) {
        let (sender, written) = mpsc::channel();
        (
            Self::Buffer(sender),
            Captured {
                written,
                bytes: Vec::new(),
            },
        )
    }
}
