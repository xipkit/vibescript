//! Keyword parameters are declared after a bare `*` or a rest parameter
//! (ADR-007), and bind by name at runtime whether or not static types are on.

mod common;

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
    // A call that misses a keyword, passes one by position or names an
    // unknown one is refused at compile time.
    for (source, call, code, at) in [
        (required, "need()", "V0303", "need()"),
        (
            SEND,
            "send_email(\"a@b.c\", \"d@e.f\")",
            "V0301",
            "send_email(",
        ),
        (SEND, "send_email(\"a@b.c\", bcc: \"x\")", "V0302", "bcc"),
    ] {
        let program = format!("{source}def run -> string\n  {call}\nend\n");
        let error = common::static_engine().compile(&program).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{call}");
        let start = program.rfind(call).unwrap() + call.find(at).unwrap();
        assert_eq!(error.diagnostics()[0].span.start, start, "{call}");
    }
    // A host's call binds by name when it starts, and is refused the same way.
    let need = Engine::new().compile(required).unwrap();
    let error = need
        .call_with_keywords("need", &[], &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert!(
        error.message.contains("missing keyword argument name"),
        "{error}"
    );
    let send = Engine::new().compile(SEND).unwrap();
    // A keyword is never bound by position.
    let error = send
        .call(
            "send_email",
            &[Value::bytes("a@b.c"), Value::bytes("d@e.f")],
            CallOptions::default(),
        )
        .unwrap_err();
    assert!(
        error.message.contains("unexpected positional arguments"),
        "{error}"
    );
    let error = send
        .call_with_keywords(
            "send_email",
            &[Value::bytes("a@b.c")],
            &[("bcc".into(), Value::bytes("x"))],
            CallOptions::default(),
        )
        .unwrap_err();
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
    let untyped = "def pick(*items: array<int>, key: string) -> string\n  key\nend\n";
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
    let run = |source: &str, call: &str| {
        let source = format!("{source}def run -> string\n  {call}\nend\n");
        let script = common::gradual_engine().compile(&source).unwrap();
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        String::from_utf8(result.value.as_bytes().unwrap().to_vec()).unwrap()
    };
    let source = "def old(a, retries: 2, name:)\n  \"#{a}#{retries}#{name}\"\nend\n";
    assert_eq!(run(source, "old(1, name: \"x\")"), "12x");
    let typed = "def old(a, name: string:)\n  name\nend\n";
    assert_eq!(run(typed, "old(1, name: \"y\")"), "y");
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
        // `name:` declares no keyword parameter in the canonical grammar.
        ("def f(a:, *, b: int)\nend\n", "expected type name"),
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

#[test]
fn nil_and_tuple_types_after_a_colon_are_positional_parameters() {
    for (source, call, expected) in [
        (
            "def f(x: nil) -> string\n  x.to_s + \"!\"\nend\n",
            "f(nil)",
            "!",
        ),
        (
            "def f(pair: [int, string]) -> string\n  pair[1] * pair[0]\nend\n",
            "f([2, \"ab\"])",
            "abab",
        ),
        (
            "enum E\n  A\nend\ndef f(pair: [E, string]) -> string\n  pair[0].to_s + pair[1]\nend\n",
            "f([E::A, \"b\"])",
            "E::Ab",
        ),
    ] {
        for static_types in [false, true] {
            let program = format!("{source}def run -> string\n  {call}\nend\n");
            let mut engine = Engine::new();
            engine.set_static_types(static_types);
            let script = engine
                .compile(&program)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            let result = script.call("run", &[], CallOptions::default()).unwrap();
            assert_eq!(
                result.value.as_bytes(),
                Some(expected.as_bytes()),
                "{source}"
            );
        }
        let outline = tooling::outline(source).unwrap();
        let function = outline.items.last().unwrap().function.as_ref().unwrap();
        assert_eq!(
            function.params[0].kind,
            ParameterKind::Positional,
            "{source}"
        );
    }
}
