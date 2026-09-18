use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, Capability, CheckReport, CheckedOutcome, Engine, ErrorKind, HostMethod, Limits,
    Position, Signature, Value,
};

fn check(source: &str) -> CheckReport {
    Engine::new()
        .compile(source)
        .unwrap()
        .check_call("run", &[], &CallOptions::default())
        .unwrap()
}

fn executed(outcome: CheckedOutcome) -> vibescript::Outcome {
    match outcome {
        CheckedOutcome::Executed(outcome) => outcome,
        other => panic!("{other:?}"),
    }
}

#[test]
fn public_calls_bind_keywords_defaults_rest_and_concrete_paths() {
    let script = Engine::new()
        .compile("def unused()->int;\"bad\";end;def run(a:int,b:2,**rest);[a,b,rest[:x]];end")
        .unwrap();
    let args = [Value::int(7)];
    let keywords = [
        ("b".into(), Value::int(3)),
        ("b".into(), Value::int(5)),
        ("x".into(), Value::int(9)),
    ];
    let options = CallOptions::default();
    let report = script
        .check_call_with_keywords("run", &args, &keywords, &options)
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(report.stats.steps > 0);
    assert_eq!(report.stats.retained_memory_bytes, 0);
    let outcome = executed(
        script
            .checked_call_with_keywords("run", &args, &keywords, options)
            .unwrap(),
    );
    assert_eq!(outcome.value.to_string(), "[7, 5, 9]");
    let script = Engine::new()
        .compile("def run(flag)->int;if flag;\"bad\";else;7;end;end")
        .unwrap();
    assert!(
        script
            .check_call("run", &[Value::boolean(false)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
    assert!(
        !script
            .check_call("run", &[Value::boolean(true)], &CallOptions::default())
            .unwrap()
            .is_clean()
    );
}

#[test]
fn reports_source_positions_and_known_parameter_and_return_types() {
    let source = "def run(x:int) -> int\n  x\nend";
    let script = Engine::new().compile(source).unwrap();
    let report = script
        .check_call(
            "run",
            &[Value::bytes(b"bad".to_vec())],
            &CallOptions::default(),
        )
        .unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert_eq!(report.diagnostics.len(), 1, "{report:?}");
    let diagnostic = &report.diagnostics[0];
    assert!(
        diagnostic.message.contains("expected int, got string"),
        "{diagnostic}"
    );
    assert_eq!(diagnostic.function, "run");
    assert_eq!(diagnostic.position.line, 1);
    assert!(diagnostic.code_frame.contains("def run"));
    assert!(diagnostic.filename.is_none());
    assert!(diagnostic.to_string().contains(&diagnostic.code_frame));
    let report = check("def run() -> int\n  \"é\"\nend");
    let diagnostic = &report.diagnostics[0];
    assert_eq!(diagnostic.message, "Return value: expected int, got string");
    assert_eq!(diagnostic.position, Position { line: 2, column: 3 });
    assert!(diagnostic.code_frame.contains('é'));
    assert!(report.stats.retained_memory_bytes > 0);
    assert!(report.stats.peak_memory_bytes >= report.stats.retained_memory_bytes);
}

#[test]
fn rejection_precedes_callbacks_defaults_and_initializer_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    for source in [
        "def run(x=effect())->int;effect();\"bad\";end",
        "class C;effect();end;def run;effect();end",
    ] {
        let script = engine.compile(source).unwrap();
        let CheckedOutcome::Rejected(report) = script
            .checked_call("run", &[], CallOptions::default())
            .unwrap()
        else {
            panic!("rejected script ran")
        };
        assert!(!report.is_clean());
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
    let script = engine.compile("def run;effect();end").unwrap();
    let options = CallOptions {
        capabilities: vec![Capability::new("sms", move |_| {
            panic!("checker invoked factory")
        })],
        ..CallOptions::default()
    };
    let CheckedOutcome::Rejected(report) = script.checked_call("run", &[], options).unwrap() else {
        panic!("factory script ran")
    };
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.incomplete.len(), 1);
    assert!(report.incomplete[0].message.contains("sms"));
    assert!(report.incomplete[0].message.contains("factory"));
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[test]
fn dynamic_unknowns_are_clean_and_runtime_failures_remain_errors() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("read", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::bytes(b"bad".to_vec()))
    });
    let script = engine.compile("def run()->int;read();end").unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .checked_call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    assert_eq!(effects.load(Ordering::Relaxed), 1);
}

#[test]
fn checked_calls_preserve_value_isolation_and_ignored_block_transfers() {
    let method = HostMethod::new_with_block("visit", |host, _, _| {
        for _ in 0..3 {
            let _ = host.call_block(&[]);
        }
        Ok(Value::int(99))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: true,
    })
    .unwrap();
    let driver = Value::object(vec![(b"visit".to_vec(), method.value())]);
    let input = Value::array(vec![Value::int(1)]);
    for (transfer, expected) in [("break 7", "[7, [1, 2]]"), ("return 9", "9")] {
        let script = Engine::new()
            .compile(&format!(
                "def run(driver,a);n=driver.visit{{a.push(2);{transfer}}};[n,a];end"
            ))
            .unwrap();
        for _ in 0..2 {
            let outcome = executed(
                script
                    .checked_call(
                        "run",
                        &[driver.clone(), input.clone()],
                        CallOptions::default(),
                    )
                    .unwrap(),
            );
            assert_eq!(outcome.value.to_string(), expected);
            assert_eq!(input.as_array().unwrap().len(), 1);
        }
    }
}

#[test]
fn report_retention_does_not_keep_code_callbacks_or_arguments_alive() {
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        let _ = &marker;
        Ok(Value::nil())
    });
    let script = engine.compile("def run;1-\"bad\";end").unwrap();
    let report = script
        .check_call("run", &[], &CallOptions::default())
        .unwrap();
    assert!(!report.is_clean());
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
    drop(script);
    drop(engine);
    assert!(weak.upgrade().is_none());
    assert!(report.diagnostics[0].message.contains("int and string"));
    let marker = Arc::new(());
    let weak = Arc::downgrade(&marker);
    let method = HostMethod::new("send", move |_, _, _| {
        let _ = &marker;
        panic!("argument callback ran")
    });
    let input = Value::object(vec![(b"send".to_vec(), method.value())]);
    let script = Engine::new().compile("def run(x:int);x;end").unwrap();
    let report = script
        .check_call("run", std::slice::from_ref(&input), &CallOptions::default())
        .unwrap();
    assert!(!report.is_clean());
    drop(input);
    drop(method);
    drop(script);
    assert!(weak.upgrade().is_none());
    assert!(report.diagnostics[0].message.contains("attached method"));
}

#[test]
fn diagnostics_are_sorted_deduplicated_and_stable_across_contexts() {
    let source = "def bad(x);begin;x-\"bad\";rescue;0;end;end\ndef run\n bad(1)\n bad(2)\nend";
    let mut previous = None;
    for _ in 0..8 {
        let report = check(source);
        assert!(report.incomplete.is_empty(), "{report:?}");
        let messages: Vec<_> = report
            .diagnostics
            .iter()
            .map(|d| (d.offset, d.function.clone(), d.message.clone()))
            .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        if let Some(previous) = &previous {
            assert_eq!(&messages, previous);
        }
        previous = Some(messages);
    }
    let source = "def second()->int;\"bad\";end\ndef first()->int;\"bad\";end\ndef run(flag);if flag;first();else;second();end;end";
    let mut engine = Engine::new();
    engine.register("unknown", |_, _| panic!("checker executed"));
    let script = engine
        .compile(&format!("{source}\ndef root;run(unknown());end"))
        .unwrap();
    let report = script
        .check_call("root", &[], &CallOptions::default())
        .unwrap();
    assert_eq!(report.diagnostics.len(), 2, "{report:?}");
    assert_eq!(report.diagnostics[0].function, "second");
    assert_eq!(report.diagnostics[1].function, "first");
    assert!(
        report
            .diagnostics
            .windows(2)
            .all(|pair| pair[0].offset <= pair[1].offset)
    );
}

#[test]
fn guards_cancellation_and_quotas_stop_before_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("def run;effect();end").unwrap();
    for kind in [
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
        ErrorKind::Steps,
        ErrorKind::Memory,
    ] {
        let mut options = CallOptions::default();
        match kind {
            ErrorKind::Cancelled => options.cancellation.cancel(),
            ErrorKind::Deadline => options.deadline = Some(std::time::Instant::now()),
            ErrorKind::Steps => options.limits.steps = Some(0),
            ErrorKind::Memory => options.limits.memory_bytes = Some(0),
            _ => unreachable!(),
        }
        assert_eq!(
            script.check_call("run", &[], &options).unwrap_err().kind,
            kind
        );
        assert_eq!(
            script.checked_call("run", &[], options).unwrap_err().kind,
            kind
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
    assert_eq!(
        script
            .checked_call("missing", &[], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
}

#[test]
fn public_reports_obey_exact_and_sampled_work_and_memory_limits() {
    let script = Engine::new()
        .compile("def run(flag);if flag;[1]+\"bad\";else;{}+2;end;end")
        .unwrap();
    let input = [Value::boolean(true)];
    let baseline = script
        .check_call("run", &input, &CallOptions::default())
        .unwrap();
    assert!(!baseline.is_clean());
    let stats = baseline.stats;
    drop(baseline);
    for (memory, steps, expected) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script
                .check_call("run", &input, &options)
                .err()
                .map(|e| e.kind),
            expected
        );
    }
    for sample in 0..16 {
        for memory in [false, true] {
            let mut options = CallOptions::default();
            let kind = if memory {
                options.limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 16);
                ErrorKind::Memory
            } else {
                options.limits.steps = Some(stats.steps * sample as u64 / 16);
                ErrorKind::Steps
            };
            assert_eq!(
                script.check_call("run", &input, &options).unwrap_err().kind,
                kind
            );
        }
    }
}

#[test]
fn lazy_globals_and_strict_validation_keep_their_entry_order() {
    let script = Engine::new().compile("def run;7;end").unwrap();
    let options = CallOptions {
        globals: [("unused".into(), Value::bytes(vec![b'x'; 128 * 1024]))].into(),
        limits: Limits {
            memory_bytes: Some(48 * 1024),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert!(script.check_call("run", &[], &options).unwrap().is_clean());
    assert_eq!(
        executed(script.checked_call("run", &[], options).unwrap())
            .value
            .as_int(),
        Some(7)
    );
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("def run(x);x;end").unwrap();
    let options = CallOptions {
        globals: [(
            "unused".into(),
            HostMethod::new("send", |_, _, _| panic!("called")).value(),
        )]
        .into(),
        ..CallOptions::default()
    };
    assert_eq!(
        script
            .check_call("missing", &[], &options)
            .unwrap_err()
            .kind,
        ErrorKind::Name
    );
    assert_eq!(
        script.check_call("run", &[], &options).unwrap_err().kind,
        ErrorKind::Runtime
    );
}

#[test]
fn descriptors_remain_attached_and_old_grants_are_rejected() {
    let method = HostMethod::new("send", |_, _, _| panic!("invalid grant called"));
    let producer = Engine::new().compile("def run(x);x;end").unwrap();
    let fresh = Value::object(vec![(b"send".to_vec(), method.value())]);
    let old = producer
        .call("run", &[fresh], CallOptions::default())
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("def run(sms);sms.send();end")
        .unwrap();
    let CheckedOutcome::Rejected(report) = script
        .checked_call("run", &[old], CallOptions::default())
        .unwrap()
    else {
        panic!("old grant approved")
    };
    assert!(report.diagnostics[0].message.contains("earlier invocation"));
    let CheckedOutcome::Rejected(report) = producer
        .checked_call("run", &[method.value()], CallOptions::default())
        .unwrap()
    else {
        panic!("detached method approved")
    };
    assert!(report.diagnostics[0].message.contains("Attached methods"));
}

#[test]
fn diagnostic_text_preserves_raw_keys_and_distinct_enum_declarations() {
    let script = Engine::new().compile("def run(x:int);x;end").unwrap();
    let input = Value::object(vec![(vec![b'\n', 0xff, 0], Value::int(1))]);
    let report = script
        .check_call("run", &[input], &CallOptions::default())
        .unwrap();
    let text = &report.diagnostics[0].message;
    assert!(
        text.contains("\\n") && text.contains("\\xff") && text.contains("\\0"),
        "{text}"
    );
    assert!(!text.contains('\n') && !text.contains('\0'));
    let other = Engine::new()
        .compile("enum State;Ready;end;State::Ready")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("enum State;Ready;end;def run(x:State);x;end")
        .unwrap();
    let report = script
        .check_call("run", &[other], &CallOptions::default())
        .unwrap();
    assert!(
        report.diagnostics[0]
            .message
            .contains("different declaration"),
        "{report:?}"
    );
}

#[test]
fn call_failures_and_unresolved_contracts_have_actionable_messages() {
    for (source, needle) in [
        ("def run(x);x;end", "missing argument"),
        ("def run;missing();end", "undefined callable"),
        ("def run;to_int();end", "wrong number"),
        ("def run;/(/;end", "regular expression"),
        ("def run;yield;end", "No block"),
        ("def run(x:Missing=7);x;end", "Unknown type"),
        ("def run;\"abc\".center(5,extra:7);end", "center"),
        ("def run;x=1;x=[];x;end", "Reassignment"),
    ] {
        let report = check(source);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains(needle)),
            "{source}: {report:?}"
        );
    }
    for source in ["def run;(-1).chr;end", "def run;1.foo;end"] {
        let report = check(source);
        assert!(!report.is_clean());
        assert!(report.diagnostics.is_empty());
        assert!(!report.incomplete.is_empty());
    }
}
