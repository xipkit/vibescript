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
    let producer = Engine::legacy_unchecked().compile("class Node;property n:int;property links;def initialize;@n=1;@links=[];end;end;def make;a=Node.new;b=Node.new;a.links=[{next:b,again:b}];b.links=[a];a;end").unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::legacy_unchecked().compile("def run(a,b)->int;a.links[0][:next].links[0].n=7;c=a.class.new;if b.n==7 && c.n==1 && a.links[0][:next]==a.links[0][:again];7;else;false;end;end").unwrap();
    (receiver, value)
}

fn direct_fixture() -> (Script, Value) {
    let producer = Engine::legacy_unchecked().compile("class Node;property n:int;property link;def initialize;@n=1;end;end;def make;a=Node.new;b=Node.new;a.link=b;b.link=a;a;end").unwrap();
    let value = producer
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::legacy_unchecked().compile("def run(a,b)->int;a.link.link.n=7;c=a.class.new;if b.n==7 && c.n==1 && a.link.link==a;7;else;false;end;end").unwrap();
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
        let lazy = Engine::legacy_unchecked()
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
    let script = Engine::legacy_unchecked()
        .compile("def run(value);7;end")
        .unwrap();
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

fn deferred_type_fixture() -> (Engine, CallOptions) {
    use crate::{HostMethod, Signature};
    let child = |marker| {
        Engine::legacy_unchecked()
            .compile(&format!(
                "class Child;trace.push({marker});end;def make;Child;end"
            ))
            .unwrap()
            .call(
                "make",
                &[],
                CallOptions {
                    globals: [("trace".into(), Value::array(vec![]))].into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .value
    };
    let mut engine = Engine::legacy_unchecked();
    engine.register_method(
        "choose",
        HostMethod::new("choose", |_, _, _| Ok(Value::boolean(true)))
            .with_signature(Signature {
                params: vec![],
                result: "bool".into(),
                accepts_block: false,
            })
            .unwrap(),
    );
    let producer = engine.compile("class Remote;if choose();left;else;right;end;def self.value;7;end;end;def make;Remote;end").unwrap();
    let mut options = CallOptions {
        globals: [
            ("trace".into(), Value::array(vec![])),
            ("left".into(), child(1)),
            ("right".into(), child(2)),
        ]
        .into(),
        ..Default::default()
    };
    let value = producer.call("make", &[], options.clone()).unwrap().value;
    options.globals.insert("C".into(), value);
    (engine, options)
}

#[test]
fn deferred_branch_continuations_release_storage_at_every_interrupted_boundary() {
    let (_, options) = deferred_type_fixture();
    let script = Engine::legacy_unchecked().compile("def take(x:C?=nil)->C?;nil;end;def run->int;take(nil);take();C.value;if trace.length==1;7;else;false;end;end").unwrap();
    let work = |ctx: &mut CallContext| -> Result<()> {
        let checked = entry::check(
            ctx,
            entry::Call {
                script: &script,
                name: "run",
                arguments: &[],
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
    };
    metered_deferred_analysis(work);
}

fn metered_deferred_analysis(work: impl Fn(&mut CallContext) -> Result<()>) {
    let mut context = CallContext::new(CallOptions::default());
    work(&mut context).unwrap();
    let stats = context.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        for sample in [0, 1, 4, 8, 12, 15, 16, 17] {
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
            let mut context = CallContext::new(CallOptions {
                limits,
                ..Default::default()
            });
            let result = work(&mut context);
            if sample == 16 {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(context.checkpoint().unwrap_err().kind, kind);
            }
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let mut context = CallContext::new(CallOptions::default());
        if kind == ErrorKind::Cancelled {
            context.cancellation().cancel();
        } else {
            context.options.deadline = Some(std::time::Instant::now());
        }
        assert_eq!(work(&mut context).unwrap_err().kind, kind);
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn deferred_property_and_predicate_continuations_release_interrupted_storage() {
    for (body, rejected) in [
        ("@item=nil", false),
        ("bind(nil)", false),
        ("@items.push(nil)", false),
        ("@items[0]=nil", false),
        ("@items.fill{nil}", false),
        ("begin;@items.push(1);rescue RuntimeError;nil;end", true),
    ] {
        let (_, options) = deferred_type_fixture();
        let producer = Engine::legacy_unchecked().compile(&format!("class Holder;property item:C?;property items:array<C?>;def initialize;@items=[nil];end;def bind(@item);end;def work;{body};7;end;end;def make;Holder.new;end")).unwrap();
        let holder = producer.call("make", &[], options.clone()).unwrap().value;
        let receiver = Engine::legacy_unchecked()
            .compile("def run(box)->int;box.work;end")
            .unwrap();
        metered_deferred_analysis(|ctx| {
            deferred_result(
                ctx,
                &receiver,
                std::slice::from_ref(&holder),
                &options,
                rejected,
            )
        });
    }
    for receiver in ["nil", "Local.new"] {
        let (engine, options) = deferred_type_fixture();
        let script = engine.compile(&format!("class Local;end;def run->int;name=if choose();\"C\";else;\"int\";end;{receiver}.is_type?(name);7;end")).unwrap();
        metered_deferred_analysis(|ctx| deferred_result(ctx, &script, &[], &options, false));
    }
}

fn deferred_result(
    ctx: &mut CallContext,
    script: &Script,
    arguments: &[Value],
    options: &CallOptions,
    rejected: bool,
) -> Result<()> {
    let checked = entry::check(
        ctx,
        entry::Call {
            script,
            name: "run",
            arguments,
            keywords: &[],
            options,
        },
    )?;
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert_eq!(
        checked.analysis.issues.data.len(),
        usize::from(rejected),
        "{checked:?}"
    );
    if rejected {
        assert!(matches!(
            checked.analysis.issues.data[0].issue.kind,
            super::flow::IssueKind::Property { .. }
        ));
    }
    assert!(
        matches!(
            checked.facts.node(checked.analysis.returns),
            Node::Integer(7)
        ),
        "{checked:?}"
    );
    Ok(())
}
