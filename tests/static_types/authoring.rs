//! Repairs observed while authoring the glue corpus from documentation.

use super::support::{clean, codes, fixed};
use vibescript::{Engine, diagnostic::Applicability};

#[test]
fn fetch_fixes_only_offer_a_non_nullable_collection_element() {
    for source in [
        "def f(s: string) -> string; s[0].upcase; end",
        "def f(xs: array<int?>) -> int; xs[0] + 1; end",
        "def f(h: hash<string, int?>) -> int; h[\"a\"] + 1; end",
        "def f(h: { a: int? }) -> int; h[\"a\"] + 1; end",
        "def f(xs: array<int>) -> array<int>; xs[0..1]; end",
        "def f(xs: array<int>) -> int | array<int>; xs[0..1]; end",
        "def f(xs: array<int>, slice: range) -> int | array<int>; xs[slice]; end",
        "class C; def [](i: int) -> int?; nil; end; end; C.new[0] + 1",
    ] {
        let diagnostics = codes(source, &["V0107"]);
        assert!(diagnostics[0].fixes.is_empty(), "{source}: {diagnostics:?}");
    }
    for source in [
        "def f(xs: array<int>) -> int; xs[0] + 1; end; f([41])",
        "def f(h: hash<string, int>) -> int; h[\"a\"] + 1; end; f({ a: 41 })",
        "def f(h: { a?: int }) -> int; h[\"a\"] + 1; end; f({ a: 41 })",
        "def f(xs: array<array<int>>) -> int; xs.fetch(0)[0] + 1; end; f([[41]])",
        "def f(m: match_data) -> int; m[1].to_i + 1; end; f(/(41)/.match(\"41\").as(match_data))",
        "def f(m: match_data) -> int; m[\"n\"].to_i + 1; end; f(/(?<n>41)/.match(\"41\").as(match_data))",
    ] {
        let diagnostics = codes(source, &["V0107"]);
        let repaired = fixed(source, &diagnostics[0]);
        clean(&repaired);
        let result = Engine::new()
            .compile(&repaired)
            .unwrap()
            .run(Default::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(42), "{repaired}");
    }
}

#[test]
fn match_fetch_is_required_while_indexing_stays_optional() {
    clean("def f(m: match_data, group: number | string) -> string; m.fetch(group); end");
    clean("def f(m: match_data) -> string?; m[1]; end");
    clean("def f(m: match_data) -> string?; m[\"name\"]; end");
    codes("def f(m: match_data) -> string; m[1]; end", &["V0107"]);
    codes("def f(m: match_data); m.fetch(:name); end", &["V0101"]);
    codes("def f(m: match_data); m.fetch; end", &["V0301"]);
    codes(
        "def f(m: match_data); m.fetch(1, \"default\"); end",
        &["V0301"],
    );
    let source = "m = /(?<n>42)/.match(\"42\").as(match_data); s: string = m[\"n\"]; s";
    let diagnostics = codes(source, &["V0107"]);
    let repaired = fixed(source, &diagnostics[0]);
    let result = Engine::new()
        .compile(&repaired)
        .unwrap()
        .run(Default::default())
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(b"42".as_slice()));
}

#[test]
fn a_missing_enum_member_is_spelled_as_a_valid_when_value() {
    let source = "enum State; Open; InReview; end\n\
        def label(state: State) -> string?\n\
          case state\n\
          when State::Open then \"open\"\n\
          end\n\
        end\n";
    let diagnostics = codes(source, &["V0114"]);
    let message = &diagnostics[0].message;
    let member = message
        .split("does not handle `")
        .nth(1)
        .unwrap()
        .split('`')
        .next()
        .unwrap();
    let repaired = source.replace(
        "when State::Open",
        &format!("when {member} then \"review\"\nwhen State::Open"),
    );
    clean(&repaired);
    let script = Engine::new()
        .compile(&format!("{repaired}\nlabel(State::InReview)"))
        .unwrap();
    assert_eq!(
        script.run(Default::default()).unwrap().value.as_bytes(),
        Some(b"review".as_slice())
    );
}

#[test]
fn foreign_member_spellings_offer_canonical_suggestions() {
    for (source, expected) in [
        ("[1, 2].filter { |n| n > 1 }", "[2]"),
        ("\" hi \".trim", "hi"),
        ("[1, 2].includes(2)", "true"),
        ("\"hi\".includes(\"h\")", "true"),
    ] {
        let diagnostics = codes(source, &["V0203"]);
        let fix = &diagnostics[0].fixes[0];
        assert_eq!(fix.applicability, Applicability::Suggestion);
        let repaired = fix.apply(source).unwrap();
        clean(&repaired);
        let value = Engine::new()
            .compile(&repaired)
            .unwrap()
            .run(Default::default())
            .unwrap()
            .value;
        assert_eq!(value.to_string(), expected);
    }
    clean("class Client; def trim -> int; 1; end; end; Client.new.trim");
    let diagnostics = codes("1.trim", &["V0203"]);
    assert!(diagnostics[0].fixes.is_empty());
}
