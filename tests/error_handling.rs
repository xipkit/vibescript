mod common;

use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorClass, ErrorKind, Limits, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    result_with(Engine::new(), source)
}

fn result_with(engine: Engine, source: &str) -> serde_json::Value {
    let script = engine.compile(source).unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    let output = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(output.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn rescue_values_classes_bindings_and_expression_forms() {
    for (source, expected) in [
        ("begin\n1//0\nrescue\n42\nend", serde_json::json!(42)),
        ("1//0 rescue 42", serde_json::json!(42)),
        ("begin\n42\nend", serde_json::json!(42)),
        (
            "a=begin\nraise \"bad\"\nrescue RuntimeError => e\n[e.class,e.class,e.message,e.message,e.dup.message]\nend\na",
            serde_json::json!(["RuntimeError", "RuntimeError", "bad", "bad", "bad"]),
        ),
        (
            "begin\nraise TypeError, \"wrong\"\nrescue ZeroDivisionError\n1\nrescue TypeError | ArgumentError => e\ne.class\nend",
            serde_json::json!("TypeError"),
        ),
        ("begin\n7\nrescue\nx=3\nelse\n9\nend", serde_json::json!(9)),
        (
            "def parse -> string\nraise \"x\"\nrescue => e\ne.message\nend\nparse",
            serde_json::json!("x"),
        ),
        (
            "begin\nrandom_id(1025)\nrescue LimitError => e\ne.class\nend",
            serde_json::json!("LimitError"),
        ),
        (
            "begin\nassert(false,\"bad\")\nrescue AssertionError => e\ne.message\nend",
            serde_json::json!("bad"),
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
    // A rescue binding shadows a local only while its handler runs. The
    // checker types the name as the outer local, so this keeps the ADR-004
    // language until the checker scopes the binding.
    let output = common::gradual_engine()
        .compile("e=7; begin\nraise \"x\"\nrescue => e\nx=e.message\nend;[e,x]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(output.value.to_string(), "[7, x]");
    // A local assigned only in a handler that did not run cannot be read.
    let source = "begin\n7\nrescue\nx=3\nend;x";
    let error = vibescript::Engine::new().compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0201"]);
    assert_eq!(error.diagnostics()[0].span.start, source.len() - 1);
}

#[test]
fn ensure_runs_once_for_normal_errors_returns_breaks_and_nexts() {
    for (source, expected, events) in [
        (
            "begin\n7\nensure\nrecord(1)\nend",
            serde_json::json!(7),
            vec![1],
        ),
        (
            "begin\nraise \"x\"\nrescue\nrecord(1);7\nelse\nrecord(2)\nensure\nrecord(3)\nend",
            serde_json::json!(7),
            vec![1, 3],
        ),
        (
            "def f -> int\nbegin\nreturn 7\nensure\nrecord(1)\nend\nend\nf",
            serde_json::json!(7),
            vec![1],
        ),
        (
            "def f -> int\nbegin\nreturn 7\nensure\nreturn 8\nend\nend\nf",
            serde_json::json!(8),
            vec![],
        ),
        (
            "i=0;while i<3\ni+=1\nbegin\nnext\nensure\nrecord(i)\nend\nend;i",
            serde_json::json!(3),
            vec![1, 2, 3],
        ),
        (
            "while true\nbegin\nbreak 7\nensure\nrecord(1)\nend\nend",
            serde_json::json!(7),
            vec![1],
        ),
        (
            "[1,2].map { |i|\nbegin\nnext i*2\nensure\nrecord(i)\nend\n}",
            serde_json::json!([2, 4]),
            vec![1, 2],
        ),
    ] {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let capture = recorded.clone();
        let mut engine = Engine::new();
        engine.register("record", move |_, args| {
            capture.lock().unwrap().push(args[0].as_int().unwrap());
            Ok(Value::nil())
        });
        let output = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        let output = stringify_json(&output.value, CallOptions::default()).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(output.value.as_bytes().unwrap()).unwrap(),
            expected,
            "{source}"
        );
        assert_eq!(*recorded.lock().unwrap(), events, "{source}");
    }
}

#[test]
fn retry_restarts_the_body_without_running_ensure_between_attempts() {
    assert_eq!(
        result(
            "n=0;e=0;begin\nn+=1;raise \"retry\" if n<3\nn\nrescue\nretry\nensure\ne+=1\nend;[n,e]"
        ),
        serde_json::json!([3, 1])
    );
    assert_eq!(
        result(
            "begin\nbegin\nraise TypeError,\"original\"\nrescue\nraise\nend\nrescue => e\n[e.class,e.message]\nend"
        ),
        serde_json::json!(["TypeError", "original"])
    );
}

#[test]
fn real_exhaustion_and_cancellation_cannot_be_rescued_or_run_cleanup() {
    for (source, kind) in [
        (
            "begin\nwhile true\n1\nend\nrescue RuntimeError\nrecord(1)\nensure\nrecord(2)\nend",
            ErrorKind::Steps,
        ),
        (
            "begin\nraise \"again\"\nrescue\nretry\nensure\nrecord(2)\nend",
            ErrorKind::Steps,
        ),
        (
            "begin\ncancel()\nrescue RuntimeError\nrecord(1)\nensure\nrecord(2)\nend",
            ErrorKind::Cancelled,
        ),
    ] {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let mut engine = Engine::new();
        engine.register("record", move |_, args| {
            observed.lock().unwrap().push(args[0].as_int().unwrap());
            Ok(Value::nil())
        });
        engine.register("cancel", |ctx, _| {
            ctx.cancellation().cancel();
            Ok(Value::nil())
        });
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions {
                limits: Limits {
                    steps: Some(1000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(error.kind, kind, "{source}: {error}");
        assert!(calls.lock().unwrap().is_empty());
    }
}

#[test]
fn nested_rescued_values_remain_protected_and_errors_preserve_their_class() {
    for write in ["e.backtrace.push(\"x\")", "e.dup.backtrace[0]=\"x\""] {
        let source = format!("begin\nraise \"bad\"\nrescue => e\n{write}\nend");
        let error = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.class(), Some(ErrorClass::Runtime));
        assert!(error.message.contains("rescued error"), "{write}: {error}");
    }
    // Writing a field or clearing an error is refused before it runs.
    for (write, at) in [("e.message=7", "message"), ("e.dup.clear", "clear")] {
        let source = format!("begin\nraise \"bad\"\nrescue => e\n{write}\nend");
        let error = vibescript::Engine::new().compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0203"], "{write}");
        assert_eq!(error.diagnostics()[0].span.start, source.find(at).unwrap());
    }
}

#[test]
fn raised_messages_preserve_arbitrary_bytes_in_rescue_and_host_errors() {
    for (prefix, class) in [
        ("raise input", ErrorClass::Runtime),
        ("raise TypeError,input", ErrorClass::Type),
        ("assert(false,input)", ErrorClass::Assertion),
    ] {
        let source = format!("def run(input: string)\n{prefix}\nend");
        let script = Engine::new().compile(&source).unwrap();
        let input = Value::bytes([0, 0xff, 0xc3, b'x']);
        let error = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap_err();
        assert_eq!(error.class(), Some(class));
        assert_eq!(error.message_bytes(), input.as_bytes().unwrap());
        assert_eq!(error.message, "\0\u{fffd}\u{fffd}x");
        let source = format!(
            "def run(input: string) -> string?\nbegin\n{prefix}\nrescue RuntimeError => e\ne.dup.message\nend\nend"
        );
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_bytes(), input.as_bytes());
    }
}

#[test]
fn rescue_modifiers_bind_to_commands_and_nullable_filters_accept_errors() {
    for source in [
        "def f(a: int) -> int\nraise \"x\"\nend\nf 1 rescue 7",
        "def f(a: int) -> int\na+1\nend\nf begin\n6\nend",
        "begin\nraise TypeError,\"x\"\nrescue TypeError?\n7\nend",
    ] {
        assert_eq!(result(source), serde_json::json!(7), "{source}");
    }
}

#[test]
fn retry_crossing_a_block_runs_its_cleanup_before_becoming_a_local_jump() {
    let source = "events: array<int> =[];begin\nbegin\nraise \"original\"\nrescue\n[1].each {\nbegin\nretry\nrescue LocalJumpError\nevents=events+[1]\nensure\nevents=events+[2]\nend\n}\nend\nrescue LocalJumpError\nevents=events+[3]\nend;events";
    assert_eq!(result(source), serde_json::json!([2, 3]));
    let source = "[1].map {\nn=0;begin\nn+=1;raise \"retry\" if n<3;n\nrescue\nretry\nend\n}";
    assert_eq!(result(source), serde_json::json!([3]));
}

#[test]
fn deeply_nested_handlers_and_modifiers_reach_the_parser_guard() {
    for (prefix, suffix) in [
        ("begin\n", "\nrescue\n1\nend"),
        ("begin\n", "\nensure\n1\nend"),
        ("1/0 rescue (", ")"),
    ] {
        let source = format!("{}1{}", prefix.repeat(1100), suffix.repeat(1100));
        let error = Engine::new().compile(&source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax);
    }
}

#[test]
fn repeated_rescue_releases_saved_errors_bindings_and_failed_call_frames() {
    let source = "def explode\nraise \"x\"*8192\nend\ndef run(n: int) -> int\ni=0;while i<n\nbegin\nexplode\nrescue=>e\nobserve(e)\nend;i+=1\nend;42\nend";
    let samples = Arc::new(Mutex::new(Vec::new()));
    let capture = samples.clone();
    let mut engine = Engine::new();
    engine.register("observe", move |ctx, args| {
        assert_eq!(
            args[0]
                .as_hash()
                .unwrap()
                .iter()
                .find(|(key, _)| key.as_bytes() == Some(b"message"))
                .unwrap()
                .1
                .as_bytes()
                .unwrap()
                .len(),
            8192
        );
        capture
            .lock()
            .unwrap()
            .push(ctx.stats().retained_memory_bytes);
        Ok(Value::nil())
    });
    let script = engine.compile(source).unwrap();
    let first = script
        .call("run", &[Value::int(1)], CallOptions::default())
        .unwrap();
    let repeated = script
        .call("run", &[Value::int(32)], CallOptions::default())
        .unwrap();
    assert_eq!(repeated.value.as_int(), Some(42));
    assert_eq!(repeated.stats.retained_memory_bytes, 0);
    assert!(repeated.stats.peak_memory_bytes <= first.stats.peak_memory_bytes + 1024);
    let samples = samples.lock().unwrap();
    assert_eq!(samples.len(), 33);
    assert!(samples.iter().max().unwrap() - samples.iter().min().unwrap() <= 1024);
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(repeated.stats.peak_memory_bytes);
    drop(samples);
    script.call("run", &[Value::int(32)], options).unwrap();
}

#[test]
fn foreign_control_errors_run_cleanup_without_entering_rescue() {
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let calls = Arc::new(Mutex::new(0));
        let recorded = calls.clone();
        let mut engine = Engine::new();
        engine.register("foreign", move |_, _| {
            Err(vibescript::Error::new(kind, "foreign"))
        });
        engine.register("record", move |_, _| {
            *recorded.lock().unwrap() += 1;
            Ok(Value::nil())
        });
        let error = engine
            .compile(
                "begin\nforeign()\nrescue RuntimeError\nrecord();record()\nensure\nrecord()\nend",
            )
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}

#[test]
fn pending_returns_stay_accounted_during_ensure_and_exhaustion_wins() {
    let mut engine = Engine::new();
    engine.register("observe", |ctx, _| {
        assert!(ctx.stats().retained_memory_bytes >= 32768);
        Ok(Value::nil())
    });
    let script = engine
        .compile("def f -> string\nbegin\nreturn \"x\"*32768\nensure\nobserve()\nend\nend\nf")
        .unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(output.value.as_bytes().unwrap().len(), 32768);
    assert!(output.stats.retained_memory_bytes >= 32768);
    engine.register("exhaust", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Err(vibescript::Error::new(ErrorKind::Host, "replacement"))
    });
    let error = engine
        .compile("begin\nraise \"original\"\nensure\nexhaust()\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    assert_ne!(error.message, "replacement");
    assert_eq!(error.diagnostic.unwrap().position.line, 4);
}

#[test]
fn rescued_error_protection_and_rendering_survive_host_transfer() {
    let original = Engine::new()
        .compile("begin\nraise \"kept\"\nrescue=>e\ne\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut engine = Engine::new();
    engine.register("retrieve", move |_, _| Ok(original.clone()));
    let source = "e=retrieve().as(error);before=\"#{e.dup}\";begin\ne.dup.backtrace.push(\"bad\")\nrescue=>failure\n[before,failure.message]\nend";
    let output = engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(encoded.value.as_bytes().unwrap()).unwrap(),
        serde_json::json!(["kept", "assignment cannot modify a rescued error"])
    );
}

#[test]
fn invalid_loop_transfers_become_rescuable_only_after_callee_cleanup() {
    for jump in ["break", "next", "break 9", "next 9"] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let mut engine = common::runtime_engine();
        engine.register("record", move |_, args| {
            recorded.lock().unwrap().push(args[0].as_int().unwrap());
            Ok(Value::nil())
        });
        let source = format!(
            "def f\nbegin\n{jump}\nrescue RuntimeError\nrecord(1)\nensure\nrecord(2)\nend\nend\nbegin\n[1].each {{f}}\nrescue LocalJumpError\nrecord(3)\nend"
        );
        engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(*events.lock().unwrap(), vec![2, 3], "{jump}");
    }
}

#[test]
fn invalid_loop_transfers_reject_before_evaluating_values() {
    for jump in ["break", "next"] {
        let source = format!(
            "events: array<int> =[];begin\n{jump} events.push(1)\nrescue RuntimeError\nevents.push(2)\nend;events"
        );
        assert_eq!(
            result_with(common::runtime_engine(), &source),
            serde_json::json!([2]),
            "{jump}"
        );
    }
}

#[test]
fn invalid_block_returns_run_cleanup_before_the_caller_rescues() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = events.clone();
    let mut engine = Engine::new();
    engine.register("record", move |_, args| {
        recorded.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    let source = "begin\n[1].each {\nbegin\nreturn 9\nrescue RuntimeError\nrecord(1)\nensure\nrecord(2)\nend\n}\nrescue LocalJumpError\nrecord(3)\nend";
    engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(*events.lock().unwrap(), vec![2, 3]);
}

#[test]
fn rescued_error_fields_iterate_in_sorted_order() {
    let fields = [
        "backtrace",
        "class",
        "code_frame",
        "message",
        "to_s",
        "type",
    ];
    // A host sees a rescued error's fields in sorted order.
    for source in [
        "begin\nraise \"boom\"\nrescue => e\ne\nend",
        "begin\n1//0\nrescue ZeroDivisionError => e\ne\nend",
        "begin\n[].fetch(1)\nrescue => e\ne.dup\nend",
    ] {
        let output = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        let keys: Vec<&[u8]> = output
            .value
            .as_hash()
            .unwrap()
            .iter()
            .map(|(key, _)| key.as_bytes().unwrap())
            .collect();
        let fields: Vec<&[u8]> = fields.iter().map(|field| field.as_bytes()).collect();
        assert_eq!(keys, fields, "{source}");
    }
    // A script reads an error through its members, not as a hash.
    for (source, at) in [
        ("begin\nraise \"boom\"\nrescue => e\ne.keys\nend", "keys"),
        (
            "begin\n1//0\nrescue ZeroDivisionError => e\ne.map { |k, v| k }\nend",
            "map",
        ),
    ] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0203"], "{source}");
        assert_eq!(error.diagnostics()[0].span.start, source.find(at).unwrap());
    }
}
