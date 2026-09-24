//! The Content-Length-framed message loop over a byte stream pair.

use super::server::{InFlight, Inbound, Outbound, Server, change_params};
use super::transport::{self, Frame};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use vibescript::CancellationToken;

/// Serves `server` over Content-Length-framed JSON-RPC until the client sends
/// `exit`, the input ends, or `cancellation` is cancelled.
///
/// Messages that are not JSON-RPC objects and bodies over 8 MiB are skipped.
/// Corrupt framing, such as a missing or malformed `Content-Length`, ends the
/// loop with an error, since no later message boundary can be trusted.
///
/// Where threads are available, input is read ahead on a separate thread.
/// Cancellation then stops the loop even while it waits for input, a queued
/// request named by `$/cancelRequest` is answered with `-32800`, and a
/// document change queued right behind another change to the same document
/// replaces it unanalyzed. Without threads, as on WASI, messages are read and
/// handled strictly in turn.
///
/// ```
/// use std::io::Cursor;
/// use vibescript::CancellationToken;
/// use vibescript_tools::lsp::{Server, serve};
///
/// let body = r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#;
/// let input = format!("Content-Length: {}\r\n\r\n{body}", body.len());
/// let mut output = Vec::new();
/// serve(&mut Server::new(), Cursor::new(input), &mut output, &CancellationToken::new())?;
/// let reply = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
/// assert_eq!(output, format!("Content-Length: {}\r\n\r\n{reply}", reply.len()).as_bytes());
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn serve(
    server: &mut Server,
    input: impl Read + Send + 'static,
    mut output: impl Write,
    cancellation: &CancellationToken,
) -> io::Result<()> {
    let mut messages = Messages::new(input, server.in_flight.clone());
    while let Some(event) = messages.next(cancellation)? {
        let Some(message) = event else {
            continue;
        };
        if messages.superseded(&message) {
            continue;
        }
        let replies = match messages.cancelled(&message) {
            Some(id) => vec![Outbound::Error {
                id,
                code: -32800,
                message: "request cancelled",
            }],
            None => server.dispatch(&message),
        };
        for reply in replies {
            transport::write(&mut output, &reply.json())?;
        }
        if server.exit_requested() {
            return Ok(());
        }
    }
    Ok(())
}

/// A decoded message, `None` for a skipped one, or a read failure.
type Event = io::Result<Option<Inbound>>;

/// Where input events come from.
enum Source<R> {
    /// Read on this thread, where threads are unavailable.
    Inline(BufReader<R>),
    /// Read ahead on a reader thread.
    Threaded {
        receiver: mpsc::Receiver<Event>,
        queue: VecDeque<Event>,
    },
}

struct Messages<R> {
    source: Source<R>,
}

/// How many decoded messages the reader thread may hold ahead of the server.
const READ_AHEAD: usize = 64;

impl<R: Read + Send + 'static> Messages<R> {
    fn new(input: R, in_flight: InFlight) -> Self {
        let (sender, receiver) = mpsc::sync_channel(READ_AHEAD);
        let input = Arc::new(Mutex::new(Some(input)));
        let shared = input.clone();
        let spawned = std::thread::Builder::new()
            .name("vibes-lsp-reader".to_owned())
            .spawn(move || {
                let taken = shared.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(input) = taken {
                    read_ahead(BufReader::new(input), &sender, &in_flight);
                }
            });
        let source = match spawned {
            Ok(_) => Source::Threaded {
                receiver,
                queue: VecDeque::new(),
            },
            Err(_) => {
                let input = input.lock().unwrap_or_else(|e| e.into_inner()).take();
                Source::Inline(BufReader::new(
                    input.expect("the reader thread never started"),
                ))
            }
        };
        Self { source }
    }
}

/// Reads and decodes messages ahead of the server. A change or close for the
/// document being analyzed cancels that analysis, since its result would be
/// superseded before it could be published.
fn read_ahead<R: Read>(
    mut reader: BufReader<R>,
    sender: &mpsc::SyncSender<Event>,
    in_flight: &InFlight,
) {
    loop {
        let event = match read_message(&mut reader) {
            Ok(Some(message)) => {
                if let Some(message) = &message {
                    supersede(message, in_flight);
                }
                Ok(message)
            }
            Ok(None) => return,
            Err(error) => Err(error),
        };
        let failed = event.is_err();
        if sender.send(event).is_err() || failed {
            return;
        }
    }
}

fn supersede(message: &Inbound, in_flight: &InFlight) {
    if !matches!(
        message.method.as_str(),
        "textDocument/didChange" | "textDocument/didClose"
    ) {
        return;
    }
    let Some(uri) = message.document() else {
        return;
    };
    let in_flight = in_flight.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((current, token)) = in_flight.as_ref() {
        if uri == *current {
            token.cancel();
        }
    }
}

/// Reads one frame: a decoded message, `Some(None)` for a skipped one, or
/// `None` at the end of input.
fn read_message(reader: &mut impl BufRead) -> io::Result<Option<Option<Inbound>>> {
    match transport::read(reader)? {
        Frame::End => Ok(None),
        Frame::Oversized(_) => Ok(Some(None)),
        Frame::Message(payload) => Ok(Some(Inbound::parse(&payload))),
    }
}

impl<R: Read> Messages<R> {
    /// The next event in input order, waiting for input; `None` at the end
    /// of input or on cancellation.
    fn next(&mut self, cancellation: &CancellationToken) -> io::Result<Option<Option<Inbound>>> {
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        let (receiver, queue) = match &mut self.source {
            Source::Inline(reader) => return read_message(reader),
            Source::Threaded { receiver, queue } => (receiver, queue),
        };
        let event = match queue.pop_front() {
            Some(event) => event,
            None => loop {
                match receiver.recv_timeout(Duration::from_millis(50)) {
                    Ok(event) => break event,
                    Err(mpsc::RecvTimeoutError::Timeout) if cancellation.is_cancelled() => {
                        return Ok(None);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
                }
            },
        };
        // Queue what has already arrived, so the server can see what follows.
        queue.extend(receiver.try_iter());
        event.map(Some)
    }

    /// Whether a full-text change is already replaced by the change queued
    /// right after it, so analyzing it would be wasted.
    fn superseded(&self, message: &Inbound) -> bool {
        let Source::Threaded { queue, .. } = &self.source else {
            return false;
        };
        let Some(Ok(Some(next))) = queue.front() else {
            return false;
        };
        message.method == "textDocument/didChange"
            && next.method == "textDocument/didChange"
            && change_params(next).is_ok_and(|(_, text)| text.is_some())
            && message
                .document()
                .is_some_and(|uri| next.document() == Some(uri))
    }

    /// The id of a request that a queued `$/cancelRequest` names, which is
    /// then answered as cancelled instead of handled.
    fn cancelled(&mut self, message: &Inbound) -> Option<Box<str>> {
        let Source::Threaded { queue, .. } = &mut self.source else {
            return None;
        };
        let id = message.id.as_ref()?;
        let wanted: serde_json::Value = serde_json::from_str(id).ok()?;
        let position = queue.iter().position(|event| {
            let Ok(Some(queued)) = event else {
                return false;
            };
            queued.method == "$/cancelRequest"
                && super::json::params(queued.params.as_deref())
                    .ok()
                    .and_then(|params| {
                        super::json::root(&params)
                            .and_then(|root| super::json::field(root, "id"))
                            .cloned()
                    })
                    .is_some_and(|cancelled| cancelled == wanted)
        })?;
        queue.remove(position);
        Some(id.clone())
    }
}
