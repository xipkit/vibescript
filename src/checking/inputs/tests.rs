use super::*;
use crate::{
    CallOptions, Engine, ErrorKind, HostMethod, Limits, Script, Signature,
    checking::{environment::Environment, facts::Node},
};

fn method(result: &str) -> HostMethod {
    HostMethod::new("deliver", |_, _, _| panic!("checker invoked a callback"))
        .with_signature(Signature {
            result: result.into(),
            ..Signature::default()
        })
        .unwrap()
}

fn script(hosts: usize) -> Script {
    let mut engine = Engine::new();
    for index in 0..hosts {
        engine.register_method(format!("registered{index}"), method("bool"));
    }
    engine.compile("def run;7;end").unwrap()
}

fn slot(facts: &Facts, fact: Fact, owner: usize) -> usize {
    let Node::Callable {
        owner: actual,
        target: Callable::Host(index),
    } = facts.node(fact)
    else {
        panic!("expected a host descriptor: {:?}", facts.node(fact));
    };
    assert_eq!(*actual, owner);
    *index
}

#[test]
fn owned_environments_survive_caller_handles_without_retaining_writers() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let (environment, code, callback, writer, input) = {
        let callback = Arc::new(());
        let weak_callback = Arc::downgrade(&callback);
        let writer = Arc::new(());
        let weak_writer = Arc::downgrade(&writer);
        let mut engine = Engine::new();
        engine.register_method(
            "echo",
            HostMethod::new("echo", move |_, _, _| {
                let _keep = &callback;
                panic!("checker invoked a callback");
            })
            .with_signature(Signature {
                params: vec![crate::SignatureParam {
                    name: "value".into(),
                    ty: "Status".into(),
                    optional: false,
                }],
                result: "Status".into(),
                ..Signature::default()
            })
            .unwrap(),
        );
        engine.set_output_writer(move |_, _| {
            let _keep = &writer;
            panic!("checker invoked a writer");
        });
        let script = engine
            .compile("enum Status;Ready;end;def run;echo(:ready).name;tool.deliver();end")
            .unwrap();
        let weak_code = Arc::downgrade(&script.inner.code);
        let input = Value::object(vec![(b"deliver".to_vec(), method("int").value())]);
        let Kind::Hash(hash) = &input.0 else {
            panic!();
        };
        let weak_input = Arc::downgrade(hash);
        let options = CallOptions {
            globals: [
                ("tool".into(), input),
                ("unused".into(), Value::bytes(vec![b'x'; 128 * 1024])),
            ]
            .into(),
            ..CallOptions::default()
        };
        let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
        (
            environment,
            weak_code,
            weak_callback,
            weak_writer,
            weak_input,
        )
    };
    assert!(writer.upgrade().is_none());
    assert!(code.upgrade().is_some());
    assert!(callback.upgrade().is_some());
    assert!(input.upgrade().is_some());
    let function = environment.world().program.names["run"];
    let report = environment
        .analyze(&mut ctx, &mut facts, function, &[])
        .unwrap();
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(report.returns, Atom::Int.fact());
    assert!(ctx.stats().peak_memory_bytes < 128 * 1024);
    drop((report, environment));
    assert!(input.upgrade().is_none());
    assert!(callback.upgrade().is_some());
    drop(facts);
    assert!(code.upgrade().is_none());
    assert!(callback.upgrade().is_none());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn host_metadata_keeps_its_signature_without_keeping_the_callback() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for bound in [false, true] {
        let callback = Arc::new(());
        let weak_callback = Arc::downgrade(&callback);
        let method = HostMethod::new("echo", move |_, _, _| {
            let _keep = &callback;
            panic!("checker invoked a callback");
        })
        .with_signature(Signature {
            result: "Status".into(),
            ..Signature::default()
        })
        .unwrap();
        let signature = Arc::downgrade(&method.compiled_signature().unwrap());
        let host = if bound {
            let value = method.value();
            let Kind::Host(descriptor) = &value.0 else {
                panic!();
            };
            Host::bound(&mut ctx, &mut facts, descriptor).unwrap()
        } else {
            Host::registered(
                &mut ctx,
                &mut facts,
                &crate::capability::Registered::Method(method.clone()),
            )
            .unwrap()
        };
        drop(method);
        assert!(weak_callback.upgrade().is_none());
        assert!(signature.upgrade().is_some());
        assert!(host.unresolved);
        drop(host);
        assert!(signature.upgrade().is_none());
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn cached_roots_and_host_slots_stay_with_their_defining_sources() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut environments = Vec::new();
    for hosts in [0, 2, 1] {
        environments.push(
            Environment::new(
                &mut ctx,
                &mut facts,
                &script(hosts),
                &CallOptions {
                    globals: [("root".into(), Value::int(hosts as i64))].into(),
                    ..CallOptions::default()
                },
            )
            .unwrap(),
        );
    }
    let shared = method("int");
    let other = method("string");
    let mut values = Values::new();
    for which in [0, 1, 2, 2, 0, 1] {
        let world = environments[which].world();
        let hosts = [0, 2, 1][which];
        assert_eq!(world.hosts.len(), hosts);
        let root = values.read(&mut ctx, &mut facts, &world, 0).unwrap();
        assert_eq!(root.value, facts.integer(&mut ctx, hosts as i64).unwrap());
        assert!(!root.incomplete);
        assert_eq!(root.throws, 0);
        for (method, offset, result) in [
            (&shared, 0, Atom::Int.fact()),
            (&other, 1, Atom::String.fact()),
            (&shared, 0, Atom::Int.fact()),
        ] {
            let admitted = values
                .argument(&mut ctx, &mut facts, &world, &method.value())
                .unwrap();
            assert_eq!(
                slot(&facts, admitted.value, world.source_owner),
                hosts + offset
            );
            assert_eq!(
                values
                    .host(&mut ctx, &world, hosts + offset)
                    .unwrap()
                    .unwrap()
                    .result,
                result
            );
        }
        for index in 0..hosts {
            assert_eq!(
                values
                    .host(&mut ctx, &world, index)
                    .unwrap()
                    .unwrap()
                    .result,
                Atom::Bool.fact()
            );
        }
        assert!(values.host(&mut ctx, &world, hosts + 2).unwrap().is_none());
    }
    drop((values, environments, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn cache_work(
    ctx: &mut CallContext,
    fixtures: &[(Script, CallOptions)],
    argument: &Value,
) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let mut values = Values::new();
    for _ in 0..2 {
        for (script, options) in fixtures {
            let environment = Environment::new(ctx, &mut facts, script, options)?;
            let world = environment.world();
            let root = values.read(ctx, &mut facts, &world, 0)?;
            assert!(!root.incomplete);
            let admitted = values.argument(ctx, &mut facts, &world, argument)?;
            let index = slot(&facts, admitted.value, world.source_owner);
            assert_eq!(
                values.host(ctx, &world, index)?.unwrap().result,
                Atom::Int.fact()
            );
        }
    }
    Ok(())
}

#[test]
fn owned_source_caches_share_limits_and_release_every_interrupted_owner() {
    let argument = method("int").value();
    let Kind::Host(descriptor) = &argument.0 else {
        panic!()
    };
    let weak_descriptor = Arc::downgrade(descriptor);
    let fixtures: Vec<_> = [0, 2, 1]
        .into_iter()
        .map(|hosts| {
            (
                script(hosts),
                CallOptions {
                    globals: [("root".into(), Value::array(vec![argument.clone(); 3]))].into(),
                    ..CallOptions::default()
                },
            )
        })
        .collect();
    let descriptor_owners = Arc::strong_count(descriptor);
    let code_owners: Vec<_> = fixtures
        .iter()
        .map(|(script, _)| Arc::strong_count(&script.inner.code))
        .collect();
    let mut ctx = CallContext::new(CallOptions::default());
    cache_work(&mut ctx, &fixtures, &argument).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for sample in 0..=24 {
        for memory in [false, true] {
            let mut limits = Limits::default();
            let expected = if memory {
                limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 24);
                ErrorKind::Memory
            } else {
                limits.steps = Some(stats.steps * sample as u64 / 24);
                ErrorKind::Steps
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let result = cache_work(&mut ctx, &fixtures, &argument);
            if sample == 24 {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().kind, expected);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(Arc::strong_count(descriptor), descriptor_owners);
            for ((script, _), owners) in fixtures.iter().zip(&code_owners) {
                assert_eq!(Arc::strong_count(&script.inner.code), *owners);
            }
        }
    }
    for memory in [false, true] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(stats.peak_memory_bytes - usize::from(memory)),
                steps: Some(stats.steps - u64::from(!memory)),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            cache_work(&mut ctx, &fixtures, &argument).unwrap_err().kind,
            if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            }
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    drop((fixtures, argument));
    assert!(weak_descriptor.upgrade().is_none());
}

#[test]
fn cached_metadata_cannot_bypass_latched_limits_or_cancellation() {
    for reason in [
        ErrorKind::Steps,
        ErrorKind::Memory,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let environment = Environment::new(
            &mut ctx,
            &mut facts,
            &script(1),
            &CallOptions {
                globals: [("root".into(), Value::int(7))].into(),
                ..CallOptions::default()
            },
        )
        .unwrap();
        let world = environment.world();
        let argument = method("int").value();
        let mut values = Values::new();
        values.read(&mut ctx, &mut facts, &world, 0).unwrap();
        values
            .argument(&mut ctx, &mut facts, &world, &argument)
            .unwrap();
        match reason {
            ErrorKind::Steps => {
                ctx.charge(u64::MAX).unwrap_err();
            }
            ErrorKind::Memory => {
                ctx.reserve(usize::MAX).unwrap_err();
            }
            ErrorKind::Cancelled => ctx.cancellation().cancel(),
            ErrorKind::Deadline => ctx.options.deadline = Some(std::time::Instant::now()),
            _ => unreachable!(),
        }
        for index in [0, 1, 99] {
            assert_eq!(
                values.host(&mut ctx, &world, index).unwrap_err().kind,
                reason
            );
            assert_eq!(
                values
                    .read(&mut ctx, &mut facts, &world, index)
                    .unwrap_err()
                    .kind,
                reason
            );
        }
        assert_eq!(
            values
                .argument(&mut ctx, &mut facts, &world, &argument)
                .unwrap_err()
                .kind,
            reason
        );
        drop((values, environment, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn public_reports_release_owned_inputs_and_code_in_every_check_scope() {
    for scope in 0..3 {
        let method = method("int");
        let value = method.value();
        let Kind::Host(descriptor) = &value.0 else {
            panic!()
        };
        let weak_descriptor = Arc::downgrade(descriptor);
        let script = Engine::new().compile("def run;tool.deliver();end").unwrap();
        let weak_code = Arc::downgrade(&script.inner.code);
        let options = CallOptions {
            globals: [(
                "tool".into(),
                Value::object(vec![(b"deliver".to_vec(), value)]),
            )]
            .into(),
            ..CallOptions::default()
        };
        let report = match scope {
            0 => script.check(&options),
            1 => script.check_function("run", &options),
            _ => script.check_call("run", &[], &options),
        }
        .unwrap();
        assert!(report.is_clean(), "{report:?}");
        drop((script, options, method));
        assert!(weak_descriptor.upgrade().is_none());
        assert!(weak_code.upgrade().is_none());
        assert!(report.is_clean());
    }
}
