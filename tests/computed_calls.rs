use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn computed_targets_support_nested_calls_keywords_splats_and_blocks() {
    for (source, expected) in [
        (
            "def f(x=42)\nx\nend\n(missing rescue f)((unknown rescue f)(8))",
            serde_json::json!(8),
        ),
        (
            "def f(*rest,b:)\n[rest,b]\nend\n(missing rescue f)(*[1,2],**{b:3})",
            serde_json::json!([[1, 2], 3]),
        ),
        (
            "def f(a,b:,**rest)\n[a,b,rest]\nend\n(missing rescue f)(2,b:3,c:4)",
            serde_json::json!([2,3,{"c":4}]),
        ),
        (
            "def f(x=2)\nyield(x)\nend\n(missing rescue f)(3) {|x| (unknown rescue f)(x+1) {|y| y+1}}",
            serde_json::json!(5),
        ),
        (
            "def f(x=2)\nyield(x)\nend\n(missing rescue f) {|x| break x+1}",
            serde_json::json!(3),
        ),
        (
            "def factory\nJSON::parse\nend\nfactory()(\"[8]\")",
            serde_json::json!([8]),
        ),
        (
            "[JSON::parse].map {(missing rescue _1)(\"[8]\")}",
            serde_json::json!([[8]]),
        ),
        (
            "class C\nCB=JSON::parse\ndef self.go\n(missing rescue CB)(\"[8]\")\nend\nend\nC.go()",
            serde_json::json!([8]),
        ),
        (
            "class C\ndef go\n(missing rescue hidden)(8)\nend\nprivate\ndef hidden(x)\nx+1\nend\nend\nC.new.go()",
            serde_json::json!(9),
        ),
        (
            "enum Status\nDraft\nend\ndef f\n42\nend\n[(Status::itself rescue f)(),(Status::to_s rescue f)(),(Status::nil? rescue f)(),(Status::Draft.itself rescue f)().name,(Status.itself rescue f)().name]",
            serde_json::json!([42, 42, 42, "Draft", "Status"]),
        ),
    ] {
        assert_eq!(result(source), expected, "{source}");
    }
}

#[test]
fn selection_rescue_finishes_before_arguments_and_callee_body() {
    for (argument, callee, expected) in [
        ("7", "x", vec![1, 2, 3]),
        ("raise \"argument\"", "x", vec![1, 2, 4]),
        ("7", "raise \"callee\"", vec![1, 2, 3, 4]),
    ] {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let mut engine = Engine::new();
        engine.register("record", move |_, args| {
            captured.lock().unwrap().push(args[0].as_int().unwrap());
            Ok(Value::nil())
        });
        let source = format!(
            "def select\nrecord(1);raise \"lookup\"\nend\ndef argument\nrecord(2);{argument}\nend\ndef callee(x)\nrecord(3);{callee}\nend\nbegin\n(select() rescue callee)(argument())\nrescue\nrecord(4)\nend"
        );
        engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(*events.lock().unwrap(), expected);
    }
}

#[test]
fn ordinary_expressions_still_reject_function_values() {
    for expression in [
        "[f][0]()",
        "(true ? f : f)()",
        "(false || f)()",
        "(begin\nf\nend)()",
        "{cb:f}.cb()",
    ] {
        let source = format!("def f(x)\nx\nend\n{expression}");
        let error = Engine::new()
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert!(
            error.message.contains("cannot be used as a value"),
            "{expression}: {error}"
        );
    }
}

#[test]
fn selected_receivers_and_builtins_survive_argument_side_effects() {
    let source = "class C\ngetter value\ndef initialize(x)\n@value=x\nend\ndef add(x)\n@value+x\nend\nend\nc=C.new(4);x=(c.add rescue missing)(begin\nc=C.new(7);8\nend);[x,c.value]";
    assert_eq!(result(source), serde_json::json!([12, 7]));
    let source =
        "cb=JSON::parse;x=(cb rescue missing)(begin\ncb=JSON::stringify;\"[8]\"\nend);[x,cb([9])]";
    assert_eq!(result(source), serde_json::json!([[8], "[9]"]));
    assert_eq!(
        result("(\"kept\".itself rescue missing)()"),
        serde_json::json!("kept")
    );
    assert_eq!(
        result("(Time.at(0).getutc.iso8601 rescue missing)()"),
        serde_json::json!("1970-01-01T00:00:00Z")
    );
}

#[test]
fn failed_computed_calls_release_argument_and_receiver_storage() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let captured = observed.clone();
    let mut engine = Engine::new();
    engine.register("observe", move |ctx, _| {
        captured
            .lock()
            .unwrap()
            .push(ctx.stats().retained_memory_bytes);
        Ok(Value::int(0))
    });
    let source = "def factory\n\"x\"*8192\nend\ndef fail\nobserve();raise \"argument\"\nend\ndef run(n)\ni=0;while i<n\nbegin\n(factory().itself rescue missing)(fail())\nrescue\n0\nend;i+=1\nend;42\nend";
    let script = engine.compile(source).unwrap();
    let first = script
        .call("run", &[Value::int(1)], CallOptions::default())
        .unwrap();
    let repeated = script
        .call("run", &[Value::int(32)], CallOptions::default())
        .unwrap();
    assert_eq!(repeated.value.as_int(), Some(42));
    assert_eq!(repeated.stats.retained_memory_bytes, 0);
    assert!(
        repeated.stats.peak_memory_bytes <= first.stats.peak_memory_bytes + 1024,
        "first={:?}, repeated={:?}, observed={:?}",
        first.stats,
        repeated.stats,
        observed.lock().unwrap()
    );
    let samples = observed.lock().unwrap();
    assert_eq!(samples.len(), 33);
    assert!(*samples.iter().min().unwrap() >= 8192);
    assert!(samples.iter().max().unwrap() - samples.iter().min().unwrap() <= 1024);
    drop(samples);
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(repeated.stats.peak_memory_bytes);
    script
        .call("run", &[Value::int(32)], options.clone())
        .unwrap();
    options.limits.memory_bytes = Some(repeated.stats.peak_memory_bytes - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(32)], options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn exhaustion_and_cancellation_in_selection_or_arguments_cannot_be_rescued() {
    for cancel in [false, true] {
        for in_target in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let captured = calls.clone();
            let token = CancellationToken::new();
            let cancellation = token.clone();
            let mut engine = Engine::new();
            engine.register("stop", move |ctx, _| {
                if cancel {
                    cancellation.cancel();
                    ctx.checkpoint()?;
                } else {
                    ctx.charge(u64::MAX)?;
                }
                Ok(Value::nil())
            });
            engine.register("fallback", move |_, _| {
                captured.fetch_add(1, Ordering::SeqCst);
                Ok(Value::nil())
            });
            let expression = if in_target {
                "(stop() rescue fallback)()"
            } else {
                "(fallback rescue missing)(stop())"
            };
            let source =
                format!("begin\n{expression}\nrescue\nfallback()\nensure\nfallback()\nend");
            let options = CallOptions {
                cancellation: token,
                ..CallOptions::default()
            };
            let error = engine.compile(&source).unwrap().run(options).unwrap_err();
            assert_eq!(
                error.kind,
                if cancel {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::Steps
                }
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn protected_match_data_and_errors_reject_wrapped_mutators() {
    for expression in ["m.clear", "m.dup.clear"] {
        let source =
            format!("def fallback\n42\nend\nm=\"a\".match(\"a\");({expression} rescue fallback)()");
        assert_eq!(result(&source), serde_json::json!(42), "{expression}");
    }
    let source = "m=\"a\".match(\"a\");begin\n(m.dup.clear rescue nil)()\nrescue\nnil\nend;[m.dup.to_s,m.captures]";
    assert_eq!(result(source), serde_json::json!(["a", []]));
    let source = "def fallback\n42\nend\nbegin\nraise \"x\"\nrescue=>e\n(e.dup.clear rescue fallback)()\nend";
    assert_eq!(result(source), serde_json::json!(42));
}

#[test]
fn selected_host_capabilities_keep_keyword_contracts_and_step_limits() {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let mut engine = Engine::new();
    engine.register_with_keywords("sms", move |ctx, args, keywords| {
        captured.fetch_add(1, Ordering::SeqCst);
        assert_eq!(args[0].as_bytes(), Some(b"destination".as_slice()));
        assert_eq!(keywords.len(), 1);
        assert_eq!(keywords[0].0.as_bytes(), Some(b"body".as_slice()));
        ctx.array(&[args[0].clone(), keywords[0].1.clone()])
    });
    let script = engine
        .compile("(missing rescue sms)(\"destination\", body:\"hello\")")
        .unwrap();
    let first = script.run(CallOptions::default()).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut options = CallOptions::default();
    options.limits.steps = Some(first.stats.steps);
    let exact = script.run(options.clone()).unwrap();
    assert_eq!(exact.stats.steps, first.stats.steps);
    options.limits.steps = Some(first.stats.steps - 1);
    assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Steps);
    assert_eq!(
        first.value.as_array().unwrap()[1].as_bytes(),
        Some(b"hello".as_slice())
    );
}

#[test]
fn computed_call_nesting_reaches_the_parser_guard() {
    let source = format!("JSON::parse{}", "()".repeat(300));
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    let source = format!("{}f{}()", "(missing rescue ".repeat(300), ")".repeat(300));
    let error = Engine::new().compile(&source).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    let error = Engine::new().compile("nil&.f()(1).field=2").err().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
}
