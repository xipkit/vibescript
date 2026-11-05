//! The Content-Length-framed message loop over a byte stream pair.

use super::server::{Inbound, Outbound, Server, change_params};
use super::transport::{self, Frame};
use std::collections::{HashMap, VecDeque};
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
    let mut messages = Messages::new(input, server.options().cancellation.clone())?;
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
            None => {
                let replies = server.dispatch(&message);
                messages.settled(&message, server);
                replies
            }
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
        /// The document versions the server did not keep, by URI and the
        /// index of the message that brought them, which the reader stops
        /// tracking.
        rejected: mpsc::Sender<(String, u64)>,
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
        let (rejected, forgotten) = mpsc::channel();
        // The input is handed over once the thread runs, so a thread that
        // cannot start leaves it to be read here.
        let (hand, taken) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("vibes-lsp-reader".to_owned())
            .spawn(move || {
                if let Ok(input) = taken.recv() {
                    let mut tracking = Tracking::new(cancellation, forgotten);
                    read_ahead(BufReader::new(input), &sender, &mut tracking);
                }
            });
        let source = match spawned {
            Ok(_) => {
                hand.send(input)
                    .map_err(|_| io::Error::other("reader thread ended before taking its input"))?;
                Source::Threaded {
                    receiver,
                    queue: VecDeque::new(),
                    rejected,
                }
            }
            Err(_) => Source::Inline(BufReader::new(input)),
        };
        Ok(Self { source })
    }

    /// Tells the reader when the document `message` opened or changed is
    /// not one the server kept, so the reader stops tracking that version.
    fn settled(&self, message: &Inbound, server: &Server) {
        let Source::Threaded { rejected, .. } = &self.source else {
            return;
        };
        if !matches!(
            message.method.as_str(),
            "textDocument/didOpen" | "textDocument/didChange"
        ) {
            return;
        }
        if let Some(uri) = message.document()
            && server.document(&uri).is_none()
        {
            // A reader that has ended has nothing left to track.
            let _ = rejected.send((uri, message.index));
        }
    }
}

/// Reads and decodes messages ahead of the server. Each document open or
/// change carries the token the server analyzes it under, and a change or
/// close for the same document cancels the analysis of the version before
/// it, since its result would be superseded before it could be published.
fn read_ahead<R: Read>(
    mut reader: BufReader<R>,
    sender: &mpsc::SyncSender<Event>,
    tracking: &mut Tracking,
) {
    let mut index = 0;
    loop {
        let event = match read_message(&mut reader) {
            Ok(Some(mut message)) => {
                if let Some(message) = &mut message {
                    index += 1;
                    message.index = index;
                    tracking.supersede(message);
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

/// The documents the reader tracks: the token of each open document's
/// latest version, so a change or close cancels its own document's analysis
/// however many other documents' notifications arrived in between.
///
/// Every open and change gets a token, so a document the server admits can
/// always be cancelled. The table holds the documents the server keeps and
/// the versions it has not settled yet: a close forgets its document, and
/// the server reports each version it did not keep, which the reader forgets
/// when it next reads. So the table is bounded by the server's documents and
/// the messages read ahead of it, which the queue to the server holds too.
struct Tracking {
    cancellation: CancellationToken,
    /// Each tracked document's latest version: the index of the message
    /// that brought it and the token its analysis runs under.
    analyzing: HashMap<String, (u64, CancellationToken)>,
    rejected: mpsc::Receiver<(String, u64)>,
}

impl Tracking {
    fn new(cancellation: CancellationToken, rejected: mpsc::Receiver<(String, u64)>) -> Self {
        Self {
            cancellation,
            analyzing: HashMap::new(),
            rejected,
        }
    }

    /// Cancels the analysis of the version of a document that `message`
    /// changes or closes, and gives an open or a change the token its own
    /// analysis runs under, for the next change or close to cancel.
    fn supersede(&mut self, message: &mut Inbound) {
        // What the server has settled since the last message read.
        for (uri, index) in self.rejected.try_iter() {
            if self
                .analyzing
                .get(&uri)
                .is_some_and(|(version, _)| *version == index)
            {
                self.analyzing.remove(&uri);
            }
        }
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
        if changes && let Some((_, token)) = self.analyzing.remove(&uri) {
            token.cancel();
        }
        if opens {
            let token = self.cancellation.child_token();
            message.token = Some(token.clone());
            self.analyzing.insert(uri, (message.index, token));
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
            Source::Threaded {
                receiver, queue, ..
            } => (receiver, queue),
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

#[cfg(test)]
mod tracking_tests {
    use super::*;

    fn message(method: &str, uri: &str, index: u64) -> Inbound {
        let params = format!(r#"{{"textDocument":{{"uri":"{uri}","text":""}}}}"#);
        Inbound {
            id: None,
            method: method.to_owned(),
            params: Some(params.into_boxed_str()),
            token: None,
            index,
        }
    }

    #[test]
    fn the_table_follows_the_servers_documents() {
        let (rejected, forgotten) = mpsc::channel();
        let mut tracking = Tracking::new(CancellationToken::new(), forgotten);
        let mut open_a = message("textDocument/didOpen", "a", 1);
        let mut open_b = message("textDocument/didOpen", "b", 2);
        tracking.supersede(&mut open_a);
        tracking.supersede(&mut open_b);
        assert!(open_a.token.is_some() && open_b.token.is_some());
        assert_eq!(tracking.analyzing.len(), 2);
        // The server did not keep A's version; a stale report for B names a
        // version B has moved past, so B stays. Both apply at the next read.
        rejected.send(("a".to_owned(), 1)).unwrap();
        rejected.send(("b".to_owned(), 1)).unwrap();
        let mut change_b = message("textDocument/didChange", "b", 3);
        tracking.supersede(&mut change_b);
        assert!(
            open_b
                .token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        );
        assert_eq!(tracking.analyzing.len(), 1);
        assert_eq!(tracking.analyzing["b"].0, 3);
        // A change to a document the reader no longer tracks still gets a
        // token for its own analysis.
        let mut change_a = message("textDocument/didChange", "a", 4);
        tracking.supersede(&mut change_a);
        assert!(
            change_a
                .token
                .as_ref()
                .is_some_and(|token| !token.is_cancelled())
        );
        tracking.supersede(&mut message("textDocument/didClose", "a", 5));
        tracking.supersede(&mut message("textDocument/didClose", "b", 6));
        assert!(
            change_a
                .token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        );
        assert!(tracking.analyzing.is_empty());
    }
}
