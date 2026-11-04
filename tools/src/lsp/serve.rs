//! The Content-Length-framed message loop over a byte stream pair.

use super::server::{Inbound, Outbound, Server, change_params};
use super::transport::{self, Frame};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::time::Duration;
use vibescript::CancellationToken;

/// Serves `server` over Content-Length-framed JSON-RPC until the client sends
/// `exit`, the input ends, or `cancellation` is cancelled.
///
/// Messages that are not JSON-RPC objects and bodies over 8 MiB are skipped.
/// Corrupt framing, such as a missing or malformed `Content-Length`, ends the
/// loop with an error, since no later message boundary can be trusted. Errors
/// read as the reference's do, such as `lsp read: missing Content-Length
/// header` or `lsp write: ...`.
///
/// Where threads are available, input is read ahead on a separate thread.
/// Cancellation then stops the loop even while it waits for input, a queued
/// request named by `$/cancelRequest` is answered with `-32800`, and a
/// document change queued right behind another change to the same document
/// replaces it unanalyzed. Without threads, as on WASI, messages are read and
/// handled strictly in turn. After the loop returns, the reader thread ends
/// when its input does.
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
    let mut messages = Messages::new(input, server.cancellation().clone())?;
    while let Some(event) = messages
        .next(cancellation)
        .map_err(|error| context("lsp read", error))?
    {
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
            transport::write(&mut output, &reply.json())
                .map_err(|error| context("lsp write", error))?;
        }
        if server.exit_requested() {
            return Ok(());
        }
    }
    Ok(())
}

/// Prefixes an error as the reference does, keeping its kind.
fn context(stage: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{stage}: {error}"))
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

/// How many decoded messages the reader thread may hold ahead of the server,
/// which bounds the memory a client flooding large changes can pin.
const READ_AHEAD: usize = 16;

impl<R: Read + Send + 'static> Messages<R> {
    #[allow(
        clippy::disallowed_methods,
        reason = "the reader blocks in read until the client closes its end, so the server cannot join it; it ends with its input, and the server has no async runtime"
    )]
    fn new(input: R, cancellation: CancellationToken) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(READ_AHEAD);
        // The input is handed over once the thread runs, so a thread that
        // cannot start leaves it to be read here.
        let (hand, taken) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("vibes-lsp-reader".to_owned())
            .spawn(move || {
                if let Ok(input) = taken.recv() {
                    read_ahead(BufReader::new(input), &sender, &cancellation);
                }
            });
        let source = match spawned {
            Ok(_) => {
                hand.send(input)
                    .map_err(|_| io::Error::other("reader thread ended before taking its input"))?;
                Source::Threaded {
                    receiver,
                    queue: VecDeque::new(),
                }
            }
            Err(_) => Source::Inline(BufReader::new(input)),
        };
        Ok(Self { source })
    }
}

/// Reads and decodes messages ahead of the server. Each document change
/// carries a token of `cancellation` that the server analyzes it under, and
/// a change or close for the same document cancels the analysis of the
/// version before it, since its result would be superseded before it could
/// be published.
fn read_ahead<R: Read>(
    mut reader: BufReader<R>,
    sender: &mpsc::SyncSender<Event>,
    cancellation: &CancellationToken,
) {
    let mut analyzing: Option<(String, CancellationToken)> = None;
    loop {
        let event = match read_message(&mut reader) {
            Ok(Some(mut message)) => {
                if let Some(message) = &mut message {
                    supersede(message, &mut analyzing, cancellation);
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

/// Cancels the analysis of the version of a document that `message`
/// changes or closes, and gives a change the token its own analysis runs
/// under, remembered in `analyzing` for the next change to cancel.
fn supersede(
    message: &mut Inbound,
    analyzing: &mut Option<(String, CancellationToken)>,
    cancellation: &CancellationToken,
) {
    let changes = matches!(
        message.method.as_str(),
        "textDocument/didChange" | "textDocument/didClose"
    );
    let opens = matches!(
        message.method.as_str(),
        "textDocument/didOpen" | "textDocument/didChange"
    );
    if !changes && !opens {
        return;
    }
    let Some(uri) = message.document() else {
        return;
    };
    if changes
        && let Some((current, token)) = analyzing.as_ref()
        && uri == *current
    {
        token.cancel();
    }
    if opens {
        let token = cancellation.child_token();
        message.token = Some(token.clone());
        *analyzing = Some((uri, token));
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
