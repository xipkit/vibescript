mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

const DECLARATIONS: &str = r#"
def plain(options: any) -> any
options
end
def wrap(&block: () -> any) -> any
yield
end
class C
property options: any
def initialize(options: hash<string, int> = {})
@options=options
end
def take(options: any) -> any
options
end
def self.take(options: any) -> any
options
end
end
module M
def self.take(options: any) -> any
options
end
end
"#;

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn call_and_ternary_nesting_reach_the_syntax_guard() {
    for (open, close) in [
        ("plain(", ")"),
        ("C.new.take(", ")"),
        ("C.new&.take(", ")"),
        ("wrap {", "}"),
        ("true ? ", " : 0"),
        ("true ? 0 : ", ""),
    ] {
        for depth in [1, 8, 16] {
            let source = format!(
                "{DECLARATIONS}\n{}1{}",
                open.repeat(depth),
                close.repeat(depth)
            );
            Engine::new().compile(&source).unwrap();
        }
        let source = format!(
            "{DECLARATIONS}\n{}1{}",
            open.repeat(1100),
            close.repeat(1100)
        );
        let error = Engine::new().compile(&source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax, "{open}");
        assert!(
            error.message.contains("nesting too deep"),
            "{open}: {error}"
        );
    }
}

#[test]
fn keyword_arguments_never_bind_positional_options() {
    // Keywords bind only declared keyword parameters, whatever the call
    // form, so passing them to an options parameter is refused before
    // anything runs.
    for (call, code, text) in [
        ("plain(retries:3)", "V0301", "plain"),
        ("plain retries:3", "V0301", "plain"),
        ("plain **{retries:3}", "V0301", "plain"),
        ("plain(retries:3) {raise \"entered\"}", "V0301", "plain"),
        ("C.new.take(retries:3)", "V0301", "take"),
        ("C.new.take retries:3", "V0301", "take"),
        ("(C.new.take)(retries:3)", "V0301", "take"),
        ("C.new&.take(retries:3)", "V0301", "take"),
        ("C.new&.take retries:3", "V0301", "take"),
        ("(C.new.take rescue plain)(retries:3)", "V0301", "take"),
        ("C.new.take(**{retries:3})", "V0301", "take"),
        ("C.new.take(retries:3) {raise \"entered\"}", "V0301", "take"),
        ("C.take(retries:3)", "V0301", "take"),
        ("C.take **{retries:3}", "V0301", "take"),
        ("C.take(options:{retries:3})", "V0301", "take"),
        ("M.take(retries:3)", "V0301", "take"),
        ("M.take retries:3", "V0301", "take"),
        ("C.new(retries:3).options", "V0302", "retries:"),
        ("(C.new)(retries:3).options", "V0302", "retries:"),
        ("c=C.new retries:3;c.options", "V0302", "retries:"),
    ] {
        let source = format!("{DECLARATIONS}\n{call}");
        let error = common::static_engine().compile(&source).err().unwrap();
        let first = &error.diagnostics()[0];
        assert_eq!(first.code.to_string(), code, "{call}");
        assert_eq!(&source[first.span.start..first.span.end], text, "{call}");
    }
    // Calls inside a class body or its singleton methods follow the same
    // rule.
    for (kind, prefix, receiver) in [
        ("class", "", "C.new"),
        ("class", "self.", "C"),
        ("module", "self.", "C"),
    ] {
        for call in [
            "take(retries:3)",
            "take retries:3",
            "(take)(retries:3)",
            "self.take(retries:3)",
            "self.take retries:3",
            "[1].map {take(retries:3)}",
        ] {
            let source = format!(
                "{kind} C\ndef {prefix}take(options: any) -> any\noptions\nend\ndef {prefix}check -> any\n{call}\nend\nend\n{receiver}.check"
            );
            let error = common::static_engine().compile(&source).err().unwrap();
            assert_eq!(error.diagnostics()[0].code.to_string(), "V0301", "{source}");
        }
    }
    // A default or a rest parameter does not take keywords either.
    for params in ["options: hash<string, int> = {}", "*options: array<any>"] {
        let source =
            format!("class C\ndef take({params}) -> any\noptions\nend\nend\nC.new.take(retries:3)");
        let error = common::static_engine().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0302"], "{params}");
    }
}

#[test]
fn explicit_options_and_matching_keywords_still_bind() {
    for call in [
        "C.new.take({retries:3})",
        "M.take(*[],**{options:{retries:3}})",
    ] {
        assert_eq!(
            result(&format!("{DECLARATIONS}\n{call}")),
            serde_json::json!({"retries":3}),
            "{call}"
        );
    }
    assert_eq!(
        result(
            "class C\ndef take(options: int = 1, *, retries: int, **rest: hash<string, int>) -> array<int | hash<string, int>>\n[options,retries,rest]\nend\nend\nC.new.take(retries:3,extra:4)"
        ),
        serde_json::json!([1,3,{"extra":4}])
    );
}

#[test]
fn safe_navigation_on_nil_skips_keyword_arguments_and_blocks() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |ctx, args| {
        ctx.charge(1)?;
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(args[0].clone())
    });
    engine
        .compile("nil&.take(retries:mark(1)) {mark(2)}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn cancellation_during_keywords_prevents_binding_rescue_and_cleanup() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let effects = Arc::new(AtomicUsize::new(0));
    let seen = effects.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancel.cancel();
        Ok(Value::nil())
    });
    engine.register("after", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let source = "class C\ndef take(*, retries: any = after()) -> any\nafter()\nend\nend\nbegin\nC.new.take(retries:stop())\nrescue RuntimeError | LimitError\nafter()\nensure\nafter()\nend";
    let error = engine
        .compile(source)
        .unwrap()
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}
