mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

const DECLARATIONS: &str = "enum Status\nDraft\nHTTPServer\nDone\nend\nenum Review\nDraft\nend\n";

fn evaluate(body: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(&format!("{DECLARATIONS}{body}"))
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn nominal_values_support_reflection_collections_and_serialization() {
    assert_eq!(
        evaluate(
            "[Status.name,Status::HTTPServer.name,Status::HTTPServer.symbol,Status::HTTPServer.enum==Status,Status::Draft==Review::Draft,Status::Draft==Status::Draft]"
        ),
        serde_json::json!(["Status", "HTTPServer", "http_server", true, false, true])
    );
    // A member never equals a symbol, so comparing them is refused.
    let source = format!("{DECLARATIONS}Status::Draft==:draft");
    let error = common::static_engine().compile(&source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find(":draft").unwrap()
    );
    assert_eq!(
        evaluate(
            "[Status::Draft.to_s,Status.inspect,\"#{Status::Draft}\",[Status::Draft,Status::Done].to_s,[Status::Draft,Status::Done].join(\" / \"),JSON.stringify({state:Status::HTTPServer}),[Status::Draft,Status::Draft,Review::Draft].uniq.length]"
        ),
        serde_json::json!([
            "Status::Draft",
            "<Enum Status>",
            "Status::Draft",
            "[Status::Draft, Status::Done]",
            "Status::Draft / Status::Done",
            "{\"state\":\"http_server\"}",
            2
        ])
    );
    for (name, expected) in [
        ("HTTPServer", "http_server"),
        ("__A__B__", "a_b_"),
        ("_", ""),
        ("Ready?", "ready?"),
        ("İ", "i"),
        ("ΣState", "σ_state"),
        ("ǅState", "ǆ_state"),
        ("a١B", "a١_b"),
        ("enum", "enum"),
    ] {
        let output = Engine::new()
            .compile(&format!("enum État\n{name}\nend\nÉtat::{name}.symbol"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(output.value.as_bytes(), Some(expected.as_bytes()));
        assert_eq!(output.value.type_name(), "symbol");
    }
}

#[test]
fn declarations_preserve_forward_lookup_shadowing_and_identifier_boundaries() {
    for name in ["Status", "JSON", "Math", "now", "module", "État"] {
        let source = format!(
            "def run(input: any) -> array<string>\nbefore={name};{name}={name}::Draft;[before.name,{name}.name]\nend\nenum {name}\nDraft\nend"
        );
        let output = Engine::new()
            .compile(&source)
            .unwrap()
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap();
        let values = output.value.as_array().unwrap();
        assert_eq!(values[0].as_bytes(), Some(name.as_bytes()));
        assert_eq!(values[1].as_bytes(), Some(b"Draft".as_slice()));
    }
    assert_eq!(
        evaluate("[Status.name,[1].map{|Status|Status},[1].map{Status::Draft.name}]"),
        serde_json::json!(["Status", [1], ["Draft"]])
    );
    // A local assigned on one path only, and `||=` on an enum, are refused
    // rather than falling back to the enum.
    for (body, code, at) in [
        (
            "def run -> string\nif false;Status=Status::Draft;end;Status.name\nend\n",
            "V0202",
            "Status.name",
        ),
        ("Status ||= 1\n", "V0104", "Status ||="),
    ] {
        let source = format!("{DECLARATIONS}{body}");
        let error = common::static_engine().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), [code], "{body}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{body}"
        );
    }
    for source in [
        "enum State\nA\na\nend",
        "enum State\n_\n__\nend",
        "enum State\nHTTPServer\nhttp_server\nend",
        "enum State\nİ\ni\nend",
        "enum İNT\nA\nend",
        "enum State?\nA\nend",
        "enum State\nself\nend",
        "enum State\nend",
        "enum State\nA\nend\ndef State\n1\nend",
        "if false;enum State;A;end;end",
        "enum;State;A;end",
        "1é",
        "0x1界",
        "1e2𝒜",
        "a\u{301}=1",
    ] {
        assert_eq!(
            Engine::new().compile(source).err().unwrap().kind,
            ErrorKind::Syntax,
            "{source}"
        );
    }
    assert_eq!(
        Engine::new()
            .compile("enum\nÉtat\nPrêt\nend\ncafé=7;數値=café;數値")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn host_imports_preserve_nominal_identity_without_retaining_the_script() {
    assert_eq!(size_of::<Value>(), 16);
    let source = format!(
        "{DECLARATIONS}def member -> Status\nStatus::Draft\nend\n\
         def compare(a: Status, b: any) -> array<bool>\n[a==b,b.is_type?(:Status) && a.enum==b.enum]\nend\n\
         def kind -> any\nStatus::Draft.enum\nend"
    );
    let script = Engine::new().compile(&source).unwrap();
    let first = script.call("member", &[], CallOptions::default()).unwrap();
    let second = script
        .clone()
        .call("member", &[], CallOptions::default())
        .unwrap();
    let foreign = Engine::new()
        .compile(&source)
        .unwrap()
        .call("member", &[], CallOptions::default())
        .unwrap();
    assert_eq!(
        first.value.as_enum_member(),
        Some(("Status", "Draft", "draft"))
    );
    assert!(first.stats.retained_memory_bytes > 0);
    for (other, expected) in [
        (second.value, b"[true,true]".as_slice()),
        (foreign.value, b"[false,false]"),
    ] {
        let result = script
            .call(
                "compare",
                &[first.value.clone(), other],
                CallOptions::default(),
            )
            .unwrap();
        let json = stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(json.value.as_bytes(), Some(expected));
    }
    // Another script can only hold a foreign enum as `any`, so the first
    // script reads its enum before the host passes it on.
    let kind = script
        .call("kind", &[], CallOptions::default())
        .unwrap()
        .value;
    drop(script);
    let mut engine = Engine::new();
    engine.register("state", move |ctx, _| ctx.import(&kind));
    let output = engine
        .compile("state()")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_enum_type(), Some("Status"));
    assert_eq!(output.value.as_enum_member(), None);
    assert!(output.stats.retained_memory_bytes > 0);
    assert_eq!(
        stringify_json(&output.value, CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Json
    );
}

#[test]
fn unused_declarations_are_lazy_and_repeated_member_storage_is_reclaimed() {
    let baseline = Engine::new()
        .compile("7")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let mut source = String::new();
    for i in 0..1000 {
        source.push_str(&format!("enum State{i}\nReady\nend\n"));
    }
    source.push('7');
    let output = Engine::new()
        .compile(&source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        output.stats.peak_memory_bytes,
        baseline.stats.peak_memory_bytes
    );
    assert_eq!(output.stats.retained_memory_bytes, 0);

    let source = format!(
        "{DECLARATIONS}i=0;state=Status::Draft;while i<2000;state=Status::Draft;text=state.to_s;i+=1;end;state"
    );
    let script = Engine::new().compile(&source).unwrap();
    for _ in 0..3 {
        let result = script
            .run(CallOptions {
                limits: Limits {
                    memory_bytes: Some(8192),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap();
        assert!(result.stats.peak_memory_bytes < 8192);
        assert!(result.stats.retained_memory_bytes < 2048);
        assert_eq!(
            result.value.as_enum_member(),
            Some(("Status", "Draft", "draft"))
        );
    }
}

#[test]
fn calls_of_enums_and_blocks_for_their_members_are_refused() {
    let mut engine = common::static_engine();
    engine.register("mark", |_, _| panic!("mark ran"));
    for (expression, code, at) in [
        ("Status(mark())", "V0201", 0),
        ("Status::Draft(mark())", "V0203", 8),
        ("Status::Draft.name(mark())", "V0301", 14),
        ("if false;Status=1;end;Status(mark())", "V0310", 22),
        ("Status.to_s{mark()}", "V0305", 11),
        ("Status::Draft.to_s{mark()}", "V0305", 18),
        ("Status.inspect{mark()}", "V0305", 14),
        ("Status::Draft.inspect{mark()}", "V0305", 21),
        ("Status.dup{mark()}", "V0305", 10),
        ("Status::Draft.dup{mark()}", "V0305", 17),
    ] {
        let error = engine
            .compile(&format!("{DECLARATIONS}{expression};mark()"))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), [code], "{expression}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            DECLARATIONS.len() + at,
            "{expression}"
        );
    }
}

#[test]
fn long_metadata_and_lookup_respect_limits_cancellation_and_latched_failures() {
    let name = format!("Member{}", "x".repeat(32768));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("mark", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    let script = engine
        .compile(&format!("enum State\n{name}\nend\nState::{name};mark()"))
        .unwrap();
    for kind in [
        ErrorKind::Memory,
        ErrorKind::Steps,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let mut options = CallOptions::default();
        match kind {
            ErrorKind::Memory => options.limits.memory_bytes = Some(16384),
            ErrorKind::Steps => options.limits.steps = Some(32),
            ErrorKind::Cancelled => options.cancellation.cancel(),
            ErrorKind::Deadline => options.deadline = Some(std::time::Instant::now()),
            _ => unreachable!(),
        }
        assert_eq!(script.run(options).unwrap_err().kind, kind);
    }
    assert_eq!(
        engine
            .compile(&format!("{DECLARATIONS}cancel();Status::Draft;mark()"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);

    let retained = Engine::new()
        .compile(&format!("enum State\n{name}\nend\nState::{name}"))
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    engine.register("ignore", move |ctx, _| {
        assert_eq!(ctx.import(&retained).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.bytes(b"x").unwrap_err().kind, ErrorKind::Memory);
        Ok(Value::nil())
    });
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(16384),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        engine
            .compile("ignore();mark()")
            .unwrap()
            .run(options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}
