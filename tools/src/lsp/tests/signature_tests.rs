//! Signature help.

use super::super::signature::{enclosing_call, parenless_call};
use super::*;
use vibescript::tooling::{Parameter, ParameterKind};

fn help(source: &str, line: i64, character: i64) -> Value {
    let mut server = server();
    let uri = "file:///tmp/sig.vibe";
    open(&mut server, uri, source);
    result(
        &mut server,
        "textDocument/signatureHelp",
        uri,
        line,
        character,
    )
}

#[test]
fn signature_help_for_user_functions() {
    let result = help(
        "def charge(amount: int, currency = \"USD\", note: string? = nil) -> money\n  money_cents(amount, currency)\nend\n\ndef run()\n  charge(100, \"USD\")\nend\n",
        5,
        14,
    );
    assert_eq!(
        result,
        json!({
            "activeParameter": 1,
            "activeSignature": 0,
            "signatures": [{
                "label": "charge(amount: int, currency = …, note: string? = …) -> money",
                "parameters": [{"label": "amount: int"}, {"label": "currency = …"}, {"label": "note: string? = …"}],
            }],
        })
    );
}

#[test]
fn keyword_parameters_render_after_a_bare_star() {
    for source in [
        "def configure(host:, port: 8080, scheme: \"https\")\n  host\nend\n\ndef run()\n  configure(host: \"a\")\nend\n",
        "def configure(*, host, port = 8080, scheme = \"https\")\n  host\nend\n\ndef run()\n  configure(host: \"a\")\nend\n",
    ] {
        let result = help(source, 5, 18);
        let label = result["signatures"][0]["label"].as_str().unwrap();
        assert_eq!(label, "configure(*, host, port = …, scheme = …)");
        assert_eq!(
            result["signatures"][0]["parameters"],
            json!([{"label": "host"}, {"label": "port = …"}, {"label": "scheme = …"}])
        );
    }
    let result = help(
        "def join(*items: array<int>, sep: string = \",\") -> string\n  \"\"\nend\n\ndef run()\n  join(1, sep: \"-\")\nend\n",
        5,
        10,
    );
    let label = result["signatures"][0]["label"].as_str().unwrap();
    assert_eq!(label, "join(*items: array<int>, sep: string = …) -> string");
}

#[test]
fn param_labels_follow_declaration_syntax() {
    let param = |name: &str, kind, type_annotation: Option<&str>, default| Parameter {
        name: name.to_owned(),
        kind,
        type_annotation: type_annotation.map(str::to_owned),
        default,
        instance: false,
    };
    for (param, want) in [
        (param("host", ParameterKind::Keyword, None, false), "host"),
        (
            param("port", ParameterKind::Keyword, None, true),
            "port = …",
        ),
        (
            param("count", ParameterKind::Positional, None, true),
            "count = …",
        ),
        (
            param("amount", ParameterKind::Positional, Some("int"), false),
            "amount: int",
        ),
        (
            param("name", ParameterKind::Keyword, Some("string"), false),
            "name: string",
        ),
        (
            param("name", ParameterKind::Keyword, Some("string"), true),
            "name: string = …",
        ),
        (
            param("rest", ParameterKind::Rest, Some("array<int>"), false),
            "*rest: array<int>",
        ),
        (
            param("options", ParameterKind::KeywordRest, None, false),
            "**options",
        ),
    ] {
        assert_eq!(hover::param_label(&param), want);
    }
}

#[test]
fn signature_help_for_builtins() {
    let result = help("def run()\n  money_cents(\nend\n", 1, 14);
    let label = result["signatures"][0]["label"].as_str().unwrap();
    assert!(
        label.contains("money_cents(cents: int, currency: string) -> money"),
        "{label}"
    );
    assert_eq!(result["activeParameter"], 0);
}

#[test]
fn signature_help_outside_a_call_returns_an_explicit_null() {
    let mut server = server();
    let uri = "file:///tmp/signo.vibe";
    open(&mut server, uri, "def run()\n  x = 1\nend\n");
    let replies = replies(
        &mut server,
        &message(
            "textDocument/signatureHelp",
            Some("32"),
            Some(position(uri, 1, 7)),
        ),
    );
    assert_eq!(
        replies[0].json(),
        r#"{"jsonrpc":"2.0","id":32,"result":null}"#
    );
}

#[test]
fn enclosing_calls_skip_nested_structure_strings_and_comments() {
    let catalog = Catalog::new();
    for (source, character, want) in [
        ("charge(", 7, Some(("charge", 0))),
        ("charge(1, ", 10, Some(("charge", 1))),
        ("outer(inner(1, 2), ", 12, Some(("inner", 0))),
        ("outer(inner(1, 2), ", 19, Some(("outer", 1))),
        ("charge(1)", 9, None),
        ("x = 1", 5, None),
        ("(1 + 2, ", 8, None),
        ("charge([1, 2], ", 15, Some(("charge", 1))),
        ("charge({a: 1, b: 2}, ", 21, Some(("charge", 1))),
        ("charge(\"1,00\", ", 15, Some(("charge", 1))),
        ("charge(\"a)b\", ", 14, Some(("charge", 1))),
        ("charge('1,00', ", 15, Some(("charge", 1))),
        ("charge('a)b', ", 14, Some(("charge", 1))),
        ("charge([1, ", 11, Some(("charge", 0))),
        ("JSON.parse(", 11, Some(("JSON.parse", 0))),
        ("price.format(", 13, None),
        ("# money_cents(", 14, None),
        ("charge (100, ", 13, Some(("charge", 1))),
        ("charge(\"#\", ", 12, Some(("charge", 1))),
        ("charge('#', ", 12, Some(("charge", 1))),
    ] {
        let got = enclosing_call(&catalog, &text::split_lines(source), 0, character);
        let got = got
            .as_ref()
            .map(|(callee, param)| (callee.as_str(), *param));
        assert_eq!(got, want, "{source}");
    }
}

#[test]
fn parenless_calls_count_top_level_commas() {
    for (source, character, want) in [
        ("assert true, ", 13, Some(("assert", 1))),
        ("assert true, 'a,b'", 18, Some(("assert", 1))),
        ("assert true, 'a#b', ", 20, Some(("assert", 2))),
        ("# assert true, ", 15, None),
        ("puts true, ", 11, None),
    ] {
        let got = parenless_call(&text::split_lines(source), 0, character);
        let got = got
            .as_ref()
            .map(|(callee, param)| (callee.as_str(), *param));
        assert_eq!(got, want, "{source}");
    }
}

#[test]
fn signature_help_for_parenless_assert() {
    let result = help("def run()\n  assert 1 == 1, \"ok\"\nend\n", 1, 17);
    assert_eq!(
        result["signatures"][0]["label"],
        builtin_signature("assert")
    );
    assert_eq!(result["activeParameter"], 1);
}

#[test]
fn signature_help_for_qualified_builtins() {
    let source = "def run()\n  JSON.parse_as(\"{}\", { name: string })\nend\n";
    let cursor = text::split_lines(source)[1].find("{ name").unwrap() as i64;
    let result = help(source, 1, cursor);
    assert_eq!(
        result["signatures"][0]["label"],
        builtin_signature("JSON.parse_as")
    );
    assert_eq!(result["activeParameter"], 1);
}

#[test]
fn signature_help_uses_the_last_compiled_functions_and_aliases() {
    let mut server = server();
    let uri = "file:///tmp/sig-alias.vibe";
    open(
        &mut server,
        uri,
        "def charge(amount: int)\n  amount\nend\nalias bill charge\n",
    );
    change(
        &mut server,
        uri,
        "def charge(amount: int)\n  amount\nend\nalias bill charge\nbill(\n",
    );
    let result = result(&mut server, "textDocument/signatureHelp", uri, 4, 5);
    assert_eq!(result["signatures"][0]["label"], "bill(amount: int)");
}
