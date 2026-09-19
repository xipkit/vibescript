use super::{
    entry,
    environment::Environment,
    facts::{Facts, Node},
    inputs::Values,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Script, Value, value::Kind,
};
use std::sync::Arc;

fn fixture() -> (Script, Value) {
    let producer = Engine::new().compile("class Node;property n:int;property links;def initialize;@n=1;@links=[];end;end;def make;a=Node.new;b=Node.new;a.links=[{next:b,again:b}];b.links=[a];a;end").unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new().compile("def run(a,b)->int;a.links[0][:next].links[0].n=7;c=a.class.new;if b.n==7 && c.n==1 && a.links[0][:next]==a.links[0][:again];7;else;false;end;end").unwrap();
    (receiver, value)
}

fn direct_fixture() -> (Script, Value) {
    let producer = Engine::new().compile("class Node;property n:int;property link;def initialize;@n=1;end;end;def make;a=Node.new;b=Node.new;a.link=b;b.link=a;a;end").unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new().compile("def run(a,b)->int;a.link.link.n=7;c=a.class.new;if b.n==7 && c.n==1 && a.link.link==a;7;else;false;end;end").unwrap();
    (receiver, value)
}

fn work(ctx: &mut CallContext, script: &Script, value: &Value, lazy: bool) -> Result<()> {
    let mut options = CallOptions::default();
    let arguments = if lazy {
        options.globals.insert("input".into(), value.clone());
        Vec::new()
    } else {
        vec![value.clone(), value.clone()]
    };
    let checked = entry::check(
        ctx,
        entry::Call {
            script,
            name: "run",
            arguments: &arguments,
            keywords: &[],
            options: &options,
        },
    )?;
    assert!(checked.analysis.issues.data.is_empty(), "{checked:?}");
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert!(matches!(
        checked.facts.node(checked.analysis.returns),
        Node::Integer(7)
    ));
    Ok(())
}

#[test]
fn captured_graphs_obey_limits_and_release_every_interrupted_snapshot() {
    for containers in [false, true] {
        let (eager, value) = if containers {
            fixture()
        } else {
            direct_fixture()
        };
        let path = if containers {
            "links[0][:next].links[0]"
        } else {
            "link.link"
        };
        let lazy = Engine::new()
            .compile(&format!("def run->int;input.{path}.n=7;input.n;end"))
            .unwrap();
        for (script, deferred) in [(&eager, false), (&lazy, true)] {
            let mut ctx = CallContext::new(CallOptions::default());
            work(&mut ctx, script, &value, deferred).unwrap();
            let stats = ctx.stats();
            assert_eq!(stats.retained_memory_bytes, 0);
            let mut again = CallContext::new(CallOptions::default());
            work(&mut again, script, &value, deferred).unwrap();
            if !containers {
                assert_eq!(again.stats().steps, stats.steps);
            }
            assert_eq!(again.stats().peak_memory_bytes, stats.peak_memory_bytes);
            for kind in [ErrorKind::Steps, ErrorKind::Memory] {
                for sample in [0, 1, 4, 8, 12, 15, 16, 17] {
                    // Rebuilt containers have fresh addresses, so metered identity-hash
                    // collisions can change step totals without changing memory capacity.
                    if containers && kind == ErrorKind::Steps && sample >= 16 {
                        continue;
                    }
                    let mut limits = Limits::default();
                    if kind == ErrorKind::Steps {
                        limits.steps = Some(if sample == 17 {
                            stats.steps - 1
                        } else {
                            stats.steps * sample / 16
                        });
                    } else {
                        limits.memory_bytes = Some(if sample == 17 {
                            stats.peak_memory_bytes - 1
                        } else {
                            stats.peak_memory_bytes * sample as usize / 16
                        });
                    }
                    let mut ctx = CallContext::new(CallOptions {
                        limits,
                        ..Default::default()
                    });
                    let result = work(&mut ctx, script, &value, deferred);
                    if sample == 16 {
                        result.unwrap_or_else(|error| panic!("{kind:?}, deferred={deferred}: {error:?}; baseline={stats:?}; actual={:?}", ctx.stats()));
                    } else {
                        assert_eq!(result.unwrap_err().kind, kind);
                        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
                    }
                    assert_eq!(ctx.stats().retained_memory_bytes, 0);
                }
            }
            for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
                let mut ctx = CallContext::new(CallOptions::default());
                if kind == ErrorKind::Cancelled {
                    ctx.cancellation().cancel()
                } else {
                    ctx.options.deadline = Some(std::time::Instant::now())
                }
                assert_eq!(
                    work(&mut ctx, script, &value, deferred).unwrap_err().kind,
                    kind
                );
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }
}

#[test]
fn captured_facts_release_runtime_objects_before_analysis_metadata() {
    let (script, value) = fixture();
    let Kind::Instance(instance) = &value.0 else {
        panic!()
    };
    let weak = Arc::downgrade(instance);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment =
        Environment::new(&mut ctx, &mut facts, &script, &CallOptions::default()).unwrap();
    let mut values = Values::new();
    let admitted = values
        .argument(&mut ctx, &mut facts, &environment.world(), &value)
        .unwrap();
    assert!(!admitted.incomplete);
    assert_eq!(values.captured.objects.data.len(), 2);
    drop(value);
    assert!(weak.upgrade().is_none());
    assert!(matches!(facts.node(admitted.value), Node::Instance { .. }));
    drop((values, environment, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn shared_container_admission_does_not_expand_the_logical_tree() {
    let script = Engine::new().compile("def run(value);7;end").unwrap();
    let mut value = Value::int(1);
    for _ in 0..crate::budget::MAX_VALUE_DEPTH {
        value = Value::array(vec![value.clone(), value]);
    }
    let report = script
        .check_call("run", &[value], &CallOptions::default())
        .unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.stats.retained_memory_bytes, 0);
}
