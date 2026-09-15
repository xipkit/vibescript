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
    def self.value; host(); end
  end
end
class Node
  property link
  def value; host(); end
end
def namespace; Outer; end
def nested; Outer.Inner; end
def class_value; Node; end
def cycle
  node = Node.new
  node.link = node
  node
end
def host_value; host(); end
"#;

struct Retired(Arc<AtomicUsize>);

impl Drop for Retired {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn engine(retired: &Arc<AtomicUsize>) -> Engine {
    let guard = Retired(retired.clone());
    let mut engine = Engine::new();
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
