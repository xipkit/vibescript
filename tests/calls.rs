use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

#[test]
fn host_and_script_keywords_preserve_input_isolation() {
    let mut engine = Engine::new();
    engine.register_with_keywords("echo", |ctx, args, keywords| {
        ctx.charge(keywords.len() as u64)?;
        let packet = keywords
            .iter()
            .find(|(key, _)| key.as_bytes() == Some(b"packet".as_slice()))
            .unwrap();
        ctx.array(&[args[0].clone(), packet.1.clone()])
    });
    let script = engine
        .compile("def run(n=7,packet:)\npacket.push(n)\necho(n,packet:)\nend")
        .unwrap();
    let packet = Value::array(vec![Value::int(1)]);
    for n in [3, 4] {
        let result = script
            .call_with_keywords(
                "run",
                &[],
                &[
                    ("packet".into(), packet.clone()),
                    ("n".into(), Value::int(n)),
                ],
                CallOptions::default(),
            )
            .unwrap();
        let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
        assert_eq!(
            encoded.value.as_bytes(),
            Some(format!("[{n},[1,{n}]]").as_bytes())
        );
    }
    assert_eq!(packet.as_array().unwrap().len(), 1);
    let script = engine.compile("def run(options)\noptions\nend").unwrap();
    assert_eq!(
        script
            .call_with_keywords(
                "run",
                &[],
                &[("different".into(), Value::int(1))],
                CallOptions::default()
            )
            .unwrap_err()
            .kind,
        ErrorKind::Argument
    );
}

#[test]
fn invalid_calls_do_not_evaluate_defaults_or_enter_host_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |ctx, _| {
        ctx.charge(1)?;
        Ok(Value::int(seen.fetch_add(1, Ordering::SeqCst) as i64 + 1))
    });
    for source in [
        "def f(a:tick(),needed:)\na\nend\nf()",
        "def f(a:tick())\na\nend\nf(1)",
        "def f(a:tick())\na\nend\nf(unknown:1)",
        "tick(unknown:1)",
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let script = engine
        .compile("def f(a=tick(),b:tick())\n[a,b]\nend\nf()")
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    let values = result.value.as_array().unwrap();
    assert_eq!(values[0].as_int(), Some(1));
    assert_eq!(values[1].as_int(), Some(2));
}

#[test]
fn callable_names_in_value_positions_do_not_execute_optional_defaults() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(9))
    });
    for params in ["a", "a=tick()", "a:tick()", "*a", "**a"] {
        let source = format!("def f({params})\ntick()\nend\nf");
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert!(error.message.contains("cannot be used as a value"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    assert_eq!(
        engine
            .compile("tick")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let result = engine
        .compile("def f()\ntick()\nend\nf")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(9));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn conditional_default_locals_resolve_hosts_before_evaluating_arguments() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register_with_keywords("probe", move |_, args, keywords| {
        assert_eq!(keywords.len(), 1);
        assert_eq!(keywords[0].0.as_bytes(), Some(b"flag".as_slice()));
        assert_eq!(args.len(), 1);
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(args[0].clone())
    });
    let script = engine
        .compile(
            "def f(a=(while true\nprobe=7\nbreak 0\nend),\n\
             b=probe(*[(while true\nprobe=42\nbreak 3\nend)],flag:true))\n\
             [b,probe]\nend\nf(1)",
        )
        .unwrap();
    let result = script.run(CallOptions::default()).unwrap();
    assert_eq!(result.value.as_array().unwrap()[0].as_int(), Some(3));
    assert_eq!(result.value.as_array().unwrap()[1].as_int(), Some(42));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for source in [
        "def f(a=(while true\nprobe=7\nbreak 0\nend),b=probe)\nb\nend\nf(1)",
        "def f(a=(while true\nprobe=nil\nbreak 0\nend),b=probe(flag:true))\nb\nend\nf()",
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn unbound_callees_and_receivers_fail_before_argument_evaluation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    for source in [
        "missing(tick())\nmissing=1",
        "missing.push(tick())\nmissing=[]",
        "missing[0]+=tick()\nmissing=[]",
        "def f(a=(while false\nmissing=[]\nend),b=missing.push(tick()))\nb\nend\nf()",
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Name,
            "{source}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    assert_eq!(
        engine
            .compile("missing[0]=tick()\nmissing=[]")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn cancellation_in_a_default_stops_later_binding() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("stop", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::int(1))
    });
    engine.register("tick", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(9))
    });
    let script = engine
        .compile("def f(a=stop(),b:tick())\nb\nend\nf()")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn defaults_use_vm_recursion_memory_and_cancellation_limits() {
    let recursive = Engine::new().compile("def f(a=f())\na\nend").unwrap();
    assert_eq!(
        recursive
            .call(
                "f",
                &[],
                CallOptions {
                    limits: Limits {
                        recursion: 16,
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    let expanding = Engine::new()
        .compile("def f(a:\"x\"*1000000)\na\nend")
        .unwrap();
    assert_eq!(
        expanding
            .call(
                "f",
                &[],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(12000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        recursive
            .call(
                "f",
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
}

#[test]
fn argument_expansion_and_rest_storage_are_accounted() {
    let array = Value::array((0..512).map(Value::int).collect());
    let hash = Value::hash(
        (0..512)
            .map(|i| (format!("k{i}").into_bytes(), Value::int(i)))
            .collect(),
    );
    for (input, source) in [
        (
            array,
            "def sink(*args)\nargs.length\nend\ndef run(input)\nsink(*input)\nend",
        ),
        (
            hash,
            "def sink(**args)\nargs.length\nend\ndef run(input)\nsink(**input)\nend",
        ),
    ] {
        let baseline = Engine::new()
            .compile("def run(input)\ninput.length\nend")
            .unwrap()
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        let script = Engine::new().compile(source).unwrap();
        for (limits, kind) in [
            (
                Limits {
                    memory_bytes: Some(baseline.stats.peak_memory_bytes + 1024),
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
            assert_eq!(
                script
                    .call(
                        "run",
                        std::slice::from_ref(&input),
                        CallOptions {
                            limits,
                            ..CallOptions::default()
                        }
                    )
                    .unwrap_err()
                    .kind,
                kind
            );
        }
        let result = script
            .call("run", &[input], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(512));
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
}

#[test]
fn pending_arguments_and_default_bindings_are_reclaimed_on_return() {
    for definitions in [
        "def sink(*args)\n7\nend\ndef f(input)\nsink(*input,(while true\nreturn 7\nend))\nend",
        "def sink(**args)\n7\nend\ndef f(input)\nsink(payload:input,other:(while true\nreturn 7\nend))\nend",
        "def early(value=(while true\nreturn 7\nend),payload:)\nvalue\nend\ndef f(input)\nearly(payload:input)\nend",
    ] {
        let source =
            format!("{definitions}\ndef run(input)\nfor i in 1..100\nf(input)\nend\n7\nend");
        let input = Value::array((0..400).map(Value::int).collect());
        let result = Engine::new()
            .compile(&source)
            .unwrap()
            .call(
                "run",
                &[input],
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(64000),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(result.stats.retained_memory_bytes, 0);
    }
}

#[test]
fn keyword_imports_and_returned_host_values_use_the_call_budget() {
    let mut engine = Engine::new();
    engine.register_with_keywords("foreign", |_, _, _| Ok(Value::bytes(vec![b'x'; 65536])));
    let script = engine.compile("def run(packet:)\npacket\nend").unwrap();
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(4096),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .call_with_keywords(
                "run",
                &[],
                &[("packet".into(), Value::bytes(vec![b'x'; 8192]))],
                options.clone()
            )
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    assert_eq!(
        engine
            .compile("foreign(marker:true)")
            .unwrap()
            .run(options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let input = Value::hash(vec![
        (vec![0xff], Value::int(1)),
        (b"a".to_vec(), Value::int(2)),
    ]);
    let script = engine
        .compile("def take(**kw)\nkw\nend\ndef run(input)\ntake(**input)\nend")
        .unwrap();
    let result = script
        .call("run", &[input], CallOptions::default())
        .unwrap();
    let entries = result.value.as_hash().unwrap();
    assert_eq!(entries[0].0.as_bytes(), Some(b"a".as_slice()));
    assert_eq!(entries[1].0.as_bytes(), Some([0xff].as_slice()));
}

#[test]
fn invalid_parameter_and_argument_order_is_rejected() {
    for source in [
        "def f(*a,*b)\nend",
        "def f(**a,**b)\nend",
        "def f(*a,b)\nend",
        "def f(a:,b)\nend",
        "def f(**a,b:)\nend",
        "def f(*a=[])\nend",
        "def f(**a={})\nend",
        "f(a:1,2)",
        "f(**{},*[1])",
    ] {
        assert_eq!(
            Engine::new().compile(source).err().unwrap().kind,
            ErrorKind::Syntax,
            "{source}"
        );
    }
    let source = format!(
        "def f(a={}1{})\na\nend",
        "f(".repeat(1100),
        ")".repeat(1100)
    );
    assert_eq!(
        Engine::new().compile(&source).err().unwrap().kind,
        ErrorKind::Syntax
    );
}
