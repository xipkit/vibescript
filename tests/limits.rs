use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, parse_json, stringify_json,
};

#[test]
fn step_memory_recursion_and_deadline_limits() {
    let engine = Engine::new();
    let forever = engine.compile("while true\n 1\nend").unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: Some(100),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(forever.run(options).unwrap_err().kind, ErrorKind::Steps);
    let options = CallOptions {
        deadline: Some(Instant::now() - Duration::from_secs(1)),
        ..CallOptions::default()
    };
    assert_eq!(forever.run(options).unwrap_err().kind, ErrorKind::Deadline);
    let recursive = engine.compile("def f(n)\n f(n+1)\nend\nf(0)").unwrap();
    let options = CallOptions {
        limits: Limits {
            recursion: 32,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        recursive.run(options).unwrap_err().kind,
        ErrorKind::Recursion
    );
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(8192),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        engine
            .compile("\"x\" * 1000000")
            .unwrap()
            .run(options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn cancelled_before_import_never_invokes_host() {
    let entered = Arc::new(AtomicBool::new(false));
    let flag = entered.clone();
    let mut engine = Engine::new();
    engine.register("host", move |_, _| {
        flag.store(true, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine.compile("def run(x)\n host()\nend").unwrap();
    let token = CancellationToken::new();
    token.cancel();
    let options = CallOptions {
        cancellation: token,
        limits: Limits {
            memory_bytes: Some(1),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call("run", &[Value::bytes(vec![b'a'; 100_000])], options)
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert!(!entered.load(Ordering::SeqCst));
}

#[test]
fn shared_inputs_charge_spare_capacity_and_release_it_for_small_results() {
    let mut bytes = Vec::with_capacity(65536);
    bytes.extend_from_slice(b"hello");
    let capacity = bytes.capacity();
    let input = Value::bytes(bytes);
    let script = Engine::new()
        .compile("def identity(s)\n s\nend\ndef first(s)\n s[0]\nend")
        .unwrap();
    let result = script
        .call(
            "identity",
            std::slice::from_ref(&input),
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), input.as_bytes());
    assert_eq!(
        result.value.as_bytes().unwrap().as_ptr(),
        input.as_bytes().unwrap().as_ptr()
    );
    assert!(result.stats.retained_memory_bytes >= capacity);
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(capacity - 1),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call("identity", std::slice::from_ref(&input), options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let first = script
        .call("first", &[input], CallOptions::default())
        .unwrap();
    assert_eq!(first.value.as_bytes(), Some(b"h".as_slice()));
    assert!(first.stats.retained_memory_bytes < 1024);
}

#[test]
fn running_script_observes_cancellation() {
    let token = CancellationToken::new();
    let signal = token.clone();
    let (started, ready) = std::sync::mpsc::channel();
    let mut engine = Engine::new();
    engine.register("started", move |_, _| {
        started.send(()).unwrap();
        Ok(Value::nil())
    });
    let script = engine.compile("started()\nwhile true\n 1\nend").unwrap();
    let handle = std::thread::spawn(move || {
        script.run(CallOptions {
            cancellation: token,
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        })
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    signal.cancel();
    assert_eq!(
        handle.join().unwrap().unwrap_err().kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn temporary_buffers_and_returned_frames_are_reclaimed() {
    let script=Engine::new().compile("def temporary(s)\n s.upcase(:ascii)\nend\ndef run(s)\n i=0\n while i<200\n  temporary(s)\n  i+=1\n end\n 7\nend").unwrap();
    let result = script
        .call(
            "run",
            &[Value::bytes(vec![b'a'; 16384])],
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(80_000),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.value.as_int(), Some(7));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    assert!(result.stats.peak_memory_bytes < 80_000);
}

#[test]
fn tiny_json_result_does_not_retain_large_source_or_siblings() {
    let raw = format!("{{\"large\":\"{}\",\"tiny\":\"x\"}}", "a".repeat(200_000));
    let script = Engine::new()
        .compile("def run(s)\n JSON.parse(s)[\"tiny\"]\nend")
        .unwrap();
    let result = script
        .call(
            "run",
            &[Value::bytes(raw.into_bytes())],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(b"x".as_slice()));
    assert!(result.stats.retained_memory_bytes < 1024);
    assert!(result.stats.peak_memory_bytes > 200_000);
}

#[test]
fn unescaped_json_strings_fit_without_repeated_buffer_growth() {
    let text = "a".repeat(65536);
    let input = format!("\"{text}\"");
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(70000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let parsed = parse_json(input.as_bytes(), options).unwrap();
    assert_eq!(parsed.value.as_bytes(), Some(text.as_bytes()));
    assert!(parsed.stats.peak_memory_bytes < 70000);
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(140000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let encoded = stringify_json(&Value::bytes(text), options.clone()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(input.as_bytes()));
    assert!(encoded.stats.peak_memory_bytes < 140000);
    let object = Value::hash(vec![(b"payload".to_vec(), Value::bytes(vec![b'a'; 65536]))]);
    let encoded = stringify_json(&object, options).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
    assert_eq!(parsed["payload"].as_str().unwrap().len(), 65536);
    assert!(encoded.stats.peak_memory_bytes < 140000);
}

#[test]
fn imported_host_arrays_and_deep_constructed_values_are_bounded() {
    let script = Engine::new().compile("def run(x)\n x\nend").unwrap();
    let input = Value::array((0..1000).map(Value::int).collect());
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(2048),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script.call("run", &[input], options).unwrap_err().kind,
        ErrorKind::Memory
    );
    let script = Engine::new()
        .compile("x = []\ni = 0\nwhile i < 200\n x = [x]\n i += 1\nend")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Recursion
    );
    let raw = format!("{}0{}", "[".repeat(200), "]".repeat(200));
    assert_eq!(
        parse_json(raw.as_bytes(), CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
}

#[test]
fn unaliased_array_growth_has_bounded_work_and_memory() {
    let script = Engine::new()
        .compile("a=[]\ni=0\nwhile i<2000\n a.push(i)\n i+=1\nend\na.sum")
        .unwrap();
    let result = script
        .run(CallOptions {
            limits: Limits {
                steps: Some(150_000),
                memory_bytes: Some(256 << 10),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1_999_000));
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn array_mutation_keeps_depth_limits_accurate() {
    let engine = Engine::new();
    let result = engine
        .compile("x=[]\ni=0\nwhile i<80\n x=[x]\n i+=1\nend\na=[x]\na[0]=0\ni=0\nwhile i<100\n a=[a]\n i+=1\nend\na.length")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    let error = engine
        .compile("a=[]\ni=0\nwhile i<200\n a << a\n i+=1\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
}

#[test]
fn children_do_not_cancel_their_parent() {
    let parent = CancellationToken::new();
    let child = parent.child_token();
    child.cancel();
    assert!(!parent.is_cancelled());
    let other = parent.child_token();
    parent.cancel();
    assert!(other.is_cancelled());
}

#[test]
fn ignored_host_memory_failure_is_not_recoverable() {
    let entered = Arc::new(AtomicBool::new(false));
    let flag = entered.clone();
    let mut engine = Engine::new();
    engine.register("host", move |ctx, _| {
        flag.store(true, Ordering::SeqCst);
        assert_eq!(ctx.bytes(&[0; 8192]).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.bytes(b"x").unwrap_err().kind, ErrorKind::Memory);
        Ok(Value::int(1))
    });
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(4096),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        engine
            .compile("host()")
            .unwrap()
            .run(options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    assert!(entered.load(Ordering::SeqCst));
}
