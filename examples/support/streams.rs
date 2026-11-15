//! A script's output streams, captured by the thread that runs it.

use std::sync::mpsc;
use vibescript::{Error, ErrorKind};

/// The end of an output stream a script's writer sends each write to, whole.
pub type Sink = mpsc::Sender<Vec<u8>>;

/// The end of an output stream that collects what was written.
pub type Written = mpsc::Receiver<Vec<u8>>;

/// A new output stream.
#[allow(
    clippy::disallowed_methods,
    reason = "a script's writer runs on the thread that collects its writes once the call returns, so a bounded stream would deadlock that thread or fail the script when full"
)]
pub fn open() -> (Sink, Written) {
    mpsc::channel()
}

/// Sends a nonempty write to its stream. Each stream's collector outlives
/// the calls that write to it, so a closed stream is a harness bug, and the
/// write fails with an internal error rather than losing the output.
pub fn write(sink: &Sink, bytes: &[u8]) -> vibescript::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    sink.send(bytes.to_vec()).map_err(|_| {
        Error::new(
            ErrorKind::Internal,
            "the output stream closed before its script",
        )
    })
}

/// Everything written to the stream so far, in order.
pub fn collect(written: &Written) -> Vec<u8> {
    written.try_iter().flatten().collect()
}
