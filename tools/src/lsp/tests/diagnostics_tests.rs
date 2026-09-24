//! Diagnostics: compile errors in the reference's form, checker findings, and
//! the caches a publish maintains.

use super::*;
use std::sync::Arc;

fn diagnostics(source: &str) -> Vec<Value> {
    open_diagnostics(&mut server(), "file:///tmp/diagnostics.vibe", source)
}

fn range(diagnostic: &Value) -> (i64, i64, i64, i64) {
    let range = &diagnostic["range"];
    (
        range["start"]["line"].as_i64().unwrap(),
        range["start"]["character"].as_i64().unwrap(),
        range["end"]["line"].as_i64().unwrap(),
        range["end"]["character"].as_i64().unwrap(),
    )
}

#[test]
fn clean_sources_have_no_diagnostics() {
    assert!(diagnostics("def run()\n  1\nend\n").is_empty());
    assert!(diagnostics("def double(x)\n  x * 2\nend\n\ndouble(3)\n").is_empty());
}

#[test]
fn parse_errors_are_errors_from_the_server() {
    let diagnostics = diagnostics("def run(\n  1\nend\n");
    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0]["severity"], 1);
    assert_eq!(diagnostics[0]["source"], "vibes-lsp");
    assert!(!diagnostics[0]["message"].as_str().unwrap().is_empty());
}

#[test]
fn diagnostics_span_the_offending_token() {
    // "123" is the offending token: line 1, columns 5-7 (0-indexed 4-7).
    let diagnostics = diagnostics("def 123()\n  1\nend\n");
    assert_eq!(range(&diagnostics[0]), (0, 4, 0, 7));
    // The parser's own message, without the rendered location.
    assert_eq!(diagnostics[0]["message"], "expected name");
}

#[test]
fn diagnostics_at_the_end_of_input_still_move_forward() {
    let diagnostics = diagnostics("def run()\n  x = [1,\nend\n");
    assert!(!diagnostics.is_empty());
    for diagnostic in &diagnostics {
        let (start_line, start, end_line, end) = range(diagnostic);
        assert!(
            end_line > start_line || (end_line == start_line && end > start),
            "{diagnostic}"
        );
    }
    let diagnostics = self::diagnostics("def run(");
    let (line, start, _, end) = range(&diagnostics[0]);
    assert_eq!((line, start, end), (0, 8, 9));
}

#[test]
fn diagnostics_use_utf16_character_offsets() {
    // Each emoji is one character but two UTF-16 units. The offending "2" is
    // at character 16 (1-indexed) on line 2, after two such characters.
    let diagnostics = diagnostics("def run()\n  x = [\"\u{1F600}\u{1F600}\", 1 2]\nend\n");
    assert_eq!(range(&diagnostics[0]), (1, 17, 1, 18));
}

#[test]
fn duplicate_functions_are_reported_at_the_duplicate() {
    // The reference reports this after parsing, at the document start; the
    // port's parser reports it at the second declaration's name.
    let diagnostics = diagnostics("def run()\n  1\nend\n\ndef run()\n  2\nend\n");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(range(&diagnostics[0]), (4, 7, 4, 8));
    assert_eq!(
        diagnostics[0]["message"],
        "duplicate or reserved function name"
    );
}

#[test]
fn errors_without_positions_are_reported_at_the_document_start() {
    // Like the reference, documents over 1 MiB are not analyzed.
    let source = format!("{}x", " ".repeat(1 << 20));
    let diagnostics = diagnostics(&source);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(range(&diagnostics[0]), (0, 0, 0, 1));
    assert_eq!(
        diagnostics[0]["message"],
        "source exceeds maximum size (1048577 > 1048576 bytes)"
    );
    // Past a raised limit, the compiler's own guard reports without a position.
    let mut server = Server::with_options(Options {
        max_source_bytes: usize::MAX,
        ..Options::default()
    });
    let source = format!("{}x", " ".repeat(8 << 20));
    let diagnostics = open_diagnostics(&mut server, "file:///tmp/huge.vibe", &source);
    assert_eq!(range(&diagnostics[0]), (0, 0, 0, 1));
    assert_eq!(diagnostics[0]["message"], "source exceeds 8 MiB");
}

#[test]
fn analysis_stops_at_its_deadline() {
    let mut server = Server::with_options(Options {
        timeout: std::time::Duration::ZERO,
        ..Options::default()
    });
    let diagnostics = open_diagnostics(
        &mut server,
        "file:///tmp/slow.vibe",
        "def run
  1
end
",
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], 2);
    assert_eq!(
        diagnostics[0]["message"],
        "compilation stopped: execution deadline exceeded"
    );
}

#[test]
fn checker_errors_are_reported_where_the_checker_places_them() {
    let diagnostics = diagnostics("def run(x: int) -> int\n  x + \"a\"\nend\n");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["severity"], 1);
    assert_eq!(diagnostics[0]["source"], "vibes-lsp");
    assert_eq!(
        diagnostics[0]["message"],
        "Return value: expected int, got string"
    );
    assert_eq!(range(&diagnostics[0]), (1, 2, 1, 3));
    // Unused declarations are checked too, as by `vibes check`.
    let diagnostics = self::diagnostics("7\ndef unused(n: string) -> int\n  n\nend\n");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(range(&diagnostics[0]).0, 2);
}

#[test]
fn incomplete_analysis_is_information() {
    let diagnostics = diagnostics("def incomplete\n  [1].map! { _1 }\nend\n");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], 3);
    assert_eq!(
        diagnostics[0]["message"],
        "Analysis of this expression is not implemented"
    );
}

#[test]
fn checks_stop_within_their_limits() {
    let mut server = Server::with_options(Options {
        limits: vibescript::Limits {
            steps: Some(50),
            ..vibescript::Limits::default()
        },
        ..Options::default()
    });
    let source = "def run(x: int) -> int\n  y = x + 1\n  z = y * 2\n  z - 3\nend\n";
    let diagnostics = open_diagnostics(&mut server, "file:///tmp/limits.vibe", source);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], 2);
    assert_eq!(range(&diagnostics[0]), (0, 0, 0, 1));
    assert!(
        diagnostics[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("static check stopped: step quota exceeded"),
        "{diagnostics:?}"
    );
    // A cancelled server stops publishing a check it cannot finish.
    let token = vibescript::CancellationToken::new();
    let mut server = Server::with_options(Options {
        cancellation: token.clone(),
        ..Options::default()
    });
    token.cancel();
    assert!(open(&mut server, "file:///tmp/cancelled.vibe", source).is_empty());
}

// A WASI guest sees only its preopened directories, which exclude the
// system temporary directory.
#[cfg(not(target_os = "wasi"))]
#[test]
fn required_files_resolve_from_the_document_directory() {
    let directory =
        std::env::temp_dir().join(format!("vibes lsp require {} é", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("helpers.vibe"),
        "export def double(x: int) -> int\n  x * 2\nend\nexport def broken -> int\n  \"no\"\nend\n",
    )
    .unwrap();
    let mut encoded = String::from("file://");
    for byte in directory.join("main.vibe").to_string_lossy().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'.' | b'-' | b'_' => {
                encoded.push(*byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    let mut server = server();
    let source = "helpers = require(\"helpers\")\nhelpers.double(2)\n";
    let diagnostics = open_diagnostics(&mut server, &encoded, source);
    // The module resolves, and its own error belongs to its own document.
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let missing = open_diagnostics(
        &mut server,
        "untitled:Untitled-1",
        "helpers = require(\"helpers\")\n",
    );
    assert_eq!(missing.len(), 1);
    assert_eq!(
        missing[0]["message"],
        "\"require\": require: module paths not configured"
    );
    // Hosts can name the directories themselves.
    let mut server = Server::with_options(Options {
        module_paths: Some(vec![directory.clone()]),
        ..Options::default()
    });
    let found = open_diagnostics(&mut server, "untitled:Untitled-1", source);
    assert!(found.is_empty(), "{found:?}");
    std::fs::remove_dir_all(&directory).unwrap();
}

#[test]
fn a_parse_that_fails_keeps_navigation_but_a_missing_source_drops_it() {
    let mut server = server();
    let uri = "file:///tmp/too-large.vibe";
    let source = "def old\n  1\nend\n";
    assert!(open_diagnostics(&mut server, uri, source).is_empty());
    let document = document(&server, uri);
    assert!(document.program.is_some() && document.compiled.is_some());
    // The completion index is built lazily, not on the diagnostics path.
    assert!(document.completion.get().is_none());
    completion_labels(&mut server, uri, 1, 2);
    assert!(self::document(&server, uri).completion.get().is_some());

    let oversized = format!("{source}{}", " ".repeat(8 << 20));
    let replies = change(&mut server, uri, &oversized);
    assert!(
        !replies[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let document = self::document(&server, uri);
    assert!(
        document.program.is_none(),
        "oversized publish kept a stale program"
    );
    assert!(
        document.compiled.is_none(),
        "oversized publish kept a stale script"
    );
    assert!(
        document.completion.get().is_none(),
        "oversized publish kept a stale index"
    );
}

#[test]
fn republishing_identical_text_skips_analysis() {
    let mut server = server();
    let uri = "file:///tmp/unchanged.vibe";
    let source = "def helper(n)\n  n\nend\n";
    assert!(open_diagnostics(&mut server, uri, source).is_empty());
    let program = document(&server, uri).program.clone().unwrap();
    let replies = change(&mut server, uri, source);
    assert!(
        replies[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(Arc::ptr_eq(
        document(&server, uri).program.as_ref().unwrap(),
        &program
    ));

    let edited = format!("{source}def broken(\n");
    let replies = change(&mut server, uri, &edited);
    assert!(
        !replies[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A failed parse outlines the sections that still parse.
    let partial = document(&server, uri).program.clone().unwrap();
    assert!(!Arc::ptr_eq(&partial, &program));
    assert_eq!(partial.items[0].name, "helper");

    // Reverting misses the cache, since the last analysis saw the edit.
    let replies = change(&mut server, uri, source);
    assert!(
        replies[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(!Arc::ptr_eq(
        document(&server, uri).program.as_ref().unwrap(),
        &partial
    ));
}

#[test]
fn did_open_publishes_diagnostics_for_the_opened_document() {
    let mut server = server();
    let replies = open(&mut server, "file:///tmp/test.vibe", "def run(\n  1\nend\n");
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(replies[0]["params"]["uri"], "file:///tmp/test.vibe");
    assert!(
        !replies[0]["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
