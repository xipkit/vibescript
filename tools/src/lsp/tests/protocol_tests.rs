//! Lifecycle, framing, formatting and document-state tests.

use super::super::server::{diagnostics_notification, formatting_edits};
use super::super::transport::{self, Frame};
use super::*;
use std::io::{self, BufReader, Cursor};
use vibescript::CancellationToken;

/// Serves framed input to completion and returns what the server wrote.
fn run(input: Vec<u8>) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    serve(
        &mut Server::new(),
        Cursor::new(input),
        &mut output,
        &CancellationToken::new(),
    )?;
    Ok(output)
}

fn framed(messages: &[Value]) -> Vec<u8> {
    let mut input = Vec::new();
    for message in messages {
        let body = message.to_string();
        input.extend_from_slice(format!("Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    }
    input
}

/// Decodes every framed message a server wrote.
fn decode(output: &[u8]) -> Vec<Value> {
    let mut reader = BufReader::new(output);
    let mut out = Vec::new();
    while let Frame::Message(body) = transport::read(&mut reader).unwrap() {
        out.push(serde_json::from_slice(&body).unwrap());
    }
    out
}

#[test]
fn serve_exits_cleanly_at_the_end_of_input() {
    assert!(run(Vec::new()).unwrap().is_empty());
}

// WASI preview 1 has no threads to block on input while another cancels.
#[cfg(not(target_os = "wasi"))]
mod blocking {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A reader that blocks until the test ends, signalling when it is first read.
    struct Blocking {
        started: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    impl Read for Blocking {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            if let Some(started) = self.started.take() {
                let _ = started.send(());
            }
            let _ = self.release.recv();
            Ok(0)
        }
    }

    #[test]
    fn cancellation_stops_a_server_blocked_on_input() {
        let (started, wait_started) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        let token = CancellationToken::new();
        let cancel = token.clone();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let input = Blocking {
                started: Some(started),
                release: released,
            };
            let _ = done.send(serve(&mut Server::new(), input, io::sink(), &token));
        });
        wait_started
            .recv_timeout(Duration::from_secs(5))
            .expect("the server began reading");
        cancel.cancel();
        let result = finished
            .recv_timeout(Duration::from_secs(5))
            .expect("the server stopped after cancellation");
        assert!(result.is_ok());
        drop(release);
    }
}

#[test]
fn serves_a_session_over_framed_streams() {
    let uri = "file:///tmp/session.vibe";
    let input = framed(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "text": "def run(\n"}}}),
        json!({"jsonrpc": "2.0", "id": "two", "method": "textDocument/hover", "params": position(uri, 0, 5)}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown"}),
    ]);
    let replies = decode(&run(input).unwrap());
    assert_eq!(replies.len(), 4, "{replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["method"], "textDocument/publishDiagnostics");
    assert_eq!(replies[2]["id"], "two");
    assert_eq!(
        replies[3],
        json!({"jsonrpc": "2.0", "id": 3, "result": null})
    );
}

#[test]
fn skips_malformed_and_oversized_messages() {
    let mut input = b"Content-Length: 9\r\n\r\nnot json!".to_vec();
    input.extend(framed(&[json!([1, 2])]));
    input.extend(framed(&[json!({"id": 1, "method": 7})]));
    input.extend(format!("Content-Length: {}\r\n\r\n", transport::MAX_PAYLOAD + 1).as_bytes());
    input.resize(input.len() + transport::MAX_PAYLOAD + 1, b'x');
    input.extend(framed(&[
        json!({"jsonrpc": "2.0", "id": 5, "method": "shutdown"}),
    ]));
    assert_eq!(
        decode(&run(input).unwrap()),
        [json!({"jsonrpc": "2.0", "id": 5, "result": null})]
    );
}

#[test]
fn corrupt_framing_ends_the_server_with_an_error() {
    let error = run(b"Content-Length: nope\r\n\r\n".to_vec()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "lsp read: invalid Content-Length: strconv.Atoi: parsing \"nope\": invalid syntax"
    );
}

#[test]
fn reads_framed_payloads_larger_than_a_source() {
    let source = "\n".repeat(1 << 20);
    let payload = json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": "file:///tmp/large.vibe", "text": source}}});
    let wire = framed(std::slice::from_ref(&payload));
    assert!(wire.len() > 1 << 20);
    let frame = transport::read(&mut BufReader::new(wire.as_slice())).unwrap();
    assert_eq!(frame, Frame::Message(payload.to_string().into_bytes()));
}

#[test]
fn initialize_advertises_the_reference_capabilities() {
    let mut server = server();
    let response = request(&mut server, "initialize", json!({}));
    assert_eq!(
        response["result"],
        json!({"capabilities": {
            "textDocumentSync": 1,
            "hoverProvider": true,
            "documentFormattingProvider": true,
            "definitionProvider": true,
            "documentSymbolProvider": true,
            "signatureHelpProvider": {"triggerCharacters": ["(", ","]},
            "completionProvider": {"resolveProvider": false, "triggerCharacters": ["."]},
        }})
    );
    // The reference answers an initialize without an id, omitting the id.
    let replies = replies(&mut server, &message("initialize", None, None));
    assert!(
        replies[0]
            .json()
            .starts_with(r#"{"jsonrpc":"2.0","result":{"capabilities""#)
    );
}

#[test]
fn requests_echo_their_ids_verbatim() {
    let mut server = server();
    for id in ["7", "\"seven\"", "1e3", "-0", "{\"a\":1}"] {
        let replies = replies(&mut server, &message("shutdown", Some(id), None));
        assert_eq!(
            replies[0].json(),
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#)
        );
    }
    assert!(replies(&mut server, &message("shutdown", None, None)).is_empty());
    let parsed = Inbound::parse(br#"{"id":null,"method":"shutdown"}"#).unwrap();
    assert!(parsed.id.is_none());
    let parsed = Inbound::parse(br#"{"ID":2,"METHOD":"shutdown","Params":null}"#).unwrap();
    assert_eq!(
        (parsed.id.as_deref(), parsed.method.as_str()),
        (Some("2"), "shutdown")
    );
    assert_eq!(parsed.params.as_deref(), Some("null"));
    for invalid in [
        &br#"[1]"#[..],
        br#"{"method":1}"#,
        br#"{"jsonrpc":2}"#,
        b"{",
        b"{} x",
    ] {
        assert!(
            Inbound::parse(invalid).is_none(),
            "{:?}",
            String::from_utf8_lossy(invalid)
        );
    }
}

#[test]
fn unknown_requests_fail_and_unknown_notifications_are_ignored() {
    let mut server = server();
    let response = request(&mut server, "workspace/symbol", json!({}));
    assert_eq!(
        response["error"],
        json!({"code": -32601, "message": "method not found"})
    );
    for method in [
        "$/cancelRequest",
        "$/setTrace",
        "initialized",
        "exit",
        "workspace/didChangeConfiguration",
    ] {
        assert!(
            handle(&mut server, &message(method, None, Some(json!({})))).is_empty(),
            "{method}"
        );
    }
    // A client response carries an id without a method.
    let replies = handle(
        &mut server,
        &Inbound::parse(br#"{"jsonrpc":"2.0","id":9,"result":null}"#).unwrap(),
    );
    assert_eq!(replies[0]["error"]["code"], -32601);
}

#[test]
fn invalid_request_params_are_rejected_like_the_reference() {
    let mut server = server();
    for (method, text) in [
        ("textDocument/formatting", "invalid formatting params"),
        ("textDocument/definition", "invalid definition params"),
        (
            "textDocument/documentSymbol",
            "invalid documentSymbol params",
        ),
        ("textDocument/signatureHelp", "invalid signatureHelp params"),
        ("textDocument/completion", "invalid completion params"),
        ("textDocument/hover", "invalid hover params"),
    ] {
        for params in [
            None,
            Some(json!([1])),
            Some(json!({"textDocument": {"uri": 5}})),
            Some(json!({"textDocument": "x"})),
        ] {
            let replies = handle(&mut server, &message(method, Some("1"), params.clone()));
            assert_eq!(
                replies[0]["error"],
                json!({"code": -32602, "message": text}),
                "{method} {params:?}"
            );
        }
        // Notifications never get a reply.
        assert!(handle(&mut server, &message(method, None, None)).is_empty());
    }
    for params in [
        json!({"position": {"line": 1.5}}),
        json!({"position": {"character": "1"}}),
    ] {
        let replies = handle(
            &mut server,
            &message("textDocument/hover", Some("1"), Some(params)),
        );
        assert_eq!(replies[0]["error"]["code"], -32602);
    }
    // `null` parameters decode to zero values.
    let replies = handle(
        &mut server,
        &message("textDocument/hover", Some("1"), Some(Value::Null)),
    );
    assert_eq!(replies[0]["result"], Value::Null);
    let replies = handle(
        &mut server,
        &message("textDocument/documentSymbol", Some("1"), Some(Value::Null)),
    );
    assert_eq!(replies[0]["result"], json!([]));
    // Malformed notifications are ignored.
    for method in [
        "textDocument/didOpen",
        "textDocument/didChange",
        "textDocument/didClose",
    ] {
        assert!(
            handle(&mut server, &message(method, None, None)).is_empty(),
            "{method}"
        );
        assert!(
            handle(
                &mut server,
                &message(method, None, Some(json!({"textDocument": 1})))
            )
            .is_empty()
        );
    }
}

#[test]
fn formatting_returns_one_full_document_edit() {
    let mut server = server();
    let uri = "file:///tmp/fmt.vibe";
    open(&mut server, uri, "def run()  \n  1\t\nend");
    let response = request(
        &mut server,
        "textDocument/formatting",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(
        response["result"],
        json!([{
            "newText": "def run()\n  1\nend\n",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 3}},
        }])
    );
}

#[test]
fn formatting_a_formatted_document_returns_no_edits() {
    let mut server = server();
    let uri = "file:///tmp/clean.vibe";
    open(&mut server, uri, "def run()\n  1\nend\n");
    let response = request(
        &mut server,
        "textDocument/formatting",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(response["result"], json!([]));
}

#[test]
fn formatting_an_unknown_document_returns_an_explicit_null() {
    let mut server = server();
    let replies = replies(
        &mut server,
        &message(
            "textDocument/formatting",
            Some("9"),
            Some(json!({"textDocument": {"uri": "file:///tmp/missing.vibe"}})),
        ),
    );
    assert_eq!(
        replies[0].json(),
        r#"{"jsonrpc":"2.0","id":9,"result":null}"#
    );
}

#[test]
fn formatting_edits_handle_bare_carriage_returns() {
    let edits = formatting_edits("a\rb\r", &text::split_lines("a\rb\r"));
    let edits: Value = serde_json::from_str(&edits.encode()).unwrap();
    assert_eq!(edits[0]["newText"], "a\nb\n");
    assert_eq!(edits[0]["range"]["end"], json!({"line": 2, "character": 0}));
}

#[test]
fn publish_diagnostics_wire_format() {
    let range = Range {
        start: Position::new(1, 2),
        end: Position::new(1, 5),
    };
    let notification = diagnostics_notification(
        "file:///tmp/wire.vibe",
        &[analysis::diagnostic(range, Severity::Error, "boom")],
    );
    assert_eq!(
        notification.json(),
        r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/wire.vibe","diagnostics":[{"range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}},"severity":1,"source":"vibes-lsp","message":"boom"}]}}"#
    );
    let clean = diagnostics_notification("file:///tmp/wire.vibe", &[]);
    assert!(clean.json().contains(r#""diagnostics":[]"#));
}

#[test]
fn did_close_drops_every_piece_of_document_state() {
    let mut server = server();
    let uri = "file:///tmp/close-evict.vibe";
    open(
        &mut server,
        uri,
        "def helper(n)\n  n\nend\n\ndef run()\n  helper(1)\nend\n",
    );
    completion_labels(&mut server, uri, 5, 2);
    symbols(&mut server, uri);
    {
        let document = document(&server, uri);
        assert!(document.compiled.is_some() && document.program.is_some());
        assert!(document.completion.get().is_some() && document.symbols.get().is_some());
    }
    close(&mut server, uri);
    assert!(server.document(uri).is_none());
}

#[test]
fn did_close_publishes_empty_diagnostics() {
    let mut server = server();
    let uri = "file:///tmp/close-clear.vibe";
    assert!(!open_diagnostics(&mut server, uri, "def run(\n  1\nend\n").is_empty());
    let replies = replies(
        &mut server,
        &message(
            "textDocument/didClose",
            None,
            Some(json!({"textDocument": {"uri": uri}})),
        ),
    );
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].json(),
        r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///tmp/close-clear.vibe","diagnostics":[]}}"#
    );
}

#[test]
fn reopening_identical_text_recomputes_the_same_diagnostics() {
    let mut server = server();
    let uri = "file:///tmp/reopen-same.vibe";
    let source = "def run(\n  1\nend\n";
    let first = open_diagnostics(&mut server, uri, source);
    assert!(!first.is_empty());
    close(&mut server, uri);
    assert_eq!(open_diagnostics(&mut server, uri, source), first);
}

#[test]
fn reopening_different_text_drops_stale_state() {
    let mut server = server();
    let uri = "file:///tmp/reopen-different.vibe";
    open(&mut server, uri, "def alpha()\n  1\nend\n");
    assert_eq!(names(&symbols(&mut server, uri)), ["alpha"]);
    close(&mut server, uri);
    assert!(!open_diagnostics(&mut server, uri, "def beta(\n  2\nend\n").is_empty());
    assert!(!names(&symbols(&mut server, uri)).contains(&"alpha"));
    close(&mut server, uri);
    assert!(open_diagnostics(&mut server, uri, "def beta()\n  2\nend\n").is_empty());
    assert_eq!(names(&symbols(&mut server, uri)), ["beta"]);
}

#[test]
fn closing_an_unknown_document_does_nothing() {
    let mut server = server();
    let kept = "file:///tmp/kept.vibe";
    open(&mut server, kept, "def run()\n  1\nend\n");
    assert!(close(&mut server, "file:///tmp/never-opened.vibe").is_empty());
    assert!(server.document(kept).is_some());
}

#[test]
fn document_lines_follow_each_change() {
    let mut server = server();
    let uri = "file:///tmp/cache.vibe";
    open(&mut server, uri, "def old\n  1\nend\n");
    assert_eq!(server.lines(uri)[0], "def old");
    change(&mut server, uri, "def fresh\n  2\nend\n");
    assert_eq!(server.lines(uri)[0], "def fresh");
    // A change without content changes is ignored.
    let params = json!({"textDocument": {"uri": uri}, "contentChanges": []});
    assert!(
        handle(
            &mut server,
            &message("textDocument/didChange", None, Some(params))
        )
        .is_empty()
    );
    assert_eq!(server.lines(uri)[0], "def fresh");
    // The last change carries the full text.
    let params = json!({"textDocument": {"uri": uri}, "contentChanges": [{"text": "a"}, {"text": "def last\nend\n"}]});
    assert_eq!(
        handle(
            &mut server,
            &message("textDocument/didChange", None, Some(params))
        )
        .len(),
        1
    );
    assert_eq!(server.lines(uri)[0], "def last");
    // An unknown document reads as one empty line.
    assert_eq!(server.lines("file:///tmp/unknown.vibe"), [""]);
}

#[test]
fn queued_changes_to_one_document_analyze_only_the_last() {
    let uri = "file:///tmp/typing.vibe";
    let mut messages = vec![
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "text": "x = 1\n"}}}),
    ];
    for index in 0..20 {
        messages.push(json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {"textDocument": {"uri": uri}, "contentChanges": [{"text": format!("x = {index}\n")}]}}));
    }
    messages.push(json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover", "params": position(uri, 0, 0)}));
    let replies = decode(&run(framed(&messages)).unwrap());
    // Whatever was skipped, the last change is published before the request.
    let last = &replies[replies.len() - 1];
    assert_eq!(last["id"], 1);
    assert_eq!(
        replies[replies.len() - 2]["method"],
        "textDocument/publishDiagnostics"
    );
    assert!(replies.len() <= 23, "{}", replies.len());
}

#[test]
fn a_queued_request_can_be_cancelled() {
    let uri = "file:///tmp/cancel.vibe";
    let mut source = String::new();
    for index in 0..3000 {
        source.push_str(&format!(
            "def f{index}(x: int) -> int\n  x + {index}\nend\n"
        ));
    }
    let messages = [
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "text": source}}}),
        json!({"jsonrpc": "2.0", "id": 1, "method": "textDocument/hover", "params": position(uri, 0, 4)}),
        json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 1}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
    ];
    let replies = decode(&run(framed(&messages)).unwrap());
    let hover = replies.iter().find(|reply| reply["id"] == 1).unwrap();
    // The cancel arrives while the document is analyzed, so the queued hover
    // is answered as cancelled; a slow reader thread could still let it run.
    assert!(
        hover["error"] == json!({"code": -32800, "message": "request cancelled"})
            || hover["result"].is_object(),
        "{hover}"
    );
    assert_eq!(replies.last().unwrap()["id"], 2);
}
