mod common;

use std::sync::{Arc, Mutex};
use vibescript::{
    CallOptions, CancellationToken, Capability, Engine, Error, ErrorKind, HostMethod, Limits, Value,
};

type Trace = Arc<Mutex<Vec<String>>>;

fn options(trace: &Trace) -> CallOptions {
    let trace = trace.clone();
    CallOptions {
        capabilities: vec![Capability::new("host", move |_| {
            let observed = trace.clone();
            let once = HostMethod::new_with_block("host.once", move |call, args, _| {
                observed.lock().unwrap().push("start".into());
                let result = call.call_block(args);
                observed
                    .lock()
                    .unwrap()
                    .push(if result.is_ok() { "done" } else { "error" }.into());
                result
            });
            let each = HostMethod::new_with_block("host.each", |call, args, _| {
                let mut output = Vec::new();
                for item in args[0].as_array().unwrap() {
                    output.push(call.call_block(std::slice::from_ref(item))?);
                }
                call.context().array(&output)
            });
            let optional = HostMethod::new_with_block("host.optional", |call, _, _| {
                Ok(Value::boolean(call.block_given()))
            });
            let ignore = HostMethod::new_with_block("host.ignore", |call, args, _| {
                if call.call_block(args).is_err() {
                    assert!(call.call_block(args).is_err());
                }
                Ok(Value::int(99))
            });
            let recover = HostMethod::new_with_block("host.recover", |call, args, _| {
                match call.call_block(args) {
                    Ok(value) => Ok(value),
                    Err(error) if error.kind != ErrorKind::ControlFlow => {
                        Ok(Value::bytes("host recovered"))
                    }
                    Err(error) => Err(error),
                }
            });
            let observed = trace.clone();
            let note = HostMethod::new("host.note", move |_, args, _| {
                observed.lock().unwrap().push(args[0].to_string());
                Ok(Value::nil())
            });
            Ok(Value::object(vec![
                (b"once".to_vec(), once.value()),
                (b"each".to_vec(), each.value()),
                (b"optional".to_vec(), optional.value()),
                (b"ignore".to_vec(), ignore.value()),
                (b"recover".to_vec(), recover.value()),
                (b"note".to_vec(), note.value()),
            ]))
        })],
        ..CallOptions::default()
    }
}

fn run(body: &str, options: CallOptions) -> vibescript::Result<vibescript::Outcome> {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine
        .compile(&format!("def run\n{body}\nend"))
        .unwrap()
        .call("run", &[], options)
}

#[test]
fn host_blocks_support_dispatch_binding_captures_and_repeated_calls() {
    for (body, expected) in [
        ("host.once(3) { |n| n+1 }", "4"),
        ("host::once(3) { |n| n+1 }", "4"),
        ("host[:once](3) { |n| n+1 }", "4"),
        ("(host[:once])(3) { |n| n+1 }", "4"),
        ("host.send(:once, 3) { |n| n+1 }", "4"),
        ("host.public_send(:once, 3) { |n| n+1 }", "4"),
        ("copy=host.dup; copy.once(3) { |n| n+1 }", "4"),
        ("host.once(1,2) { |a,b,c| [a,b,c] }", "[1, 2, nil]"),
        ("host.once([2,3]) { |a,b| a+b }", "5"),
        ("host.once([[2,3],4]) { |(a,b),c| a+b+c }", "9"),
        ("host.once(3) { _1+1 }", "4"),
        ("host.once(3) { |n: int| n+1 }", "4"),
        (
            "begin; host.once(\"bad\") { |n: int| n }; rescue RuntimeError; 7; end",
            "7",
        ),
        ("host.once(3) { next 4 }", "4"),
        ("host.once(3) { break 4 }", "4"),
        ("host.once(3) { return 4 }; 99", "4"),
        ("host.once { block_given? }", "false"),
        ("host.optional()", "false"),
        ("host.optional { raise \"unused\" }", "true"),
        (
            "begin; host.once(); rescue => e; [e.class.to_s,e.message]; end",
            "[RuntimeError, block required]",
        ),
        ("host.each([1,2,3]) { |n| n*2 }", "[2, 4, 6]"),
        ("host.each([1,2,3]) { |n| break 7 if n==2; n }", "7"),
        (
            "host.each([1,2]) { |i| host.each([3,4]) { |j| i+j } }",
            "[[4, 5], [5, 6]]",
        ),
        ("a=[]; host.each([1,2,3]) { |n| a.push(n) }; a", "[1, 2, 3]"),
        (
            "a=[]; for i in [1,2]; a.push(host.once(i) { break 7 }); end; a",
            "[7, 7]",
        ),
    ] {
        let trace = Trace::default();
        let result = run(body, options(&trace)).unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.to_string(), expected, "{body}");
    }
}

#[test]
fn host_boundaries_preserve_ensure_and_rescue_order() {
    for (body, expected, events) in [
        (
            "begin; host.once { begin; return 7; ensure; host.note(\"block ensure\"); end }; ensure; host.note(\"outer ensure\"); end",
            "7",
            vec!["start", "block ensure", "error", "outer ensure"],
        ),
        (
            "begin; host.once { begin; raise \"bad\"; ensure; host.note(\"block ensure\"); end }; rescue; host.note(\"outer rescue\"); 7; ensure; host.note(\"outer ensure\"); end",
            "7",
            vec![
                "start",
                "block ensure",
                "error",
                "outer rescue",
                "outer ensure",
            ],
        ),
        (
            "begin; host.recover { raise \"bad\" }; rescue; \"script recovered\"; end",
            "host recovered",
            vec![],
        ),
        (
            "host.once { begin; raise \"bad\"; rescue; 7; end }",
            "7",
            vec!["start", "done"],
        ),
    ] {
        let trace = Trace::default();
        let result = run(body, options(&trace)).unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.to_string(), expected, "{body}");
        assert_eq!(*trace.lock().unwrap(), events, "{body}");
    }
}

#[test]
fn ignored_host_control_errors_cannot_replace_or_repeat_a_transfer() {
    for body in [
        "host.ignore { host.note(\"body\"); return 7 }; 99",
        "host.ignore { host.note(\"body\"); break 7 }",
    ] {
        let trace = Trace::default();
        let result = run(body, options(&trace)).unwrap();
        assert_eq!(result.value.as_int(), Some(7));
        assert_eq!(*trace.lock().unwrap(), ["body"]);
    }
}

#[test]
fn block_presence_and_absorbed_breaks_obey_host_contracts() {
    let events = Trace::default();
    let args_events = events.clone();
    let result_events = events.clone();
    let callback_events = events.clone();
    let method = HostMethod::new_with_block("checked", move |call, args, _| {
        callback_events.lock().unwrap().push("callback".into());
        call.call_block(args)
    })
    .with_block_contract(
        move |_, _, _, block| {
            args_events.lock().unwrap().push("arguments".into());
            if !block {
                return Err(Error::new(ErrorKind::Argument, "block required"));
            }
            Ok(())
        },
        move |_, value| {
            result_events.lock().unwrap().push("result".into());
            if value.as_int().is_none() {
                return Err(Error::new(ErrorKind::Type, "integer result required"));
            }
            Ok(())
        },
    );
    let opts = CallOptions {
        capabilities: vec![Capability::new("checked", move |_| Ok(method.value()))],
        ..CallOptions::default()
    };
    for (body, expected, events_want) in [
        ("checked { 7 }", 7, vec!["arguments", "callback", "result"]),
        (
            "checked { break 7 }",
            7,
            vec!["arguments", "callback", "result"],
        ),
        (
            "begin; checked { break \"wrong\" }; rescue; 8; end",
            8,
            vec!["arguments", "callback", "result"],
        ),
        ("checked { return 9 }; 99", 9, vec!["arguments", "callback"]),
        ("begin; checked(); rescue; 10; end", 10, vec!["arguments"]),
    ] {
        events.lock().unwrap().clear();
        assert_eq!(
            run(body, opts.clone()).unwrap().value.as_int(),
            Some(expected),
            "{body}"
        );
        assert_eq!(*events.lock().unwrap(), events_want, "{body}");
    }
    let script = Engine::new()
        .compile("def run() -> int; checked { return \"wrong\" }; end")
        .unwrap();
    assert_eq!(
        script.call("run", &[], opts).unwrap_err().kind,
        ErrorKind::Type
    );
}

#[test]
fn host_block_arguments_and_retained_results_keep_value_semantics() {
    let source = Value::array(vec![Value::int(1)]);
    let supplied = source.clone();
    let retained = Arc::new(Mutex::new(Vec::new()));
    let observed = retained.clone();
    let method = HostMethod::new_with_block("visit", move |call, _, _| {
        let first = call.call_block(std::slice::from_ref(&supplied))?;
        observed.lock().unwrap().push(first);
        call.call_block(std::slice::from_ref(&supplied))
    });
    let opts = CallOptions {
        capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
        ..CallOptions::default()
    };
    let output = run(
        "a=[]; visit { |input| input.push(2); a.push(input); a }; a[0].push(3); a",
        opts,
    )
    .unwrap();
    assert_eq!(source.to_string(), "[1]");
    assert_eq!(retained.lock().unwrap()[0].to_string(), "[[1, 2]]");
    assert_eq!(output.value.to_string(), "[[1, 2, 3], [1, 2]]");
}

#[test]
fn ignored_quota_errors_keep_their_original_block_diagnostics() {
    let method = HostMethod::new_with_block("visit", |call, _, _| {
        let mut error = call.call_block(&[]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        let diagnostic = error.diagnostic.clone().unwrap();
        error.message = "tampered".into();
        error.diagnostic = None;
        let repeated = call.call_block(&[]).unwrap_err();
        assert_eq!(repeated.kind, ErrorKind::Steps);
        assert_eq!(repeated.diagnostic.as_ref(), Some(&diagnostic));
        Ok(Value::int(99))
    });
    let opts = CallOptions {
        capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
        limits: Limits {
            steps: Some(2_000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let error = run("visit {\n while true\n  1\n end\n}\n99", opts).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
    assert_eq!(error.message, "step quota exceeded (2000)");
    let diagnostic = error.diagnostic.unwrap();
    assert!(
        (3..=4).contains(&diagnostic.position.line),
        "{diagnostic:?}"
    );
    assert_eq!(diagnostic.frames[0].position, diagnostic.position);
}

#[test]
fn arguments_retained_by_the_host_remain_charged_during_block_execution() {
    let method = HostMethod::new_with_block("visit", |call, args, _| {
        assert_eq!(args[0].as_bytes().unwrap().len(), 524_288);
        call.call_block(&[])
    });
    let opts = CallOptions {
        globals: [("payload".into(), Value::bytes(vec![b'a'; 524_288]))].into(),
        capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
        limits: Limits {
            memory_bytes: Some(900_000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(
        run("visit(payload) { \"b\"*524288 }", opts)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn retained_block_errors_keep_and_release_their_diagnostic_reservations() {
    let samples = Arc::new(Mutex::new(Vec::new()));
    let observed = samples.clone();
    let method = HostMethod::new_with_block("visit", move |call, _, _| {
        let before = call.context().stats().retained_memory_bytes;
        let mut errors = Vec::new();
        for _ in 0..32 {
            errors.push(call.call_block(&[]).unwrap_err());
        }
        let retained = call.context().stats().retained_memory_bytes;
        assert!(retained > before + 32 * 4096, "{before} -> {retained}");
        drop(errors);
        let released = call.context().stats().retained_memory_bytes;
        assert!(retained > released + 32 * 4096, "{retained} -> {released}");
        observed
            .lock()
            .unwrap()
            .extend([before, retained, released]);
        Ok(Value::nil())
    });
    let opts = CallOptions {
        capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
        ..CallOptions::default()
    };
    run("visit { raise(\"x\"*4096) }", opts).unwrap();
    assert_eq!(samples.lock().unwrap().len(), 3);
}

#[test]
fn recursive_host_blocks_reach_the_configured_limit_on_the_default_stack() {
    let script = Engine::new()
        .compile("def recurse; host.once { recurse() }; end; def run; recurse(); end")
        .unwrap();
    let error = script
        .call("run", &[], options(&Trace::default()))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
}

#[test]
fn nested_yield_and_local_recovery_preserve_their_control_boundaries() {
    for (driver, expected) in [
        ("host.once { yield }; 99", 99),
        ("for i in [1,2]; yield; end; 99", 99),
        ("[1].each { yield }; 99", 99),
        ("yield; 99", 7),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def relay; {driver}; end; def run; relay {{ break 7 }}; end"
            ))
            .unwrap();
        assert_eq!(
            script
                .call("run", &[], options(&Trace::default()))
                .unwrap()
                .value
                .as_int(),
            Some(expected),
            "{driver}"
        );
    }
    for (body, expected) in [
        (
            "host.once { begin; return 7; ensure; return 8; end }; 99",
            8,
        ),
        (
            "host.once { n=0; begin; n+=1; raise \"again\" if n<3; n; rescue; retry; end }",
            3,
        ),
        (
            "host.once { begin; raise \"bad\"; ensure; break 7; end }",
            7,
        ),
    ] {
        assert_eq!(
            run(body, options(&Trace::default()))
                .unwrap()
                .value
                .as_int(),
            Some(expected)
        );
    }
    let script = Engine::new().compile(
        "def relay; host.once { [block_given?,yield] }; end; def run; relay { return 7 }; 99; end"
    ).unwrap();
    assert_eq!(
        script
            .call("run", &[], options(&Trace::default()))
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn ordinary_block_errors_allow_later_calls_without_replaying_handlers() {
    let method = HostMethod::new_with_block("visit", |call, _, _| {
        let error = call.call_block(&[Value::int(1)]).unwrap_err();
        assert_eq!(error.message, "first");
        let next = call.call_block(&[Value::int(2)])?;
        assert_eq!(next.as_int(), Some(2));
        Err(error)
    });
    let mut opts = options(&Trace::default());
    opts.capabilities
        .push(Capability::new("visit", move |_| Ok(method.value())));
    let result = run(
        "a=[]; begin; visit { |n| begin; a.push(n); raise \"first\" if n==1; n; ensure; a.push(n+10); end }; rescue => e; a.push(e.message); end; a",
        opts,
    ).unwrap();
    assert_eq!(result.value.to_string(), "[1, 11, 2, 12, first]");
}

#[test]
fn host_block_dispatch_preserves_exact_step_and_memory_thresholds() {
    for body in [
        "host.once(4) { |n| host.each([1,2,3]) { |m| n+m } }",
        "begin; host.once { raise \"bad\" }; rescue => e; e.message; end",
        "host.once { break [1,2,3] }",
    ] {
        let opts = options(&Trace::default());
        let baseline = run(body, opts.clone()).unwrap();
        let mut exact = opts.clone();
        exact.limits.steps = Some(baseline.stats.steps);
        exact.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes);
        assert_eq!(
            run(body, exact).unwrap().value.to_string(),
            baseline.value.to_string()
        );
        let mut short = opts.clone();
        short.limits.steps = Some(baseline.stats.steps - 1);
        assert_eq!(run(body, short).unwrap_err().kind, ErrorKind::Steps);
        let mut short = opts;
        short.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
        assert_eq!(run(body, short).unwrap_err().kind, ErrorKind::Memory);
    }
}

#[test]
fn retained_block_values_remain_charged_and_discarded_values_release_storage() {
    let method = HostMethod::new_with_block("visit", |call, _, _| {
        let mut held = Vec::new();
        for _ in 0..16 {
            held.push(call.call_block(&[])?);
        }
        let retained = call.context().stats().retained_memory_bytes;
        drop(held);
        let released = call.context().stats().retained_memory_bytes;
        assert!(retained >= released + 16 * 8192, "{retained} -> {released}");
        for _ in 0..32 {
            drop(call.call_block(&[])?);
        }
        assert_eq!(call.context().stats().retained_memory_bytes, released);
        Ok(Value::nil())
    });
    let result = run(
        "visit { \"x\"*8192 }",
        CallOptions {
            capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
            ..CallOptions::default()
        },
    )
    .unwrap();
    assert_eq!(result.stats.retained_memory_bytes, 0);
}

#[test]
fn ignored_cancellation_prevents_reentry_rescue_ensure_and_later_effects() {
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let trace = Trace::default();
    let seen = trace.clone();
    let method = HostMethod::new_with_block("visit", move |call, _, _| {
        let error = call.call_block(&[]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(error.class(), None);
        seen.lock().unwrap().push("cancelled".into());
        assert_eq!(call.call_block(&[]).unwrap_err().kind, ErrorKind::Cancelled);
        Ok(Value::int(99))
    });
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancel.cancel();
        Ok(Value::nil())
    });
    let seen = trace.clone();
    engine.register("effect", move |_, _| {
        seen.lock().unwrap().push("effect".into());
        Ok(Value::nil())
    });
    let script = engine.compile("def run; begin; visit { begin; stop(); effect(); rescue; effect(); ensure; effect(); end }; effect(); rescue; effect(); ensure; effect(); end; end").unwrap();
    let error = script
        .call(
            "run",
            &[],
            CallOptions {
                cancellation,
                capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(*trace.lock().unwrap(), ["cancelled"]);
}

#[test]
fn precancelled_or_expired_calls_never_invoke_host_blocks() {
    let method = HostMethod::new_with_block("visit", |_, _, _| panic!("expired host callback ran"));
    let script = Engine::new().compile("visit { 1 }").unwrap();
    for deadline in [false, true] {
        let mut opts = CallOptions {
            capabilities: vec![Capability::new("visit", {
                let method = method.clone();
                move |_| Ok(method.value())
            })],
            ..CallOptions::default()
        };
        if deadline {
            opts.deadline = Some(std::time::Instant::now());
        } else {
            opts.cancellation.cancel();
        }
        assert_eq!(
            script.run(opts).unwrap_err().kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
    }
}

#[test]
fn foreign_block_arguments_keep_their_program_types_and_isolated_state() {
    let producer = Engine::new().compile("class Box; property items: array<int>; def initialize; @items=[1]; end; def add; @items.push(2); end; end; def make; [Box.new, /a/.match(\"a\")]; end").unwrap();
    let input = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let supplied = input.clone();
    let method = HostMethod::new_with_block("visit", move |call, _, _| {
        call.call_block(std::slice::from_ref(&supplied))
    });
    let opts = CallOptions {
        capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
        ..CallOptions::default()
    };
    let script = Engine::new().compile("def run; visit { |pair| box=pair[0]; m=pair[1]; box.add; begin; box.items.push(\"bad\"); rescue; nil; end; begin; m.captures.push(\"bad\"); rescue; nil; end; [box.items,m.captures] }; end").unwrap();
    common::scope(|scope| {
        let jobs: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| script.call("run", &[], opts.clone()).unwrap()))
            .collect();
        for job in jobs {
            assert_eq!(job.join().unwrap().value.to_string(), "[[1, 2], []]");
        }
    });
    let check = Engine::new()
        .compile("def run(pair); pair[0].items; end")
        .unwrap();
    assert_eq!(
        check
            .call("run", &[input], CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "[1]"
    );
}

#[test]
fn block_capability_methods_cannot_be_detached_or_regranted() {
    for body in [
        "host[:once]",
        "host::once",
        "a=[host[:once]]; 1",
        "host.once(host::once) { 1 }",
    ] {
        assert_eq!(
            run(body, options(&Trace::default())).unwrap_err().kind,
            ErrorKind::Type,
            "{body}"
        );
    }
    let saved = run("host", options(&Trace::default())).unwrap().value;
    let script = Engine::new()
        .compile("def run(old); old.once { 1 }; end")
        .unwrap();
    let error = script
        .call("run", &[saved], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert!(
        error.message.contains("not granted to this call"),
        "{error}"
    );
}

#[test]
fn host_frames_return_to_pending_reads_writes_reductions_and_initializers() {
    let values = HostMethod::new_with_block("host.values", |call, args, _| {
        if call.block_given() {
            call.call_block(args)
        } else {
            call.context().array(args)
        }
    });
    let last = HostMethod::new_with_block("host.pick", |_, args, _| Ok(args[0].clone()));
    let opts = CallOptions {
        capabilities: vec![Capability::new("host", move |_| {
            Ok(Value::object(vec![
                (b"values".to_vec(), values.value()),
                (b"pick".to_vec(), last.value()),
            ]))
        })],
        ..CallOptions::default()
    };
    for (source, expected) in [
        ("host.values([1])[0].push(2)", "[1, 2]"),
        ("host.values([1]) { |a| a }.push(2)", "[1, 2]"),
        ("[host,7].reduce(:pick)", "7"),
        ("module M; C=host.values(3) { |n| n+1 }; end; M.C", "4"),
        ("def run(n=host.values(3) { |x| x+1 }); n; end; run()", "4"),
    ] {
        let script = Engine::new().compile(source).unwrap();
        assert_eq!(
            script
                .run(opts.clone())
                .unwrap_or_else(|error| panic!("{source}: {error}"))
                .value
                .to_string(),
            expected,
            "{source}"
        );
    }
}

#[test]
fn retained_non_utf8_block_errors_preserve_bytes_and_release_storage() {
    let payload = vec![0xff; 4096];
    let expected = payload.clone();
    let method = HostMethod::new_with_block("visit", move |call, _, _| {
        let first = call.call_block(&[]).unwrap_err();
        assert_eq!(first.message_bytes(), expected);
        assert!(first.message.len() > first.message_bytes().len());
        let baseline = call.context().stats().retained_memory_bytes;
        let mut held = Vec::new();
        for _ in 0..16 {
            let error = call.call_block(&[]).unwrap_err();
            assert_eq!(error.message_bytes(), expected);
            held.push(error);
        }
        assert!(call.context().stats().retained_memory_bytes > baseline + 16 * expected.len());
        drop(held);
        assert_eq!(call.context().stats().retained_memory_bytes, baseline);
        Ok(Value::nil())
    });
    let opts = CallOptions {
        globals: [("payload".into(), Value::bytes(payload))].into(),
        capabilities: vec![Capability::new("visit", move |_| Ok(method.value()))],
        ..CallOptions::default()
    };
    let result = run("visit { raise payload }", opts).unwrap();
    assert_eq!(result.stats.retained_memory_bytes, 0);
}
