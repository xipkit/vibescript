use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

const DECLARATIONS: &str = r#"
def plain(options)
options
end
class C
property options
def initialize(options={})
@options=options
end
def take(options)
options
end
def self.take(options)
options
end
end
module M
def self.take(options)
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
        ("C.new.take(options:", ")"),
        ("C.new&.take(options:", ")"),
        ("plain {", "}"),
        ("plain do\n", "\nend"),
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
fn parenthesized_methods_keep_keywords_separate_from_positional_options() {
    for receiver in ["C.new", "C", "M"] {
        for call in [
            format!("{receiver}.take(retries:3)"),
            format!("({receiver}.take)(retries:3)"),
            format!("{receiver}&.take(retries:3)"),
            format!("({receiver}.take rescue plain)(retries:3)"),
            format!("(missing rescue {receiver}.take)(retries:3)"),
            format!("{receiver}.take(**{{retries:3}})"),
            format!("{receiver}.take(retries:3) {{raise \"entered\"}}"),
        ] {
            let error = Engine::new()
                .compile(&format!("{DECLARATIONS}\n{call}"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Argument, "{call}");
            assert_eq!(error.message, "missing argument options", "{call}");
        }
    }
}

#[test]
fn functions_constructors_bare_methods_and_forwarding_accept_options() {
    for call in [
        "plain(retries:3)",
        "plain **{retries:3}",
        "(missing rescue plain)(retries:3)",
        "plain(retries:3) {raise \"entered\"}",
        "C.new(retries:3).options",
        "(C.new)(retries:3).options",
        "(missing rescue C.new)(retries:3).options",
        "c=C.new retries:3;c.options",
        "C.send(:new,retries:3).options",
        "C.new.take retries:3",
        "C.take **{retries:3}",
        "M.take retries:3",
        "C.new&.take retries:3",
        "C.new.take retries:3 do\nraise \"entered\"\nend",
        "C.new.send(:take,retries:3)",
        "C.public_send(:take,retries:3)",
        "M.send(:send,:public_send,:take,retries:3)",
    ] {
        assert_eq!(
            result(&format!("{DECLARATIONS}\n{call}")),
            serde_json::json!({"retries":3}),
            "{call}"
        );
    }
}

#[test]
fn implicit_methods_preserve_call_form_through_blocks_and_rescue() {
    for (kind, prefix, receiver) in [
        ("class", "", "C.new"),
        ("class", "self.", "C"),
        ("module", "self.", "C"),
    ] {
        for call in [
            "take(retries:3)",
            "(take)(retries:3)",
            "(missing rescue take)(retries:3)",
            "self.take(retries:3)",
            "take(retries:3) {raise \"entered\"}",
            "[1].map {take(retries:3)}",
        ] {
            let source = format!(
                "{kind} C\ndef {prefix}take(options)\noptions\nend\ndef {prefix}check\n{call}\nend\nend\n{receiver}.check"
            );
            let error = Engine::new()
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Argument, "{source}");
            assert_eq!(error.message, "missing argument options", "{source}");
        }
        for call in [
            "take retries:3",
            "take retries:3 do\nraise \"entered\"\nend",
            "send(:take,retries:3)",
            "self.take retries:3",
        ] {
            let source = format!(
                "{kind} C\ndef {prefix}take(options)\noptions\nend\ndef {prefix}check\n{call}\nend\nend\n{receiver}.check"
            );
            assert_eq!(
                result(&source),
                serde_json::json!({"retries":3}),
                "{source}"
            );
        }
    }
}

#[test]
fn methods_named_call_or_send_keep_ordinary_binding_rules() {
    for method in ["call", "send", "public_send"] {
        let declarations = format!("class C\ndef {method}(options)\noptions\nend\nend");
        let source = format!("{declarations}\nC.new.{method}(retries:3)");
        let error = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument);
        assert_eq!(error.message, "missing argument options");
        assert_eq!(
            result(&format!("{declarations}\nC.new.{method} retries:3")),
            serde_json::json!({"retries":3})
        );
        let forward = if method == "send" {
            "public_send"
        } else {
            "send"
        };
        assert_eq!(
            result(&format!(
                "{declarations}\nC.new.{forward}(:{method},retries:3)"
            )),
            serde_json::json!({"retries":3})
        );
    }
}

#[test]
fn explicit_options_and_matching_keywords_still_bind() {
    for call in [
        "C.new.take({retries:3})",
        "C.take(options:{retries:3})",
        "M.take(*[],**{options:{retries:3}})",
        "(missing rescue C.new.take)(options:{retries:3})",
    ] {
        assert_eq!(
            result(&format!("{DECLARATIONS}\n{call}")),
            serde_json::json!({"retries":3}),
            "{call}"
        );
    }
    for params in ["options={}", "*options"] {
        let source =
            format!("class C\ndef take({params})\noptions\nend\nend\nC.new.take(retries:3)");
        let error = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument);
        assert_eq!(error.message, "unexpected keyword argument retries");
    }
    assert_eq!(
        result(
            "class C\ndef take(options=1,retries:,**rest)\n[options,retries,rest]\nend\nend\nC.new.take(retries:3,extra:4)"
        ),
        serde_json::json!([1,3,{"extra":4}])
    );
}

#[test]
fn typed_options_are_checked_after_the_correct_binding_is_selected() {
    let declarations = "class C\nproperty options\ndef initialize(options:{retries:int})\n@options=options\nend\ndef take(options:{retries:int})\noptions\nend\nend\nc=C.new(retries:3)";
    for call in [
        "c.options",
        "c.take(options:{retries:3})",
        "c.take retries:3",
        "c.send(:take,retries:3)",
    ] {
        assert_eq!(
            result(&format!("{declarations}\n{call}")),
            serde_json::json!({"retries":3})
        );
    }
    for (call, kind) in [
        ("c.take(retries:3)", ErrorKind::Argument),
        ("c.take(retries:\"bad\")", ErrorKind::Argument),
        ("c.take(options:{retries:\"bad\"})", ErrorKind::Type),
        ("c.send(:take,retries:\"bad\")", ErrorKind::Type),
        ("C.new(retries:\"bad\")", ErrorKind::Type),
    ] {
        let error = Engine::new()
            .compile(&format!("{declarations}\n{call}"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, kind, "{call}");
    }
}

#[test]
fn rejected_binding_evaluates_arguments_but_skips_defaults_body_and_block() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |ctx, args| {
        ctx.charge(1)?;
        seen.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(args[0].clone())
    });
    for call in [
        "C.new.take(retries:mark(1),extra:mark(2)) {mark(5)}",
        "(missing rescue C.new.take)(retries:mark(1),extra:mark(2)) {mark(5)}",
    ] {
        events.lock().unwrap().clear();
        let source = format!("class C\ndef take(options=mark(3))\nmark(4)\nend\nend\n{call}");
        assert_eq!(
            engine
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
        assert_eq!(*events.lock().unwrap(), [1, 2]);
    }
    events.lock().unwrap().clear();
    engine
        .compile("nil&.take(retries:mark(1)) {mark(2)}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert!(events.lock().unwrap().is_empty());
}

#[test]
fn rejected_options_release_storage_and_enforce_exact_quotas() {
    let script=Engine::new().compile("class C\ndef take(options)\nraise \"entered\"\nend\nend\ndef attempt(c)\nbegin\nc.take(retries:\"x\"*8192)\nrescue ArgumentError\n42\nend\nend\ndef run(n)\nc=C.new;i=0;while i<n\nattempt(c);i+=1\nend;42\nend").unwrap();
    let mut unbounded = CallOptions::default();
    unbounded.limits.steps = None;
    let once = script
        .call("run", &[Value::int(1)], unbounded.clone())
        .unwrap();
    let many = script.call("run", &[Value::int(64)], unbounded).unwrap();
    assert_eq!(many.value.as_int(), Some(42));
    assert_eq!(once.stats.retained_memory_bytes, 0);
    assert_eq!(many.stats.retained_memory_bytes, 0);
    assert_eq!(once.stats.peak_memory_bytes, many.stats.peak_memory_bytes);
    let mut options = CallOptions::default();
    options.limits.steps = Some(once.stats.steps);
    options.limits.memory_bytes = Some(once.stats.peak_memory_bytes);
    script
        .call("run", &[Value::int(1)], options.clone())
        .unwrap();
    options.limits.steps = Some(once.stats.steps - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(1)], options.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(once.stats.steps);
    options.limits.memory_bytes = Some(once.stats.peak_memory_bytes - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(1)], options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn rejected_calls_release_new_receivers_across_collection_cycles() {
    let script = Engine::new().compile("class C\ndef take(options)\nraise \"entered\"\nend\nend\ndef run(n)\ni=0;while i<n\nbegin\nC.new.take(retries:3)\nrescue ArgumentError\ni+=1\nend\nend;42\nend").unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = None;
    let mut peaks = Vec::new();
    for count in [64, 128, 512] {
        let output = script
            .call("run", &[Value::int(count)], options.clone())
            .unwrap();
        assert_eq!(output.value.as_int(), Some(42));
        assert_eq!(output.stats.retained_memory_bytes, 0);
        peaks.push(output.stats.peak_memory_bytes);
    }
    assert_eq!(peaks[0], peaks[1]);
    assert_eq!(peaks[1], peaks[2]);
}

#[test]
fn cancellation_during_keywords_prevents_binding_rescue_and_cleanup() {
    for call in [
        "C.new.take(retries:stop())",
        "C.new.send(:take,retries:stop())",
    ] {
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
        let source = format!(
            "class C\ndef take(options=after())\nafter()\nend\nend\nbegin\n{call}\nrescue RuntimeError | LimitError\nafter()\nensure\nafter()\nend"
        );
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions {
                cancellation: token,
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}
