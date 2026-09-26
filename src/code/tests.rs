use crate::{
    CallContext, CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value,
    namespace::Namespace, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = r#"
module Outer
  module Inner
    def self.value -> any; host(); end
  end
end
class Node
  property link: Node?
  def value -> any; host(); end
end
def namespace -> any; Outer; end
def nested -> any; Outer::Inner; end
def class_value -> any; Node; end
def cycle -> Node
  node = Node.new
  node.link = node
  node
end
def host_value -> int; host().as(int); end
"#;

#[test]
fn compiler_diagnostics_preserve_latched_control_errors() {
    let source = format!("{}def", "# text\n".repeat(256));
    let registered = std::collections::BTreeMap::new();
    let mut unlimited = CallOptions::default();
    unlimited.limits.steps = None;
    let mut context = CallContext::new(unlimited.clone());
    let error = super::Code::compile_mode(
        &source,
        registered.iter(),
        true,
        None,
        &crate::compilation::Meter(std::cell::RefCell::new(&mut context)),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert!(error.diagnostic.is_some());
    let total = context.stats().steps;
    for steps in [1, total / 2, total - 1] {
        let mut options = unlimited.clone();
        options.limits.steps = Some(steps);
        let mut context = CallContext::new(options);
        let error = super::Code::compile_mode(
            &source,
            registered.iter(),
            true,
            None,
            &crate::compilation::Meter(std::cell::RefCell::new(&mut context)),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert!(error.diagnostic.is_none());
        assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    }
}

#[test]
fn compilation_preserves_borrowed_host_order_identity_and_partial_failure_cleanup() {
    use crate::{capability::Registered, compilation::Meter};
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        time::Instant,
    };

    let retired = Arc::new(AtomicUsize::new(0));
    let called = Arc::new(AtomicUsize::new(0));
    let registered: BTreeMap<_, _> = (0..32)
        .map(|index| {
            let guard = Retired(retired.clone());
            let called = called.clone();
            let callback: crate::HostCallback = Arc::new(move |_, _, _| {
                let _ = &guard;
                called.fetch_add(1, Ordering::SeqCst);
                Ok(Value::int(index))
            });
            (format!("host_{index:02}"), Registered::Callback(callback))
        })
        .collect();
    let source = "def value; [host_00(), host_31()]; end";
    let run = |context: &mut CallContext| {
        super::Code::compile_mode(
            source,
            registered.iter().rev(),
            true,
            None,
            &Meter(RefCell::new(context)),
        )
    };
    let mut context = CallContext::new(CallOptions::default());
    let budget = Arc::downgrade(&context.identity());
    let code = run(&mut context).unwrap();
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert_eq!(
        code.program.hosts,
        registered.keys().rev().cloned().collect::<Vec<_>>()
    );
    for (index, (name, callback)) in code.program.hosts.iter().zip(&code.hosts).enumerate() {
        let (Registered::Callback(original), Registered::Callback(callback)) =
            (&registered[name], callback)
        else {
            panic!("expected plain host callbacks");
        };
        assert!(Arc::ptr_eq(original, callback));
        assert_eq!(
            callback(&mut context, &[], &[]).unwrap().as_int(),
            Some(31 - index as i64)
        );
    }
    let stats = context.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    drop(context);
    assert!(budget.upgrade().is_none());
    drop(code);
    for memory in [false, true] {
        for short in [0, 1] {
            let mut options = CallOptions::default();
            if memory {
                options.limits.memory_bytes = Some(stats.peak_memory_bytes - short);
            } else {
                options.limits.steps = Some(stats.steps - short as u64);
            }
            let mut context = CallContext::new(options);
            let result = run(&mut context);
            if short == 0 {
                drop(result.unwrap());
            } else {
                let kind = if memory {
                    ErrorKind::Memory
                } else {
                    ErrorKind::Steps
                };
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(context.checkpoint().unwrap_err().kind, kind);
            }
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }
    for deadline in [false, true] {
        for at in [0, 15, 31, 32, 47, 63] {
            let mut context = CallContext::new(CallOptions::default());
            let visited = Cell::new(0);
            let work = Meter(RefCell::new(&mut context));
            let input = registered.iter().rev().inspect(|_| {
                if visited.get() == at {
                    let context = &mut *work.0.borrow_mut();
                    if deadline {
                        context.options.deadline = Some(Instant::now());
                    } else {
                        context.cancellation().cancel();
                    }
                }
                visited.set(visited.get() + 1);
            });
            let error = super::Code::compile_mode(source, input, true, None, &work).unwrap_err();
            let kind = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            assert_eq!(error.kind, kind, "host visit {at}");
            assert_eq!(context.checkpoint().unwrap_err().kind, kind);
            assert_eq!(context.stats().retained_memory_bytes, 0);
            for callback in registered.values() {
                let Registered::Callback(callback) = callback else {
                    unreachable!()
                };
                assert_eq!(Arc::strong_count(callback), 1);
            }
        }
    }
    assert_eq!(retired.load(Ordering::SeqCst), 0);
    assert_eq!(called.load(Ordering::SeqCst), 32);
    drop(registered);
    assert_eq!(retired.load(Ordering::SeqCst), 32);
}

struct Retired(Arc<AtomicUsize>);

impl Drop for Retired {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn engine(retired: &Arc<AtomicUsize>) -> Engine {
    let guard = Retired(retired.clone());
    let mut engine = crate::Engine::new();
    engine.register("host", move |_, _| {
        let _ = &guard;
        Ok(Value::int(11))
    });
    engine
}

#[test]
fn unused_namespace_templates_do_not_retain_code_or_hosts() {
    let retired = Arc::new(AtomicUsize::new(0));
    let script = engine(&retired).compile(SOURCE).unwrap();
    let code = Arc::downgrade(&script.inner.code);
    let definitions = script
        .inner
        .code
        .program
        .namespaces
        .iter()
        .map(Arc::downgrade)
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 3);
    drop(script);
    assert!(code.upgrade().is_none());
    assert!(
        definitions
            .iter()
            .all(|definition| definition.upgrade().is_none())
    );
    assert_eq!(retired.load(Ordering::SeqCst), 1);
}

#[test]
fn escaping_namespaces_classes_and_cycles_retain_original_code_until_released() {
    for function in ["namespace", "nested", "class_value", "cycle"] {
        let retired = Arc::new(AtomicUsize::new(0));
        let mut engine = engine(&retired);
        let script = engine.compile(SOURCE).unwrap();
        let code = Arc::downgrade(&script.inner.code);
        let configuration = Arc::downgrade(&script.inner);
        engine.register("host", |_, _| Ok(Value::int(22)));
        assert_eq!(
            script
                .call("host_value", &[], CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(11)
        );
        let value = script
            .call(function, &[], CallOptions::default())
            .unwrap()
            .value;
        let namespace = match &value.0 {
            Kind::Namespace(namespace) => namespace,
            Kind::Instance(instance) => instance.class(),
            _ => panic!("unexpected result from {function}"),
        };
        assert!(Arc::ptr_eq(
            namespace.owner.as_ref().unwrap(),
            &script.inner.code
        ));
        drop(script);
        drop(engine);
        assert!(configuration.upgrade().is_none(), "{function}");
        assert!(code.upgrade().is_some(), "{function}");
        assert_eq!(retired.load(Ordering::SeqCst), 0, "{function}");
        drop(value);
        assert!(code.upgrade().is_none(), "{function}");
        assert_eq!(retired.load(Ordering::SeqCst), 1, "{function}");
    }
}

#[test]
fn imported_namespace_releases_the_source_budget_and_retains_code() {
    let retired = Arc::new(AtomicUsize::new(0));
    let script = engine(&retired).compile(SOURCE).unwrap();
    let code = Arc::downgrade(&script.inner.code);
    let template = Namespace::untracked(script.inner.code.program.namespaces[0].clone());
    let mut source = CallContext::new(CallOptions::default());
    let original = Value(Kind::Namespace(
        Namespace::import(&mut source, &template).unwrap(),
    ));
    let charged = source.stats().retained_memory_bytes;
    assert!(charged > 0);
    let mut receiver = CallContext::new(CallOptions::default());
    let copied = receiver.import(&original).unwrap();
    assert_eq!(receiver.stats().retained_memory_bytes, charged);
    drop(original);
    drop(template);
    drop(script);
    assert_eq!(source.stats().retained_memory_bytes, 0);
    assert!(code.upgrade().is_some());
    drop(copied);
    assert_eq!(receiver.stats().retained_memory_bytes, 0);
    assert!(code.upgrade().is_none());
    assert_eq!(retired.load(Ordering::SeqCst), 1);
}

#[test]
fn imported_instance_cycle_releases_original_heap_and_code() {
    let retired = Arc::new(AtomicUsize::new(0));
    let script = engine(&retired).compile(SOURCE).unwrap();
    let code = Arc::downgrade(&script.inner.code);
    let namespace = Namespace::untracked(script.inner.code.program.namespaces[2].clone());
    let mut source = CallContext::new(CallOptions::default());
    let root = crate::objects::new(&mut source, &namespace).unwrap();
    let original = Value(Kind::Instance(root.clone()));
    crate::objects::set(&mut source, &root, "link", &original).unwrap();
    crate::objects::set(
        &mut source,
        &root,
        "namespace",
        &Value(Kind::Namespace(namespace.clone())),
    )
    .unwrap();
    drop(root);
    crate::objects::finish(&mut source).unwrap();
    let mut receiver = CallContext::new(CallOptions::default());
    let copied = receiver.import(&original).unwrap();
    drop(original);
    drop(namespace);
    drop(script);
    assert_eq!(source.stats().retained_memory_bytes, 0);
    let Kind::Instance(instance) = &copied.0 else {
        panic!("expected instance")
    };
    let link = crate::objects::field(&mut receiver, instance, "link")
        .unwrap()
        .unwrap();
    assert!(matches!(&link.0, Kind::Instance(link) if instance.same(link)));
    drop(link);
    crate::objects::finish(&mut receiver).unwrap();
    assert!(code.upgrade().is_some());
    drop(copied);
    assert_eq!(receiver.stats().retained_memory_bytes, 0);
    assert!(code.upgrade().is_none());
    assert_eq!(retired.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_namespace_imports_release_partial_accounting_and_code_references() {
    let retired = Arc::new(AtomicUsize::new(0));
    let script = engine(&retired).compile(SOURCE).unwrap();
    let code = Arc::downgrade(&script.inner.code);
    let value = script
        .call("namespace", &[], CallOptions::default())
        .unwrap()
        .value;
    let mut sizing = CallContext::new(CallOptions::default());
    let imported = sizing.import(&value).unwrap();
    let bytes = sizing.stats().retained_memory_bytes;
    drop(imported);
    assert_eq!(sizing.stats().retained_memory_bytes, 0);
    let token = CancellationToken::new();
    token.cancel();
    for (options, expected) in [
        (
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(bytes - 1),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Memory,
        ),
        (
            CallOptions {
                cancellation: token,
                ..CallOptions::default()
            },
            ErrorKind::Cancelled,
        ),
    ] {
        let mut receiver = CallContext::new(options);
        assert_eq!(receiver.import(&value).unwrap_err().kind, expected);
        assert_eq!(receiver.stats().retained_memory_bytes, 0);
    }
    drop(value);
    drop(script);
    assert!(code.upgrade().is_none());
    assert_eq!(retired.load(Ordering::SeqCst), 1);
}

#[test]
fn metered_compilation_stops_at_limits_and_matches_unmetered_results() {
    let engine = crate::Engine::new();
    let mut source = String::new();
    for index in 0..300 {
        source.push_str(&format!(
            "def f{index}(x: int) -> int\n  x + {index}\nend\n"
        ));
    }
    source.push_str("f299(1)\n");
    let limits = |steps| CallOptions {
        limits: Limits {
            steps: Some(steps),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let error = engine
        .compile_with_options(&source, &limits(1_000))
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Steps);
    let script = engine
        .compile_with_options(&source, &limits(10_000_000))
        .unwrap();
    let value = script.run(CallOptions::default()).unwrap().value;
    assert_eq!(value.as_int(), Some(300));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let options = CallOptions {
        cancellation,
        ..CallOptions::default()
    };
    let error = engine
        .compile_with_options(&source, &options)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled);

    for broken in [
        "def f(\n  1\nend\n",
        "x = [1,\n",
        &" ".repeat(crate::syntax::MAX_SOURCE + 1),
    ] {
        let metered = engine
            .compile_with_options(broken, &CallOptions::default())
            .err()
            .unwrap();
        assert_eq!(metered, engine.compile(broken).err().unwrap());
    }
}
