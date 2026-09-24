//! Definitions and the document outline.

use super::*;
use std::sync::Arc;

fn character_span(range: Range) -> (u32, u32) {
    (range.start.character, range.end.character)
}

#[test]
fn definition_resolves_top_level_symbols() {
    let mut server = server();
    let uri = "file:///tmp/nav.vibe";
    open(&mut server, uri, NAVIGATION);
    let location = result(&mut server, "textDocument/definition", uri, 20, 4);
    assert_eq!(
        location,
        json!({"uri": uri, "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 10}}})
    );
    // The reference encodes the location as a map, with sorted keys.
    let replies = replies(
        &mut server,
        &message(
            "textDocument/definition",
            Some("2"),
            Some(position(uri, 20, 4)),
        ),
    );
    assert_eq!(
        replies[0].json(),
        r#"{"jsonrpc":"2.0","id":2,"result":{"range":{"end":{"character":10,"line":0},"start":{"character":4,"line":0}},"uri":"file:///tmp/nav.vibe"}}"#
    );
}

#[test]
fn definition_resolves_enum_members() {
    let mut server = server();
    let uri = "file:///tmp/nav-enum.vibe";
    open(&mut server, uri, NAVIGATION);
    assert_eq!(
        definition(&server, uri, "Published").unwrap().start.line,
        16
    );
}

#[test]
fn unknown_definitions_return_an_explicit_null() {
    let mut server = server();
    let uri = "file:///tmp/nav-null.vibe";
    open(&mut server, uri, NAVIGATION);
    let replies = replies(
        &mut server,
        &message(
            "textDocument/definition",
            Some("42"),
            Some(position(uri, 1, 3)),
        ),
    );
    assert_eq!(
        replies[0].json(),
        r#"{"jsonrpc":"2.0","id":42,"result":null}"#
    );
}

#[test]
fn document_symbols_outline_the_declarations() {
    let mut server = server();
    let uri = "file:///tmp/outline.vibe";
    open(&mut server, uri, NAVIGATION);
    let symbols = symbols(&mut server, uri);
    assert_eq!(names(&symbols), ["helper", "Wallet", "Status", "run"]);
    assert_eq!(symbols[0]["kind"], 12);
    assert_eq!(symbols[3]["kind"], 12);
    assert_eq!(symbols[1]["kind"], 5);
    assert_eq!(
        names(symbols[1]["children"].as_array().unwrap()),
        ["balance", "self.empty"]
    );
    assert_eq!(symbols[2]["kind"], 10);
    assert_eq!(symbols[2]["children"].as_array().unwrap().len(), 2);
}

#[test]
fn document_symbols_wire_shape() {
    let lines = text::split_lines("class Wallet\n  def balance()\n    1\n  end\nend\n");
    let child = navigation::symbol("balance", SymbolKind::Method, 1, &lines, Vec::new());
    let leaf = server::symbol_json(&child).encode();
    assert!(!leaf.contains("children"), "{leaf}");
    let parent = navigation::symbol("Wallet", SymbolKind::Class, 0, &lines, vec![child]);
    assert_eq!(
        server::symbol_json(&parent).encode(),
        concat!(
            r#"{"name":"Wallet","kind":5,"#,
            r#""range":{"start":{"line":0,"character":0},"end":{"line":1,"character":15}},"#,
            r#""selectionRange":{"start":{"line":0,"character":0},"end":{"line":0,"character":12}},"#,
            r#""children":[{"name":"balance","kind":6,"#,
            r#""range":{"start":{"line":1,"character":0},"end":{"line":1,"character":15}},"#,
            r#""selectionRange":{"start":{"line":1,"character":0},"end":{"line":1,"character":15}}}]}"#
        )
    );
}

#[test]
fn navigation_survives_a_mid_edit_parse() {
    let mut server = server();
    let uri = "file:///tmp/outline-midedit.vibe";
    open(&mut server, uri, NAVIGATION);
    change(&mut server, uri, &format!("{NAVIGATION}\ndef broken("));
    assert!(definition(&server, uri, "helper").is_some());
}

#[test]
fn the_outline_is_cached_until_the_next_edit() {
    let mut server = server();
    let uri = "file:///tmp/outline-cache.vibe";
    open(
        &mut server,
        uri,
        "def alpha()\n  1\nend\n\ndef beta()\n  2\nend\n",
    );
    assert_eq!(symbols(&mut server, uri).len(), 2);
    let first = document(&server, uri).symbols.get().unwrap().clone();
    symbols(&mut server, uri);
    assert!(Arc::ptr_eq(
        document(&server, uri).symbols.get().unwrap(),
        &first
    ));

    // Shift the declarations while leaving the buffer unparsable: the outline
    // must follow the live lines.
    change(
        &mut server,
        uri,
        "# one\n# two\ndef alpha()\n  1\nend\n\ndef beta()\n  2\nend\n\ndef broken(",
    );
    let shifted = symbols(&mut server, uri);
    assert_eq!(shifted.len(), 2);
    assert_eq!(shifted[0]["range"]["start"]["line"], 2);

    // A buffer declaring none of the symbols must not resurrect them.
    change(&mut server, uri, "def broken(");
    assert!(symbols(&mut server, uri).is_empty());
}

#[test]
fn a_clean_parse_without_declarations_clears_navigation() {
    let mut server = server();
    let uri = "file:///tmp/cleared.vibe";
    open(&mut server, uri, NAVIGATION);
    change(&mut server, uri, "# nothing here\n");
    assert!(definition(&server, uri, "helper").is_none());
    assert!(symbols(&mut server, uri).is_empty());
}

#[test]
fn definition_resolves_setter_methods() {
    let mut server = server();
    let uri = "file:///tmp/setter.vibe";
    open(
        &mut server,
        uri,
        "class Counter\n  def value=(n)\n    @value = n\n  end\nend\n\ndef run()\n  c = Counter.new\n  c.value = 3\nend\n",
    );
    let range = definition(&server, uri, "value").unwrap();
    assert_eq!(range.start.line, 1);
    assert_eq!(character_span(range), (6, 11));
}

#[test]
fn definition_ranges_cover_the_name() {
    let mut server = server();
    let uri = "file:///tmp/namerange.vibe";
    open(&mut server, uri, NAVIGATION);
    assert_eq!(
        character_span(definition(&server, uri, "helper").unwrap()),
        (4, 10)
    );
}

#[test]
fn parent_ranges_enclose_their_children() {
    let mut server = server();
    let uri = "file:///tmp/enclose.vibe";
    open(&mut server, uri, NAVIGATION);
    for symbol in document(&server, uri).symbols() {
        for child in &symbol.children {
            assert!(child.range.end.line <= symbol.range.end.line, "{symbol:?}");
        }
    }
}

#[test]
fn navigation_drops_declarations_missing_from_the_live_buffer() {
    let mut server = server();
    let uri = "file:///tmp/replaced.vibe";
    open(&mut server, uri, NAVIGATION);
    change(&mut server, uri, "def broken(");
    assert!(definition(&server, uri, "helper").is_none());
    assert!(symbols(&mut server, uri).is_empty());
}

#[test]
fn the_outline_includes_modules_constants_and_visibility_prefixed_defs() {
    let mut server = server();
    let uri = "file:///tmp/outline-modules.vibe";
    open(&mut server, uri, MODULE_NAVIGATION);
    let symbols = symbols(&mut server, uri);
    assert_eq!(names(&symbols), ["Billing", "Account", "run"]);
    let billing = &symbols[0];
    assert_eq!(billing["kind"], 2);
    let children = billing["children"].as_array().unwrap();
    assert_eq!(names(children), ["self.code", "LIMIT", "Codes"]);
    assert_eq!(
        children
            .iter()
            .map(|child| child["kind"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [6, 14, 2]
    );
    let codes = children[2]["children"].as_array().unwrap();
    assert_eq!(names(codes), ["self.tag", "PREFIX"]);
    assert_eq!(symbols[1]["kind"], 5);
    assert_eq!(
        names(symbols[1]["children"].as_array().unwrap()),
        ["guard", "shown"]
    );
    // Repeated requests reuse the cached outline.
    let cached = document(&server, uri).symbols.get().unwrap().clone();
    self::symbols(&mut server, uri);
    assert!(Arc::ptr_eq(
        document(&server, uri).symbols.get().unwrap(),
        &cached
    ));
}

#[test]
fn definition_resolves_modules_and_visibility_prefixed_defs() {
    let mut server = server();
    let uri = "file:///tmp/nav-modules.vibe";
    open(&mut server, uri, MODULE_NAVIGATION);
    for (word, line) in [
        ("Billing", 0),
        ("LIMIT", 1),
        ("Codes", 3),
        ("PREFIX", 4),
        ("tag", 6),
        ("code", 11),
        ("guard", 17),
        ("shown", 21),
    ] {
        let range = definition(&server, uri, word).unwrap_or_else(|| panic!("{word}"));
        assert_eq!(range.start.line, line, "{word}");
    }
}

#[test]
fn definitions_use_utf16_offsets() {
    let mut server = server();
    let uri = "file:///tmp/utf16.vibe";
    open(
        &mut server,
        uri,
        "def caf\u{e9}(n)\n  n\nend\n\nx = \"\u{1F600}\" + caf\u{e9}(1)\n",
    );
    // The emoji takes two UTF-16 units, so the call starts at character 11.
    let location = result(&mut server, "textDocument/definition", uri, 4, 12);
    assert_eq!(
        location["range"]["start"],
        json!({"line": 0, "character": 4})
    );
    assert_eq!(location["range"]["end"], json!({"line": 0, "character": 8}));
    assert!(result(&mut server, "textDocument/definition", uri, 4, 6).is_null());
}
