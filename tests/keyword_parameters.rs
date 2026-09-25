//! Keyword parameters are declared after a bare `*` or a rest parameter
//! (ADR-007), and bind by name at runtime whether or not static types are on.

use vibescript::{
    CallOptions, Engine, ErrorKind, Value,
    tooling::{self, ParameterKind},
};

const SEND: &str = "def send_email(to: string, *, cc: string? = nil, retries: int = 3) -> string\n  \"#{to}|#{cc}|#{retries}\"\nend\n";

fn run(source: &str, call: &str) -> vibescript::Result<String> {
    let source = format!("{source}def run -> string\n  {call}\nend\n");
    let script = Engine::new().compile(&source)?;
    let result = script.call("run", &[], CallOptions::default())?;
    Ok(String::from_utf8(result.value.as_bytes().unwrap().to_vec()).unwrap())
}

#[test]
fn keywords_after_a_bare_star_bind_by_name() {
    assert_eq!(
        run(SEND, "send_email(\"a@b.c\", retries: 5)").unwrap(),
        "a@b.c||5"
    );
    assert_eq!(
        run(SEND, "send_email(\"a@b.c\", cc: \"d@e.f\")").unwrap(),
        "a@b.c|d@e.f|3"
    );
    let required = "def need(*, name: string) -> string\n  name\nend\n";
    assert_eq!(run(required, "need(name: \"n\")").unwrap(), "n");
    let error = run(required, "need()").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert!(
        error.message.contains("missing keyword argument name"),
        "{error}"
    );
    // A keyword is never bound by position.
    let error = run(SEND, "send_email(\"a@b.c\", \"d@e.f\")").unwrap_err();
    assert!(
        error.message.contains("unexpected positional arguments"),
        "{error}"
    );
    let error = run(SEND, "send_email(\"a@b.c\", bcc: \"x\")").unwrap_err();
    assert!(
        error.message.contains("unexpected keyword argument bcc"),
        "{error}"
    );
}

#[test]
fn keywords_after_a_rest_parameter_need_no_star() {
    let join = "def join(*items: array<int>, sep: string = \",\") -> string\n  items.map { |i| i.to_s }.join(sep)\nend\n";
    assert_eq!(run(join, "join(1, 2, 3, sep: \"-\")").unwrap(), "1-2-3");
    assert_eq!(run(join, "join(1, 2)").unwrap(), "1,2");
    let untyped = "def pick(*items, key)\n  key\nend\n";
    assert_eq!(run(untyped, "pick(1, key: \"k\")").unwrap(), "k");
}

#[test]
fn typed_keywords_check_their_arguments_and_defaults() {
    let script = Engine::new().compile(SEND).unwrap();
    let result = script
        .call_with_keywords(
            "send_email",
            &[Value::bytes("a")],
            &[("retries".into(), Value::int(1))],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(b"a||1".as_slice()));
    let error = script
        .call_with_keywords(
            "send_email",
            &[Value::bytes("a")],
            &[("retries".into(), Value::bytes("x"))],
            CallOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    let wrong_default = "def f(*, n: int = \"x\") -> string\n  \"#{n}\"\nend\n";
    assert_eq!(run(wrong_default, "f").unwrap_err().kind, ErrorKind::Type);
}

#[test]
fn the_removed_forms_still_run_without_static_types() {
    let source = "def old(a, retries: 2, name:)\n  \"#{a}#{retries}#{name}\"\nend\n";
    assert_eq!(run(source, "old(1, name: \"x\")").unwrap(), "12x");
    let typed = "def old(a, name: string:)\n  name\nend\n";
    assert_eq!(run(typed, "old(1, name: \"y\")").unwrap(), "y");
}

#[test]
fn a_misplaced_star_is_a_syntax_error() {
    for (source, message) in [
        (
            "def f(*, )\nend\n",
            "a bare `*` must be followed by keyword parameters",
        ),
        (
            "def f(*, **opts)\nend\n",
            "a bare `*` must be followed by keyword parameters",
        ),
        (
            "def f(*, a: int, *, b: int)\nend\n",
            "duplicate `*` before keyword parameters",
        ),
        (
            "def f(*items, *, b: int)\nend\n",
            "parameters after a rest parameter are keyword parameters already",
        ),
        (
            "def f(a:, *, b: int)\nend\n",
            "a bare `*` must precede keyword and keyword rest parameters",
        ),
        // After a bare `*`, only the canonical forms parse.
        ("def f(*, b: 3)\nend\n", "expected type name"),
        ("def f(*, b: int:)\nend\n", "expected \")\""),
        // A lone `*` still needs a rest parameter's name.
        ("def f(*)\nend\n", "expected rest parameter name"),
    ] {
        let error = Engine::new().compile(source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax, "{source}");
        assert!(error.message.contains(message), "{source}: {error}");
    }
}

#[test]
fn outlines_report_keywords_after_the_star() {
    let outline = tooling::outline(
        "def f(a: int, *, b: string, c: int = 1)\nend\ndef g(*r: array<int>, d: bool)\nend\n",
    )
    .unwrap();
    let kinds: Vec<Vec<(String, ParameterKind)>> = outline
        .items
        .iter()
        .map(|item| {
            item.function
                .as_ref()
                .unwrap()
                .params
                .iter()
                .map(|param| (param.name.clone(), param.kind))
                .collect()
        })
        .collect();
    assert_eq!(
        kinds,
        [
            vec![
                ("a".into(), ParameterKind::Positional),
                ("b".into(), ParameterKind::Keyword),
                ("c".into(), ParameterKind::Keyword),
            ],
            vec![
                ("r".into(), ParameterKind::Rest),
                ("d".into(), ParameterKind::Keyword),
            ],
        ]
    );
}

#[test]
fn static_types_check_the_new_form_and_reject_the_removed_ones() {
    let mut engine = Engine::new();
    engine.set_static_types(true);
    let script = engine.compile(SEND).unwrap();
    let result = script
        .call("send_email", &[Value::bytes("a")], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(b"a||3".as_slice()));
    let error = engine
        .compile("def f(a: int, retries: 3) -> int\n  a + retries\nend\n")
        .err()
        .unwrap();
    let diagnostic = &error.diagnostics()[0];
    assert_eq!(diagnostic.code.to_string(), "V0414");
    let fixed = diagnostic.fixes[0]
        .apply("def f(a: int, retries: 3) -> int\n  a + retries\nend\n")
        .unwrap();
    assert_eq!(
        fixed,
        "def f(a: int, *, retries: int = 3) -> int\n  a + retries\nend\n"
    );
    assert!(engine.compile(&fixed).is_ok());
}
