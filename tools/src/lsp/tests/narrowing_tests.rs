//! Member completion narrowed to the receiver's kind (the reference's
//! `lsp_member_narrowing_test.go`).

use super::super::completion::narrowed_entries;
use super::super::docs::runtime_members;
use super::*;

fn narrowed(source: &str, line: i64, character: i64) -> Option<Vec<String>> {
    let entries = narrowed_entries(source, &text::split_lines(source), line, character)?;
    Some(entries.iter().map(|entry| entry.label.clone()).collect())
}

/// A string receiver was offered money and temporal methods; completion now
/// narrows to the receiver's kind when the syntax decides it.
#[test]
fn completion_narrows_to_the_receiver_kind() {
    for (source, line, character, receiver) in [
        ("def f(s: string)\n  s.\nend", 1, 4, "string"),
        ("def f(s: string)\n  s.up\nend", 1, 6, "string"),
        ("def f(items: array<int>)\n  items.\nend", 1, 8, "array"),
        ("def f(m: money)\n  m.\nend", 1, 4, "money"),
        ("x = \"abc\".\n", 0, 10, "string"),
        ("x = [1].\n", 0, 8, "array"),
        ("x = ({a: 1}).\n", 0, 13, "hash"),
        ("x = 1.\n", 0, 6, "int"),
    ] {
        let labels =
            narrowed(source, line, character).unwrap_or_else(|| panic!("{source} fell back"));
        assert_eq!(labels.len(), runtime_members()[receiver].len(), "{source}");
    }
}

/// Narrowing wrongly would hide members that apply, so every receiver the
/// syntax does not decide falls back to the full union.
#[test]
fn unresolved_receivers_fall_back_to_the_full_union() {
    for (source, line, character) in [
        ("def f(x)\n  x.\nend", 1, 4),
        ("def f(s: string?)\n  s.\nend", 1, 4),
        ("def f(s: string | int)\n  s.\nend", 1, 4),
        ("def f(u: User)\n  u.\nend", 1, 4),
        ("def f()\n  x = 1\n  x.\nend", 2, 4),
        ("def f()\n  build().\nend", 1, 10),
        ("def f()\n  x = 1\nend", 1, 7),
    ] {
        assert!(narrowed(source, line, character).is_none(), "{source}");
    }
}

/// The narrowed list keeps every member of its kind, including the
/// universal helpers, and nothing from other kinds.
#[test]
fn narrowed_lists_keep_every_member_of_the_kind() {
    let labels = narrowed("def f(s: string)\n  s.\nend", 1, 4).unwrap();
    for want in [
        "upcase",
        "split",
        "length",
        "nil?",
        "respond_to?",
        "eql?",
        "tap",
    ] {
        assert!(labels.iter().any(|label| label == want), "{want}");
    }
    for unwanted in ["amount", "cents", "ago", "before"] {
        assert!(!labels.iter().any(|label| label == unwanted), "{unwanted}");
    }
}

/// Narrowed items keep the union items' shape: a method kind and the
/// receiver kind as their detail.
#[test]
fn narrowed_items_keep_their_shape() {
    let source = "def f(s: string)\n  s.\nend";
    let entries = narrowed_entries(source, &text::split_lines(source), 1, 4).unwrap();
    assert!(!entries.is_empty());
    for entry in &entries {
        assert_eq!(entry.kind, CompletionKind::Method);
        assert_eq!(entry.detail, "string");
    }
    // Through the protocol, a narrowed list replaces the union.
    let mut server = server();
    let uri = "file:///tmp/narrow.vibe";
    open(&mut server, uri, source);
    let labels = completion_labels(&mut server, uri, 1, 4);
    assert_eq!(labels.len(), entries.len());
    assert_eq!(labels["upcase"]["detail"], "string");
}
