use super::*;

fn limited(max_documents: Option<usize>, max_document_bytes: Option<usize>) -> Server {
    Server::with_options(Options {
        max_documents,
        max_document_bytes,
        ..Options::default()
    })
}

fn rejected(server: &Server, uri: &str, replies: &[Value], reason: &str) {
    assert!(server.document(uri).is_none());
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(replies[0]["params"]["uri"], uri);
    let diagnostics = replies[0]["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["severity"], 2);
    assert_eq!(diagnostics[0]["message"], reason);
}

const COUNT_LIMIT: &str = "open document count limit exceeded";
const BYTE_LIMIT: &str = "aggregate document storage limit exceeded";
const LINE: usize = std::mem::size_of::<String>();

#[test]
fn session_limits_are_opt_in_and_analysis_limits_do_not_bound_storage() {
    let mut server = Server::with_options(Options {
        max_source_bytes: 1,
        ..Options::default()
    });
    let source = " ".repeat(32);
    for index in 0..128 {
        let uri = format!("untitled:{index}");
        let diagnostics = open_diagnostics(&mut server, &uri, &source);
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("source exceeds")
        );
    }
    for index in 0..128 {
        assert_eq!(
            document(&server, &format!("untitled:{index}")).text(),
            source
        );
    }
}

#[test]
fn repeated_distinct_uris_stop_at_the_count_limit_and_close_releases_it() {
    let mut server = limited(Some(2), None);
    open(&mut server, "untitled:0", "");
    open(&mut server, "untitled:1", "");
    for index in 2..130 {
        let uri = format!("untitled:{index}");
        let replies = open(&mut server, &uri, "x = 1\n");
        rejected(&server, &uri, &replies, COUNT_LIMIT);
    }
    // Duplicate opens and changes replace a slot rather than consuming one.
    open(&mut server, "untitled:0", "x = 1\n");
    change(&mut server, "untitled:0", "x = 2\n");
    assert_eq!(document(&server, "untitled:0").text(), "x = 2\n");
    close(&mut server, "untitled:0");
    close(&mut server, "untitled:0");
    open(&mut server, "untitled:2", "");
    assert!(server.document("untitled:2").is_some());
    assert!(server.document("untitled:1").is_some());
}

#[test]
fn exact_byte_limits_count_both_uris_unicode_and_all_line_endings() {
    let uri = "untitled:é😀";
    for (source, line_count, payload) in [
        ("", 1, 0),
        ("é😀", 1, 6),
        ("é😀\r\nx\ry\n", 4, 8),
        ("\r\n\r\n", 3, 0),
        ("\n\r", 3, 0),
        ("\r\r\n\n", 4, 0),
    ] {
        let bytes = 2 * uri.len() + source.len() + payload + line_count * LINE;
        let mut server = limited(None, Some(bytes - 1));
        let replies = open(&mut server, uri, source);
        rejected(&server, uri, &replies, BYTE_LIMIT);
        let mut server = limited(None, Some(bytes));
        open(&mut server, uri, source);
        assert_eq!(document(&server, uri).text(), source);
        assert_eq!(document(&server, uri).lines.capacity(), line_count);
    }
}

#[test]
fn aggregate_bytes_bound_repeated_distinct_uris_without_a_count_limit() {
    let bytes = 2 * "untitled:0000".len() + LINE;
    let mut server = limited(None, Some(2 * bytes));
    for index in 0..128 {
        let uri = format!("untitled:{index:04}");
        let replies = open(&mut server, &uri, "");
        if index < 2 {
            assert!(server.document(&uri).is_some());
        } else {
            rejected(&server, &uri, &replies, BYTE_LIMIT);
        }
    }
    close(&mut server, "untitled:0000");
    open(&mut server, "untitled:0128", "");
    assert!(server.document("untitled:0128").is_some());
    assert!(server.document("untitled:0001").is_some());
}

#[test]
fn newline_metadata_cannot_bypass_the_byte_limit() {
    let source = "\n".repeat(1024);
    let mut server = limited(None, Some(4096));
    let replies = open(&mut server, "u", &source);
    rejected(&server, "u", &replies, BYTE_LIMIT);
    open(&mut server, "u", " ");
    assert_eq!(document(&server, "u").text(), " ");
}

#[test]
fn updates_shrink_grow_and_republish_within_the_shared_budget() {
    let uri = "a";
    let full = "# four";
    let bytes = 2 + 2 * full.len() + LINE;
    let mut server = limited(Some(2), Some(bytes + 2 + LINE));
    open(&mut server, uri, full);
    open(&mut server, uri, full);
    change(&mut server, uri, full);
    open(&mut server, "b", "");
    // Shrinking a document makes room for another document to grow.
    change(&mut server, uri, "");
    change(&mut server, "b", full);
    assert_eq!(document(&server, uri).text(), "");
    assert_eq!(document(&server, "b").text(), full);
    close(&mut server, "b");
    change(&mut server, uri, full);
    assert_eq!(document(&server, uri).text(), full);
}

#[test]
fn rejected_updates_drop_stale_features_release_budget_and_can_recover() {
    let uri = "a";
    let mut server = limited(Some(1), Some(128));
    open(&mut server, uri, "def old\n  1\nend\n");
    assert!(hover_at(&mut server, uri, 0, 5).contains("def old"));
    let replies = change(&mut server, uri, &" ".repeat(1024));
    rejected(&server, uri, &replies, BYTE_LIMIT);
    assert_eq!(
        request(
            &mut server,
            "textDocument/formatting",
            json!({"textDocument": {"uri": uri}})
        )["result"],
        Value::Null
    );
    assert_eq!(
        result(&mut server, "textDocument/hover", uri, 0, 5),
        Value::Null
    );
    assert_eq!(
        request(
            &mut server,
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": uri}})
        )["result"],
        json!([])
    );
    let cleared = close(&mut server, uri);
    assert_eq!(cleared[0]["params"]["diagnostics"], json!([]));
    // Unknown changes follow the same admission path as opens.
    change(&mut server, "b", "");
    assert!(server.document("b").is_some());
    let replies = change(&mut server, "c", "");
    rejected(&server, "c", &replies, COUNT_LIMIT);
    close(&mut server, "b");
    change(&mut server, uri, "x = 1\n");
    assert_eq!(document(&server, uri).text(), "x = 1\n");
    let replies = open(&mut server, uri, &" ".repeat(1024));
    rejected(&server, uri, &replies, BYTE_LIMIT);
    open(&mut server, "b", "");
    assert!(server.document("b").is_some());
}

#[test]
fn zero_limits_reject_even_empty_documents_and_clear_warnings_on_close() {
    for mut server in [limited(Some(0), None), limited(None, Some(0))] {
        let replies = open(&mut server, "", "");
        assert_eq!(replies[0]["params"]["diagnostics"][0]["severity"], 2);
        assert!(server.document("").is_none());
        let replies = close(&mut server, "");
        assert_eq!(replies[0]["params"]["diagnostics"], json!([]));
    }
}

#[test]
fn interrupted_analysis_still_owns_and_releases_its_storage() {
    let token = vibescript::CancellationToken::new();
    token.cancel();
    let mut server = Server::with_options(Options {
        cancellation: token,
        max_documents: Some(1),
        max_document_bytes: Some(128),
        ..Options::default()
    });
    assert!(open(&mut server, "a", "x = 1\n").is_empty());
    let replies = open(&mut server, "b", "");
    rejected(&server, "b", &replies, COUNT_LIMIT);
    close(&mut server, "a");
    open(&mut server, "b", "");
    assert!(server.document("b").is_some());
}

#[test]
fn bounded_session_supports_normal_editor_features() {
    let mut server = limited(Some(4), Some(64 << 10));
    let uri = "file:///tmp/editor.vibe";
    let source = "def add(a: int, b: int) -> int\n  a + b\nend\n\nadd(1, 2)\n";
    assert!(open_diagnostics(&mut server, uri, source).is_empty());
    assert!(hover_at(&mut server, uri, 4, 1).contains("def add"));
    assert!(completion_labels(&mut server, uri, 1, 2).contains_key("a"));
    assert_eq!(
        result(&mut server, "textDocument/definition", uri, 4, 1)["uri"],
        uri
    );
    let params = json!({"textDocument": {"uri": uri}});
    assert_eq!(
        request(&mut server, "textDocument/documentSymbol", params.clone())["result"][0]["name"],
        "add"
    );
    assert!(request(&mut server, "textDocument/formatting", params)["result"].is_array());
    let replies = change(&mut server, uri, &source.replace("add(1, 2)", "add(2, 3)"));
    assert_eq!(replies[0]["params"]["diagnostics"], json!([]));
    close(&mut server, uri);
    assert!(server.document(uri).is_none());
}
