mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, Capability, Engine, Error, ErrorClass, ErrorKind, HostMethod, Limits, Value,
};

fn granted(method: HostMethod) -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::new("sms", move |_| {
            Ok(Value::object(vec![(b"deliver".to_vec(), method.value())]))
        })],
        ..CallOptions::default()
    }
}

fn echo() -> HostMethod {
    HostMethod::new("sms.deliver", |ctx, args, keywords| {
        let mut values = args.to_vec();
        values.extend(keywords.iter().map(|(_, value)| value.clone()));
        ctx.array(&values)
    })
}

#[test]
fn capability_methods_support_named_scoped_computed_and_forwarded_calls() {
    for source in [
        "sms.deliver(1, 2)",
        "sms.deliver 1, 2",
        "sms::deliver(1, 2)",
        "sms[:deliver](1, 2)",
        "(sms[:deliver])(1, 2)",
        "sms.send(:deliver, 1, 2)",
        "sms.public_send(:deliver, 1, 2)",
        "sms.deliver(*[1], last: 2)",
        "sms.deliver(1, **{last: 2})",
        "local = sms; local.deliver(1, 2)",
        "sms.dup.deliver(1, 2)",
        "[sms][0].deliver(1, 2)",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let output = script
            .run(granted(echo()))
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(output.value.to_string(), "[1, 2]", "{source}");
    }
}

#[test]
fn grants_bind_in_order_before_initializers() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine
        .compile("module M; C=sms.deliver(3); end; M.C")
        .unwrap();
    let mut opts = granted(echo());
    let observed = order.clone();
    opts.capabilities.insert(
        0,
        Capability::new("sms", move |_| {
            observed.lock().unwrap().push(1);
            Ok(Value::nil())
        }),
    );
    assert_eq!(script.run(opts).unwrap().value.to_string(), "[3]");
    assert_eq!(*order.lock().unwrap(), [1]);
    assert!(script.run(CallOptions::default()).is_err());
}

#[test]
fn explicit_globals_take_precedence_over_capability_bindings() {
    let bound = Arc::new(AtomicUsize::new(0));
    let observed = bound.clone();
    let opts = CallOptions {
        globals: [("sms".into(), Value::int(7))].into(),
        capabilities: vec![Capability::new("sms", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::object(vec![(b"deliver".to_vec(), echo().value())]))
        })],
        ..CallOptions::default()
    };
    let script = Engine::new().compile("sms").unwrap();
    assert_eq!(script.run(opts).unwrap().value.as_int(), Some(7));
    assert_eq!(bound.load(Ordering::Relaxed), 1);
}

#[test]
fn explicit_global_method_grants_remain_callable_without_becoming_values() {
    let method = echo();
    let cap_method = method.clone();
    let granted = CallOptions {
        capabilities: vec![Capability::new("deliver", move |_| Ok(cap_method.value()))],
        ..CallOptions::default()
    };
    let globals = CallOptions {
        globals: [("deliver".into(), method.value())].into(),
        ..CallOptions::default()
    };
    let mut engine = Engine::new();
    engine.register("deliver", |_, _| panic!("shadowed registered function ran"));
    for options in [granted, globals] {
        for source in ["deliver(1, 2)", "deliver(*[1, 2])", "(deliver)(1, 2)"] {
            let output = engine
                .compile(source)
                .unwrap()
                .run(options.clone())
                .unwrap();
            assert_eq!(output.value.to_string(), "[1, 2]", "{source}");
        }
        for source in ["deliver", "value=deliver;value(1)", "[deliver]"] {
            let error = engine
                .compile(source)
                .unwrap()
                .run(options.clone())
                .unwrap_err();
            assert!(
                error.message.contains("cannot be used as a value"),
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn deferred_callback_metadata_stays_charged_until_destruction() {
    struct LiveDefinition(Arc<AtomicUsize>);
    impl Drop for LiveDefinition {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let observed = live.clone();
    let maximum = peak.clone();
    let method = HostMethod::new("sms.deliver", move |ctx, _, _| {
        let count = observed.load(Ordering::Relaxed);
        assert!(ctx.stats().retained_memory_bytes >= count * 8192);
        observed.fetch_add(1, Ordering::Relaxed);
        maximum.fetch_max(count + 1, Ordering::Relaxed);
        let definition = LiveDefinition(observed.clone());
        let descriptor = HostMethod::new("x".repeat(8192), move |_, _, _| {
            let _ = &definition;
            Ok(Value::nil())
        });
        Ok(Value::object(vec![(b"run".to_vec(), descriptor.value())]))
    });
    let script = Engine::new()
        .compile("i=0;while i<16;temporary=sms.deliver();temporary=nil;i+=1;end;7")
        .unwrap();
    let output = script.run(granted(method.clone())).unwrap();
    assert_eq!(live.load(Ordering::Relaxed), 0);
    assert!(output.stats.peak_memory_bytes >= peak.load(Ordering::Relaxed) * 8192);
    assert_eq!(output.stats.retained_memory_bytes, 0);
    let mut opts = granted(method);
    opts.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(script.run(opts).unwrap_err().kind, ErrorKind::Memory);
    assert_eq!(live.load(Ordering::Relaxed), 0);
}

#[test]
fn capability_factories_create_fresh_state_for_each_concurrent_invocation() {
    let calls = CallOptions {
        capabilities: vec![Capability::new("counter", |_| {
            let count = AtomicUsize::new(0);
            let method = HostMethod::new("counter.bump", move |_, _, _| {
                Ok(Value::int(count.fetch_add(1, Ordering::Relaxed) as i64 + 1))
            });
            Ok(Value::object(vec![(b"bump".to_vec(), method.value())]))
        })],
        ..CallOptions::default()
    };
    let script = Engine::new()
        .compile("[counter.bump(), counter.bump()]")
        .unwrap();
    common::scope(|scope| {
        let workers: Vec<_> = (0..6)
            .map(|_| {
                scope.spawn(|| {
                    for _ in 0..3 {
                        let output = script.run(calls.clone()).unwrap();
                        assert_eq!(output.value.to_string(), "[1, 2]");
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
}

#[test]
fn contracts_validate_arguments_before_effects_and_each_successful_result() {
    let effects = Arc::new(AtomicUsize::new(0));
    let returns = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let validated = returns.clone();
    let method = HostMethod::new("sms.deliver", move |_, args, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(args[0].clone())
    })
    .with_contract(
        |_, args, keywords| {
            if args.len() != 1 || args[0].as_int().is_none() || !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Type,
                    "sms.deliver requires one integer",
                ));
            }
            Ok(())
        },
        move |_, value| {
            validated.fetch_add(1, Ordering::Relaxed);
            if value.as_int() == Some(0) {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "sms.deliver cannot return zero",
                ));
            }
            Ok(())
        },
    );
    let script = Engine::new()
        .compile("def run(n); sms.deliver(n); end")
        .unwrap();
    let error = script
        .call("run", &[Value::bytes("bad")], granted(method.clone()))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(returns.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call("run", &[Value::int(7)], granted(method.clone()))
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    let error = script
        .call("run", &[Value::int(0)], granted(method))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert!(error.diagnostic.is_some());
    assert_eq!(effects.load(Ordering::Relaxed), 2);
    assert_eq!(returns.load(Ordering::Relaxed), 2);
}

#[test]
fn contracts_follow_method_identity_including_factory_results() {
    let protected = HostMethod::new("shared.name", |_, _, _| panic!("contract was bypassed"))
        .with_contract(
            |_, _, _| {
                Err(Error::new(ErrorKind::Type, "private contract").with_class(ErrorClass::Type))
            },
            |_, _| Ok(()),
        );
    let factory = HostMethod::new("factory.make", move |_, _, _| {
        Ok(Value::object(vec![(
            b"deliver".to_vec(),
            protected.value(),
        )]))
    });
    let other = HostMethod::new("shared.name", |_, _, _| Ok(Value::int(42)));
    let opts = CallOptions {
        capabilities: vec![
            Capability::new("factory", move |_| {
                Ok(Value::object(vec![(b"make".to_vec(), factory.value())]))
            }),
            Capability::new("other", move |_| {
                Ok(Value::object(vec![(b"deliver".to_vec(), other.value())]))
            }),
        ],
        ..CallOptions::default()
    };
    let script = Engine::new().compile("result=begin; factory.make().deliver(); rescue TypeError; 7; end; [result, other.deliver(), {a: 1}.merge({b: 2})]").unwrap();
    assert_eq!(
        script.run(opts).unwrap().value.to_string(),
        "[7, 42, {a: 1, b: 2}]"
    );
}

#[test]
fn saved_namespaces_cannot_reuse_grants_in_later_calls_even_with_unlimited_memory() {
    let script = Engine::new()
        .compile("def save; sms; end; def use(saved); saved.deliver(); end")
        .unwrap();
    for unlimited in [false, true] {
        let mut opts = granted(echo());
        if unlimited {
            opts.limits.memory_bytes = None;
        }
        let saved = script.call("save", &[], opts.clone()).unwrap().value;
        for mut receiving in [CallOptions::default(), opts] {
            if unlimited {
                receiving.limits.memory_bytes = None;
            }
            let error = script
                .call("use", std::slice::from_ref(&saved), receiving)
                .unwrap_err();
            assert!(
                error.message.contains("was not granted to this call"),
                "{error}"
            );
        }
    }
}

#[test]
fn capability_methods_cannot_escape_through_reads_containers_or_host_arguments() {
    for source in [
        "sms.deliver",
        "sms::deliver",
        "sms[:deliver]",
        "a=sms::deliver; a(1)",
        "a=sms[:deliver]; a(1)",
        "[sms[:deliver]]",
        "{f: sms[:deliver]}",
        "identity(sms[:deliver])",
        "sms.deliver.to_s",
        "sms[:deliver].clone",
    ] {
        let mut engine = Engine::new();
        engine.register("identity", |_, _| panic!("detached method reached host"));
        let error = engine
            .compile(source)
            .unwrap()
            .run(granted(echo()))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
        assert!(
            error.message.contains("cannot be used as a value"),
            "{source}: {error}"
        );
    }
}

#[test]
fn strict_effects_reject_method_globals_before_binding_any_capabilities() {
    let invoked = Arc::new(AtomicUsize::new(0));
    let observed = invoked.clone();
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("7").unwrap();
    let opts = CallOptions {
        globals: [(
            "hidden".to_owned(),
            Value::object(vec![(b"f".to_vec(), echo().value())]),
        )]
        .into(),
        capabilities: vec![Capability::new("sms", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        })],
        ..CallOptions::default()
    };
    assert!(script.run(opts).unwrap_err().message.contains("data-only"));
    assert_eq!(invoked.load(Ordering::Relaxed), 0);
}

#[test]
fn cancellation_and_latched_exhaustion_stop_every_callback_boundary() {
    for stage in 0..3 {
        let effects = Arc::new(AtomicUsize::new(0));
        let observed = effects.clone();
        let method = HostMethod::new("sms.deliver", move |ctx, _, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            if stage == 1 {
                ctx.cancellation().cancel();
            }
            Ok(Value::int(1))
        })
        .with_contract(
            move |ctx, _, _| {
                if stage == 0 {
                    ctx.cancellation().cancel();
                }
                Ok(())
            },
            move |ctx, _| {
                if stage == 2 {
                    ctx.cancellation().cancel();
                }
                Ok(())
            },
        );
        let script = Engine::new()
            .compile("begin; sms.deliver(); rescue; 9; ensure; sms.deliver(); end")
            .unwrap();
        assert_eq!(
            script.run(granted(method)).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        assert_eq!(effects.load(Ordering::Relaxed), usize::from(stage != 0));
    }
    let method = HostMethod::new("sms.deliver", |_, _, _| {
        panic!("exhausted contract ran callback")
    })
    .with_contract(
        |ctx, _, _| {
            let _ = ctx.charge(u64::MAX);
            Ok(())
        },
        |_, _| Ok(()),
    );
    let script = Engine::new()
        .compile("begin; sms.deliver(); rescue; 9; end")
        .unwrap();
    assert_eq!(
        script.run(granted(method)).unwrap_err().kind,
        ErrorKind::Steps
    );
}

#[test]
fn names_metadata_and_results_are_charged_and_released() {
    let method = HostMethod::new("sms.deliver", |_, _, _| Ok(Value::int(7)));
    let script = Engine::new().compile("sms.deliver()").unwrap();
    let output = script.run(granted(method.clone())).unwrap();
    assert_eq!(output.stats.retained_memory_bytes, 0);
    for (steps, memory, error) in [
        (
            Some(output.stats.steps),
            Some(output.stats.peak_memory_bytes),
            None,
        ),
        (
            Some(output.stats.steps - 1),
            Some(output.stats.peak_memory_bytes),
            Some(ErrorKind::Steps),
        ),
        (
            Some(output.stats.steps),
            Some(output.stats.peak_memory_bytes - 1),
            Some(ErrorKind::Memory),
        ),
    ] {
        let mut opts = granted(method.clone());
        opts.limits = Limits {
            steps,
            memory_bytes: memory,
            ..Limits::default()
        };
        match error {
            None => assert_eq!(script.run(opts).unwrap().value.as_int(), Some(7)),
            Some(expected) => assert_eq!(script.run(opts).unwrap_err().kind, expected),
        }
    }
    let large = HostMethod::new("sms.deliver", |_, _, _| {
        Ok(Value::bytes(vec![b'x'; 100_000]))
    });
    let mut opts = granted(large);
    opts.limits.memory_bytes = Some(32_768);
    assert_eq!(script.run(opts).unwrap_err().kind, ErrorKind::Memory);
}

#[test]
fn host_returns_are_isolated_and_can_admit_foreign_script_programs() {
    let foreign = Engine::new()
        .compile("module M; @@n=0; def self.bump; @@n+=1; end; end; M")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let method = HostMethod::new("sms.deliver", move |_, _, _| Ok(foreign.clone()));
    let script = Engine::new()
        .compile("m=sms.deliver(); [m.bump(), m.bump()]")
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            script
                .run(granted(method.clone()))
                .unwrap()
                .value
                .to_string(),
            "[1, 2]"
        );
    }
    let data = Value::array(vec![Value::int(1)]);
    let retained = data.clone();
    let method = HostMethod::new("sms.deliver", move |_, _, _| Ok(data.clone()));
    let script = Engine::new()
        .compile("a=sms.deliver(); a.push(2); [a, sms.deliver()]")
        .unwrap();
    assert_eq!(
        script.run(granted(method)).unwrap().value.to_string(),
        "[[1, 2], [1]]"
    );
    assert_eq!(retained.to_string(), "[1]");
}

#[test]
fn rejected_blocks_and_missing_members_skip_callback_effects() {
    let method = HostMethod::new("sms.deliver", |_, _, _| {
        panic!("invalid invocation ran host")
    });
    let script = Engine::new()
        .compile("begin; sms.deliver(1) { 2 }; rescue ArgumentError; 7; end")
        .unwrap();
    assert_eq!(
        script.run(granted(method.clone())).unwrap().value.as_int(),
        Some(7)
    );
    let script = Engine::new()
        .compile("begin; sms.missing(sms.deliver()); rescue; 9; end")
        .unwrap();
    assert_eq!(script.run(granted(method)).unwrap().value.as_int(), Some(9));
}

#[test]
fn selected_methods_survive_argument_replacement_of_their_namespace() {
    for name in ["deliver", "push", "call", "map", "send"] {
        for args in ["replace()", "*[replace()]"] {
            let method = HostMethod::new(format!("sms.{name}"), |_, args, _| Ok(args[0].clone()));
            let member = name.as_bytes().to_vec();
            let opts = CallOptions {
                capabilities: vec![Capability::new("sms", move |_| {
                    Ok(Value::object(vec![(member.clone(), method.value())]))
                })],
                ..CallOptions::default()
            };
            let source = format!("def replace; sms=nil; 42; end; [sms.{name}({args}), sms]");
            let script = Engine::new().compile(&source).unwrap();
            assert_eq!(
                script
                    .run(opts)
                    .unwrap_or_else(|error| panic!("{source}: {error}"))
                    .value
                    .to_string(),
                "[42, nil]"
            );
        }
    }
}

#[test]
fn binding_is_guarded_before_and_after_host_code_and_precedes_initialization() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("module M; C=effect(); end; 7").unwrap();
    let bound = Arc::new(AtomicUsize::new(0));
    let observed = bound.clone();
    let capability = Capability::new("sms", move |ctx| {
        observed.fetch_add(1, Ordering::Relaxed);
        ctx.cancellation().cancel();
        Err(Error::new(ErrorKind::Runtime, "binding failed"))
    });
    let opts = CallOptions {
        capabilities: vec![capability],
        ..CallOptions::default()
    };
    assert_eq!(script.run(opts).unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    assert_eq!(bound.load(Ordering::Relaxed), 1);
    for failure in [
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
        ErrorKind::Memory,
        ErrorKind::Steps,
    ] {
        let capability = Capability::new("sms", |_| panic!("refused binding ran"));
        let mut opts = CallOptions {
            capabilities: vec![capability],
            ..CallOptions::default()
        };
        match failure {
            ErrorKind::Cancelled => opts.cancellation.cancel(),
            ErrorKind::Deadline => opts.deadline = Some(std::time::Instant::now()),
            ErrorKind::Memory => opts.limits.memory_bytes = Some(0),
            ErrorKind::Steps => opts.limits.steps = Some(0),
            _ => unreachable!(),
        }
        assert_eq!(script.run(opts).unwrap_err().kind, failure);
    }
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[test]
fn expired_grants_stay_revoked_inside_foreign_instance_graphs() {
    let script = Engine::new().compile(
        "class Box; def initialize(cap); @saved=cap; @next=self; end; def next_node; @next; end; def read; @saved.deliver(); end; end; def make; Box.new(sms); end"
    ).unwrap();
    let saved = script.call("make", &[], granted(echo())).unwrap().value;
    let consumer = Engine::new()
        .compile("def run(box); box.next_node.read; end")
        .unwrap();
    for opts in [CallOptions::default(), granted(echo())] {
        let error = consumer
            .call("run", std::slice::from_ref(&saved), opts)
            .unwrap_err();
        assert!(
            error.message.contains("was not granted to this call"),
            "{error}"
        );
    }
}

#[test]
fn host_retention_keeps_charges_and_a_new_call_accounts_its_own_namespace() {
    let retained = Arc::new(Mutex::new(None));
    let held = retained.clone();
    let mut engine = Engine::new();
    engine.register("retain", move |_, args| {
        *held.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    let script = engine.compile("retain(sms); 7").unwrap();
    let output = script.run(granted(echo())).unwrap();
    assert!(output.stats.retained_memory_bytes > 0);
    let saved = retained.lock().unwrap().take().unwrap();
    let receiver = Engine::new()
        .compile("def take(value); value; end")
        .unwrap();
    let copied = receiver
        .call("take", &[saved], CallOptions::default())
        .unwrap();
    assert!(copied.stats.retained_memory_bytes > 0);
    let mut limits = CallOptions::default();
    limits.limits.memory_bytes = Some(copied.stats.peak_memory_bytes - 1);
    assert_eq!(
        receiver
            .call("take", &[copied.value], limits)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}
