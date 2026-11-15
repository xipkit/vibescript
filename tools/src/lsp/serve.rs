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
        let replies = match messages.cancelled(&message) {
            Some(id) => vec![Outbound::Error {
                id,
                code: -32800,
                message: "request cancelled",
            }],
            None => {
                let replies = server.dispatch(&message);
                settled(&message, server);
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
    /// Read ahead on a reader thread, with the events taken from it so far
    /// queued for the server to look ahead at.
    Threaded {
        receiver: mpsc::Receiver<Event>,
        queue: VecDeque<Event>,
    },
}

struct Messages<R> {
    source: Source<R>,
}

/// How many decoded messages the reader thread may hold ahead of the server,
/// and how many the server queues behind the one it handles. With the one
/// the reader is waiting to hand over, that bounds what a client flooding
/// large changes can pin to `2 * READ_AHEAD + 2` messages: past it, the
/// reader stops reading and the client's writes block.
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
                    let mut tracking = Tracking::new(cancellation);
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
                }
            }
            Err(_) => Source::Inline(BufReader::new(input)),
        };
        Ok(Self { source })
    }
}

/// Tells the reader when the document `message` opened or changed is not
/// one the server kept, by cancelling the token the reader gave that
/// version, so the reader stops tracking it.
fn settled(message: &Inbound, server: &Server) {
    if !matches!(
        message.method.as_str(),
        "textDocument/didOpen" | "textDocument/didChange"
    ) {
        return;
    }
    if let Some(token) = &message.token
        && let Some(uri) = message.document()
        && server.document(&uri).is_none()
    {
        token.cancel();
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
    loop {
        let event = match read_message(&mut reader) {
            Ok(Some(mut message)) => {
                if let Some(message) = &mut message {
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
/// the server cancels the token of each version it did not keep, which the
/// reader forgets when it next reads an open, change or close. So the table
/// is bounded by the server's documents and the messages read ahead of it,
/// which the queue to the server holds too.
struct Tracking {
    cancellation: CancellationToken,
    /// The token each tracked document's latest version is analyzed under.
    analyzing: HashMap<String, CancellationToken>,
}

impl Tracking {
    fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            analyzing: HashMap::new(),
        }
    }

    /// Cancels the analysis of the version of a document that `message`
    /// changes or closes, and gives an open or a change the token its own
    /// analysis runs under, for the next change or close to cancel.
    fn supersede(&mut self, message: &mut Inbound) {
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
        // Forget the versions the server did not keep.
        self.analyzing.retain(|_, token| !token.is_cancelled());
        if changes && let Some(token) = self.analyzing.remove(&uri) {
            token.cancel();
        }
        if opens {
            let token = self.cancellation.child_token();
            message.token = Some(token.clone());
            self.analyzing.insert(uri, token);
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

/// Moves events that have arrived from `receiver` into `queue` until it
/// holds the next event and `READ_AHEAD` behind it. Events left in the
/// channel keep it full, so the reader thread waits instead of reading on.
///
/// At most `READ_AHEAD` events are taken, all the channel held when filling
/// began, since a change replacing the one before it leaves the queue no
/// longer and a client sending such changes as fast as they are taken could
/// otherwise keep the server here.
fn fill(queue: &mut VecDeque<Event>, receiver: &mpsc::Receiver<Event>) {
    for _ in 0..READ_AHEAD {
        if queue.len() > READ_AHEAD {
            return;
        }
        let Ok(event) = receiver.try_recv() else {
            return;
        };
        enqueue(queue, event);
    }
}

/// Queues `event`, first dropping the changes queued right before it that
/// it replaces: when `event` is a full-text change, the changes to the same
/// document behind it, since analyzing them would be wasted. So a flood of
/// changes to one document holds one body.
fn enqueue(queue: &mut VecDeque<Event>, event: Event) {
    if let Ok(Some(change)) = &event
        && change.method == "textDocument/didChange"
        && let Ok((uri, Some(_))) = change_params(change)
    {
        while let Some(Ok(Some(queued))) = queue.back()
            && queued.method == "textDocument/didChange"
            && queued.document().as_deref() == Some(uri.as_str())
        {
            queue.pop_back();
        }
    }
    queue.push_back(event);
}

impl<R: Read> Messages<R> {
    /// How many events are queued behind the one last taken.
    #[cfg(test)]
    fn queued(&self) -> usize {
        match &self.source {
            Source::Inline(_) => 0,
            Source::Threaded { queue, .. } => queue.len(),
        }
    }

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
        loop {
            // Queue what has already arrived before taking the next event,
            // so a change replaced by a later one is dropped unhandled and
            // the server can see what follows.
            fill(queue, receiver);
            if let Some(event) = queue.pop_front() {
                return event.map(Some);
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => queue.push_back(event),
                Err(mpsc::RecvTimeoutError::Timeout) if cancellation.is_cancelled() => {
                    return Ok(None);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
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
    use super::super::Options;
    use super::*;

    fn message(method: &str, uri: &str) -> Inbound {
        let params = format!(r#"{{"textDocument":{{"uri":"{uri}","text":""}}}}"#);
        Inbound {
            id: None,
            method: method.to_owned(),
            params: Some(params.into_boxed_str()),
            token: None,
        }
    }

    #[test]
    fn the_table_follows_the_servers_documents() {
        let mut tracking = Tracking::new(CancellationToken::new());
        let mut open_a = message("textDocument/didOpen", "a");
        let mut open_b = message("textDocument/didOpen", "b");
        let mut change_b = message("textDocument/didChange", "b");
        tracking.supersede(&mut open_a);
        tracking.supersede(&mut open_b);
        tracking.supersede(&mut change_b);
        assert!(
            open_b
                .token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        );
        assert_eq!(tracking.analyzing.len(), 2);
        // The server kept neither A's version nor B's first, which B has
        // moved past, so B stays. Both apply at the next document read.
        for rejected in [&open_a, &open_b] {
            rejected.token.as_ref().unwrap().cancel();
        }
        tracking.supersede(&mut message("textDocument/didOpen", "c"));
        assert_eq!(tracking.analyzing.len(), 2);
        assert!(!tracking.analyzing.contains_key("a"));
        assert!(
            change_b
                .token
                .as_ref()
                .is_some_and(|token| !token.is_cancelled())
        );
        // A change to a document the reader no longer tracks still gets a
        // token for its own analysis.
        let mut change_a = message("textDocument/didChange", "a");
        tracking.supersede(&mut change_a);
        assert!(
            change_a
                .token
                .as_ref()
                .is_some_and(|token| !token.is_cancelled())
        );
        for uri in ["a", "b", "c"] {
            tracking.supersede(&mut message("textDocument/didClose", uri));
        }
        assert!(
            change_a
                .token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        );
        assert!(tracking.analyzing.is_empty());
    }

    #[test]
    fn the_server_cancels_the_versions_it_does_not_keep() {
        let mut server = Server::with_options(Options {
            max_documents: Some(1),
            ..Options::default()
        });
        let mut tracking = Tracking::new(CancellationToken::new());
        let mut kept = message("textDocument/didOpen", "a");
        let mut rejected = message("textDocument/didOpen", "b");
        for message in [&mut kept, &mut rejected] {
            tracking.supersede(message);
            server.dispatch(message);
            settled(message, &server);
        }
        assert!(
            kept.token
                .as_ref()
                .is_some_and(|token| !token.is_cancelled())
        );
        assert!(
            rejected
                .token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
        );
        tracking.supersede(&mut message("textDocument/didOpen", "c"));
        let mut tracked: Vec<_> = tracking.analyzing.keys().collect();
        tracked.sort();
        assert_eq!(tracked, ["a", "c"]);
    }
}

#[cfg(test)]
mod lookahead_tests {
    use super::*;

    fn change(uri: &str, text: &str) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {"textDocument": {"uri": uri}, "contentChanges": [{"text": text}]},
        })
        .to_string()
    }

    fn inbound(body: &str) -> Inbound {
        Inbound::parse(body.as_bytes()).expect("a JSON-RPC message")
    }

    /// The server taking events from a channel the test fills in place of
    /// the reader thread.
    fn threaded(receiver: mpsc::Receiver<Event>) -> Messages<io::Empty> {
        Messages {
            source: Source::Threaded {
                receiver,
                queue: VecDeque::new(),
            },
        }
    }

    /// Each message a slow server takes, which waits for the client to fill
    /// the channel before taking one, with the id it answers as cancelled.
    /// Checks that the server never queues more than `READ_AHEAD` messages.
    fn handled_slowly(bodies: &[String]) -> Vec<(Inbound, Option<Box<str>>)> {
        let (sender, receiver) = mpsc::sync_channel(READ_AHEAD);
        let mut messages = threaded(receiver);
        let mut sent = 0;
        let mut handled = Vec::new();
        let never = CancellationToken::new();
        loop {
            // The client floods until the server stops taking its messages.
            while let Some(body) = bodies.get(sent)
                && sender.try_send(Ok(Some(inbound(body)))).is_ok()
            {
                sent += 1;
            }
            if sent == bodies.len() {
                break;
            }
            let message = messages.next(&never).unwrap().unwrap().unwrap();
            assert!(messages.queued() <= READ_AHEAD);
            let cancelled = messages.cancelled(&message);
            handled.push((message, cancelled));
        }
        drop(sender);
        while let Some(message) = messages.next(&never).unwrap() {
            let message = message.unwrap();
            let cancelled = messages.cancelled(&message);
            handled.push((message, cancelled));
        }
        handled
    }

    #[test]
    fn a_slow_server_queues_at_most_read_ahead_messages() {
        let bodies: Vec<_> = (0..10 * READ_AHEAD)
            .map(|index| change(&format!("file:///tmp/{index}.vibe"), "x = 1\n"))
            .collect();
        let handled = handled_slowly(&bodies);
        let uris: Vec<_> = handled
            .iter()
            .map(|(message, _)| message.document().unwrap())
            .collect();
        let sent: Vec<_> = (0..10 * READ_AHEAD)
            .map(|index| format!("file:///tmp/{index}.vibe"))
            .collect();
        assert_eq!(uris, sent);
    }

    #[test]
    fn a_capped_queue_still_drops_replaced_changes_and_finds_cancels() {
        let mut bodies: Vec<_> = (0..5)
            .map(|index| change(&format!("file:///tmp/busy{index}.vibe"), ""))
            .collect();
        bodies.push(r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover"}"#.to_owned());
        for index in 0..3 * READ_AHEAD {
            bodies.push(change("file:///tmp/typing.vibe", &format!("x = {index}\n")));
        }
        bodies.push(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":1}}"#.to_owned());
        bodies.push(r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#.to_owned());
        let handled = handled_slowly(&bodies);
        let summary: Vec<_> = handled
            .iter()
            .map(|(message, cancelled)| {
                let text = change_params(message).ok().and_then(|(_, text)| text);
                (
                    message.method.as_str(),
                    text.unwrap_or_default(),
                    cancelled.as_deref().map(str::to_owned),
                )
            })
            .collect();
        let change = |text: &str| ("textDocument/didChange", text.to_owned(), None);
        let last = format!("x = {}\n", 3 * READ_AHEAD - 1);
        assert_eq!(
            summary,
            [
                change(""),
                change(""),
                change(""),
                change(""),
                change(""),
                ("textDocument/hover", String::new(), Some("1".to_owned())),
                change(&last),
                ("shutdown", String::new(), None),
            ]
        );
    }
}

// WASI preview 1 has no threads to read ahead on.
#[cfg(all(test, not(target_os = "wasi")))]
mod backpressure_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// A client's pipe holding framed messages, counting how many of them
    /// the reader has begun to read. A read never crosses into the next
    /// message, so the reader begins one only once it wants it.
    struct Flood {
        frames: Vec<Vec<u8>>,
        frame: usize,
        offset: usize,
        begun: Arc<AtomicUsize>,
    }

    impl Read for Flood {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(frame) = self.frames.get(self.frame) else {
                return Ok(0);
            };
            if self.offset == 0 {
                self.begun.fetch_add(1, Ordering::SeqCst);
            }
            let count = frame.get(self.offset..).unwrap_or_default().read(buffer)?;
            self.offset += count;
            if self.offset == frame.len() {
                self.frame += 1;
                self.offset = 0;
            }
            Ok(count)
        }
    }

    #[test]
    fn a_slow_server_stops_the_reader_from_reading_on() {
        let total = 10 * READ_AHEAD;
        // Large enough that BufReader reads each body in several pieces.
        let text = "x".repeat(64 * 1024);
        let frames = (0..total)
            .map(|index| {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {"uri": format!("file:///tmp/{index}.vibe")},
                        "contentChanges": [{"text": text}],
                    },
                })
                .to_string();
                format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
            })
            .collect();
        let begun = Arc::new(AtomicUsize::new(0));
        let flood = Flood {
            frames,
            frame: 0,
            offset: 0,
            begun: Arc::clone(&begun),
        };
        let mut messages = Messages::new(flood, CancellationToken::new()).unwrap();
        let never = CancellationToken::new();
        for handled in 0..total {
            // The reader stops once the channel is full and it holds one
            // more message to send.
            let stalled = total.min(handled + messages.queued() + READ_AHEAD + 1);
            let deadline = Instant::now() + Duration::from_secs(5);
            while begun.load(Ordering::SeqCst) < stalled {
                assert!(Instant::now() < deadline, "the reader stopped early");
                std::thread::yield_now();
            }
            let read = begun.load(Ordering::SeqCst);
            assert!(
                read - handled <= 2 * READ_AHEAD + 1,
                "{read} messages read with {handled} handled"
            );
            let message = messages.next(&never).unwrap().unwrap().unwrap();
            assert_eq!(
                message.document().unwrap(),
                format!("file:///tmp/{handled}.vibe")
            );
        }
        assert!(messages.next(&never).unwrap().is_none());
    }
}
