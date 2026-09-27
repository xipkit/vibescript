//! Repairs observed while authoring the glue corpus from documentation.

use super::support::{clean, codes, fixed};
use vibescript::{Engine, diagnostic::Applicability};

#[test]
fn fetch_fixes_only_offer_a_non_nullable_collection_element() {
    for source in [
        "def f(m: match_data) -> int; m[1].to_i; end",
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
