use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value};

#[test]
fn captured_writes_preserve_host_inputs_and_independent_calls() {
    let script = Engine::new()
        .compile(
            "def zero()\nyield\nend\n\
             def run(input)\na=input;old=a\n\
             zero {zero {a[0]+=1;a.push(7)}}\n[a,old]\nend",
        )
        .unwrap();
    let input = Value::array(vec![Value::int(2)]);
    for _ in 0..3 {
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let values = result.value.as_array().unwrap();
        let updated = values[0].as_array().unwrap();
        assert_eq!(updated.len(), 2);
        assert_eq!(updated[0].as_int(), Some(3));
        assert_eq!(updated[1].as_int(), Some(7));
        assert_eq!(values[1].as_array().unwrap()[0].as_int(), Some(2));
    }
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(input.as_array().unwrap()[0].as_int(), Some(2));
}

#[test]
fn block_control_flow_releases_pending_arguments_and_receivers() {
    let input = Value::array((0..512).map(Value::int).collect());
    let definitions = "def zero()\nyield\nend\n\
        def sink(a,b)\nb\nend\n\
        def named(payload:,done:)\ndone\nend\n\
        def defaulted(payload:,done:zero {return 7})\ndone\nend\n";
    for body in [
        "a=[input];a[0].push(zero {return 7})",
        "sink(input,zero {return 7})",
        "named(payload:input,done:zero {return 7})",
        "defaulted(payload:input)",
        "zero {zero {return 7}}",
        "zero {break 7}",
        "zero {next 7}",
    ] {
        let source = format!(
            "{definitions}\ndef work(input)\n{body}\nend\n\
             def run(input)\nfor i in 1..200\nwork(input)\nend\n7\nend"
        );
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(96_000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0, "{body}");
        assert!(result.stats.peak_memory_bytes < 96_000, "{body}");
    }
}

#[test]
fn block_destructuring_charges_its_copy_before_entering_the_body() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let input = Value::array((0..512).map(Value::int).collect());
    let baseline = engine
        .compile("def run(input)\ninput.length\nend")
        .unwrap()
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    let script = engine
        .compile("def one(x)\nyield x\nend\ndef run(input)\none(input){|(*rest)|entered()}\nend")
        .unwrap();
    for (limits, kind) in [
        (
            Limits {
                memory_bytes: Some(baseline.stats.peak_memory_bytes + 4096),
                ..Limits::default()
            },
            ErrorKind::Memory,
        ),
        (
            Limits {
                steps: Some(baseline.stats.steps + 128),
                ..Limits::default()
            },
            ErrorKind::Steps,
        ),
    ] {
        let error = script
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits,
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let result = script
        .call("run", &[input], CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn block_errors_and_cancellation_prevent_later_host_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::nil())
    });
    for (source, kind) in [
        ("yield tick()", ErrorKind::Argument),
        ("block_given?(tick())", ErrorKind::Argument),
        ("missing {tick()}", ErrorKind::Name),
        (
            "def zero()\nyield\nend\nzero {cancel();tick()}",
            ErrorKind::Cancelled,
        ),
        (
            "def zero()\nyield\nend\nzero {return 7}",
            ErrorKind::Argument,
        ),
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            kind,
            "{source}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn block_recursion_and_syntax_depth_are_bounded() {
    let source = "def zero()\nyield\nend\ndef recurse()\nzero {recurse()}\nend";
    let script = Engine::new().compile(source).unwrap();
    let error = script
        .call(
            "recurse",
            &[],
            CallOptions {
                limits: Limits {
                    recursion: 16,
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        script
            .call(
                "recurse",
                &[],
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    for (open, close) in [("zero {", "}"), ("zero do\n", "\nend")] {
        let source = format!("{}1{}", open.repeat(300), close.repeat(300));
        let error = Engine::new().compile(&source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Syntax);
        assert!(error.message.contains("nesting too deep"));
    }
}

#[test]
fn nested_frame_storage_is_reserved_before_the_block_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let count = 512;
    let mut source = "def zero()\nyield\nend\ndef run()\n".to_owned();
    for index in 0..count {
        source.push_str(&format!("outer{index}=1\n"));
    }
    source.push_str("zero {\n");
    for index in 0..count {
        source.push_str(&format!("inner{index}=2\n"));
    }
    source.push_str("entered()\n}\nend\n");
    let script = engine.compile(&source).unwrap();
    let error = script
        .call(
            "run",
            &[],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(2 * count * size_of::<Value>()),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn breaking_a_receiving_loop_restores_local_call_lookup() {
    let script = Engine::new()
        .compile(
            "def id(x)\nx\nend\n\
             def receive()\nid=9\nwhile true\nid=yield id(3)\nend\nid(4)\nend\n\
             def run(input)\nreceive {break 7}\nend",
        )
        .unwrap();
    let error = script
        .call("run", &[Value::nil()], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert!(error.message.contains("non-callable"));
}
