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
    let recursive = engine
        .compile("def f(n: int) -> any\n f(n+1)\nend\nf(0)")
        .unwrap();
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
    let script = engine
        .compile("def run(x: any) -> any\n host()\nend")
        .unwrap();
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
        .compile("def identity(s: string) -> string\n s\nend\ndef first(s: string) -> string?\n s[0]\nend")
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
#[cfg_attr(target_os = "wasi", ignore = "WASI has no threads")]
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
    let script=Engine::new().compile("def temporary(s: string) -> string\n s.upcase(:ascii)\nend\ndef run(s: string) -> int\n i=0\n while i<200\n  temporary(s)\n  i+=1\n end\n 7\nend").unwrap();
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
        .compile(
            "def run(s: string) -> string?\n JSON.parse(s).as(hash<string, string>)[\"tiny\"]\nend",
        )
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
    let script = Engine::new()
        .compile("def run(x: any) -> any\n x\nend")
        .unwrap();
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
        .compile("x: array<any> = []\ni = 0\nwhile i < 10001\n x = [x]\n i += 1\nend")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Recursion
    );
    let raw = format!("{}0{}", "[".repeat(10_001), "]".repeat(10_001));
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
        .compile("a: array<int> =[]\ni=0\nwhile i<2000\n a.push(i)\n i+=1\nend\na.sum")
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
        .compile("x: array<any> =[]\ni=0\nwhile i<80\n x=[x]\n i+=1\nend\na: array<any> =[x]\na[0]=0\ni=0\nwhile i<100\n a=[a]\n i+=1\nend\na.length")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1));
    let mut value = Value::int(0);
    for _ in 0..9_999 {
        value = Value::array(vec![value]);
    }
    let script = engine
        .compile("def once(a: array<any>) -> array<any>; a << a; end; def twice(a: array<any>) -> array<any>; a << a; a << a; end")
        .unwrap();
    let accepted = script
        .call("once", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(accepted.value.as_array().unwrap().len(), 2);
    let error = script
        .call("twice", &[value], CallOptions::default())
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

#[test]
fn memory_limits_do_not_change_step_accounting() {
    let mut engine = Engine::new();
    engine.register("echo", |_, values| Ok(values[0].clone()));
    for source in [
        "def run(x: any) -> any; echo(x); end",
        "def run(x: any) -> any; x; end",
        "class C; @n: int; def initialize(@n: int); end; def n -> int; @n; end; end; def run(x: any) -> any; echo(C.new(3)); end",
        "module M; N = 3; end; def run(x: any) -> any; echo(M); end",
        "def run(x: any) -> any; echo(/abc/i); end",
        "def run(x: any) -> any; echo(Time.parse('2026-01-01T00:00:00Z')); end",
    ] {
        let script = engine.compile(source).unwrap();
        let argument = Value::object(vec![(
            b"items".to_vec(),
            Value::array(vec![Value::bytes("payload"), Value::int(42)]),
        )]);
        let run = |memory_bytes| {
            script
                .call(
                    "run",
                    std::slice::from_ref(&argument),
                    CallOptions {
                        limits: Limits {
                            memory_bytes,
                            ..Limits::default()
                        },
                        ..CallOptions::default()
                    },
                )
                .unwrap()
        };
        let limited = run(Some(16 << 20));
        let unlimited = run(None);
        assert_eq!(
            limited.value.to_string(),
            unlimited.value.to_string(),
            "{source}"
        );
        assert_eq!(limited.stats.steps, unlimited.stats.steps, "{source}");
    }
}

/// Nested `begin`s around assignments of distinct locals, each narrowed
/// before them: every level's rescue and ensure forget what the levels
/// inside assign, so checking takes work proportional to the depth times
/// the locals, which the compile budget must be able to stop.
fn nested_begins(levels: usize, locals: usize) -> String {
    let mut source: String = (0..locals).map(|i| format!("x{i}: int? = 1\n")).collect();
    source.push_str(&"begin\n".repeat(levels));
    source.extend((0..locals).map(|i| format!("x{i} = nil\n")));
    source.push_str(&"rescue\nc = 1\nensure\nc = 2\nend\n".repeat(levels));
    source
}

#[test]
fn type_checking_stops_at_the_compile_budget() {
    // WASI checks syntax at most 128 levels tall.
    let (levels, locals) = if cfg!(target_os = "wasi") {
        (100, 8_000)
    } else {
        (400, 2_000)
    };
    let source = nested_begins(levels, locals);
    let engine = Engine::new();
    let started = Instant::now();
    let checked = engine.type_check(&source).unwrap();
    let full = started.elapsed();
    assert!(checked.steps > 4 * Limits::default().steps.unwrap());
    // The default step quota stops the check, well before it would finish.
    let started = Instant::now();
    let error = engine
        .compile_with_options(&source, &CallOptions::default())
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Steps, "{error}");
    assert!(
        started.elapsed() < full / 2,
        "{:?} with a quota, {full:?} without",
        started.elapsed()
    );
    // So do a deadline and cancellation, without a step quota.
    let unlimited = Limits {
        steps: None,
        ..Limits::default()
    };
    let started = Instant::now();
    let options = CallOptions {
        limits: unlimited.clone(),
        deadline: Some(started + full / 10),
        ..CallOptions::default()
    };
    let error = engine
        .compile_with_options(&source, &options)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Deadline, "{error}");
    assert!(started.elapsed() < full / 2);
    // WASI has no threads to cancel from.
    if cfg!(target_os = "wasi") {
        return;
    }
    let cancellation = CancellationToken::new();
    let options = CallOptions {
        limits: unlimited,
        cancellation: cancellation.clone(),
        ..CallOptions::default()
    };
    let started = Instant::now();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(full / 10);
        cancellation.cancel();
    });
    let error = engine
        .compile_with_options(&source, &options)
        .err()
        .unwrap();
    canceller.join().unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled, "{error}");
    assert!(started.elapsed() < full / 2);
}

/// Two unions of shapes with optional fields, which a value of one may fit
/// in any alternative of the other, so relating them compares every pair.
fn loose_unions(arms: usize) -> String {
    let union = |prefix: &str, extra: &str| {
        (0..arms)
            .map(|i| format!("{{{prefix}{i}?: int{extra}}}"))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    format!(
        "type A = {}\ntype B = {}\ndef f(x: A, y: B?) -> B?\n  z: A? = x\n  y\nend\ndef g(x: A) -> B\n  x\nend\n",
        union("a", ""),
        union("b", ", x: int")
    )
}

#[test]
fn type_operations_stop_at_the_compile_budget() {
    let source = loose_unions(1_000);
    let engine = Engine::new();
    let started = Instant::now();
    let checked = engine.type_check(&source).unwrap();
    let full = started.elapsed();
    assert!(checked.steps > 2 * Limits::default().steps.unwrap());
    // The step quota stops the comparison inside one assignment.
    let started = Instant::now();
    let error = engine
        .compile_with_options(&source, &CallOptions::default())
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Steps, "{error}");
    assert!(
        started.elapsed() < full,
        "{:?} with a quota, {full:?} without",
        started.elapsed()
    );
    let started = Instant::now();
    let options = CallOptions {
        limits: Limits {
            steps: None,
            ..Limits::default()
        },
        deadline: Some(started + full / 10),
        ..CallOptions::default()
    };
    let error = engine
        .compile_with_options(&source, &options)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Deadline, "{error}");
    assert!(started.elapsed() < full);
}
