use super::*;
use crate::{CallOptions, CancellationToken, Limits, namespace::Definition};

fn template(name: &str) -> Arc<Namespace> {
    Namespace::untracked(Definition::new(
        0,
        name.into(),
        Vec::new(),
        Vec::new(),
        Some((0, false)),
        Vec::new(),
        None,
    ))
}

fn bind(
    ctx: &mut CallContext,
    template: &Arc<Namespace>,
    environment: &Arc<Instance>,
) -> Arc<Namespace> {
    let namespace = Namespace::import(ctx, template).unwrap();
    Namespace::with_environment(ctx, &namespace, environment.clone()).unwrap()
}

fn namespace(value: &Value) -> &Arc<Namespace> {
    let Kind::Namespace(namespace) = &value.0 else {
        panic!("expected namespace");
    };
    namespace
}

#[test]
fn captured_namespaces_keep_their_environment_and_reclaim_unrelated_objects() {
    let mut ctx = CallContext::new(CallOptions::default());
    let environment = new(&mut ctx, &template("Scope")).unwrap();
    let captured = bind(&mut ctx, &template("Captured"), &environment);
    set(&mut ctx, &environment, "value", &Value::int(7)).unwrap();
    set(
        &mut ctx,
        &environment,
        "cycle",
        &Value(Kind::Namespace(captured.clone())),
    )
    .unwrap();
    let unrelated = new(&mut ctx, &template("Discarded")).unwrap();
    set(
        &mut ctx,
        &unrelated,
        "bytes",
        &Value::bytes(vec![b'x'; 1 << 20]),
    )
    .unwrap();
    drop(unrelated);
    drop(environment);
    finish(&mut ctx).unwrap();
    assert!(ctx.stats().retained_memory_bytes < 32768);
    let environment = captured.environment.as_ref().unwrap();
    assert_eq!(
        field(&mut ctx, environment, "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(7)
    );
    let cycle = field(&mut ctx, environment, "cycle").unwrap().unwrap();
    assert!(namespace(&cycle).same_binding(&captured));
    drop(cycle);
    drop(captured);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn importing_a_namespace_isolates_captured_state_and_preserves_cycles() {
    let mut source = CallContext::new(CallOptions::default());
    let environment = new(&mut source, &template("Scope")).unwrap();
    let captured = bind(&mut source, &template("Captured"), &environment);
    set(&mut source, &environment, "value", &Value::int(7)).unwrap();
    set(
        &mut source,
        &environment,
        "class",
        &Value(Kind::Namespace(captured.clone())),
    )
    .unwrap();
    drop(environment);
    finish(&mut source).unwrap();
    let incoming = Value(Kind::Namespace(captured));
    let mut target = CallContext::new(CallOptions::default());
    let copied = target.import(&incoming).unwrap();
    let environment = namespace(&copied).environment.as_ref().unwrap();
    let class = field(&mut target, environment, "class").unwrap().unwrap();
    assert!(namespace(&copied).same_binding(namespace(&class)));
    assert!(!namespace(&copied).same_binding(namespace(&incoming)));
    set(&mut target, environment, "value", &Value::int(8)).unwrap();
    assert_eq!(
        field(
            &mut source,
            namespace(&incoming).environment.as_ref().unwrap(),
            "value"
        )
        .unwrap()
        .unwrap()
        .as_int(),
        Some(7)
    );
    assert_eq!(
        field(&mut target, environment, "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(8)
    );
    drop(class);
    drop(incoming);
    assert_eq!(source.stats().retained_memory_bytes, 0);
    finish(&mut target).unwrap();
    drop(copied);
    assert_eq!(target.stats().retained_memory_bytes, 0);
}

#[test]
fn importing_an_instance_preserves_a_cycle_through_its_class_environment() {
    let mut source = CallContext::new(CallOptions::default());
    let environment = new(&mut source, &template("Scope")).unwrap();
    let captured = bind(&mut source, &template("Captured"), &environment);
    let instance = new(&mut source, &captured).unwrap();
    set(
        &mut source,
        &environment,
        "instance",
        &Value(Kind::Instance(instance.clone())),
    )
    .unwrap();
    set(
        &mut source,
        &environment,
        "class",
        &Value(Kind::Namespace(captured.clone())),
    )
    .unwrap();
    set(
        &mut source,
        &instance,
        "self",
        &Value(Kind::Instance(instance.clone())),
    )
    .unwrap();
    drop(captured);
    drop(environment);
    finish(&mut source).unwrap();
    let mut target = CallContext::new(CallOptions::default());
    let copied = import(&mut target, &instance).unwrap();
    let class = Namespace::import(&mut target, copied.class()).unwrap();
    let environment = class.environment.as_ref().unwrap();
    let cycle = field(&mut target, environment, "instance")
        .unwrap()
        .unwrap();
    assert!(matches!(&cycle.0, Kind::Instance(node) if copied.same(node)));
    let again = field(&mut target, &copied, "self").unwrap().unwrap();
    assert!(matches!(&again.0, Kind::Instance(node) if copied.same(node)));
    let declared = field(&mut target, environment, "class").unwrap().unwrap();
    assert!(namespace(&declared).same_binding(&class));
    drop(instance);
    assert_eq!(source.stats().retained_memory_bytes, 0);
    drop(cycle);
    drop(again);
    drop(declared);
    drop(class);
    finish(&mut target).unwrap();
    drop(copied);
    assert_eq!(target.stats().retained_memory_bytes, 0);
}

#[test]
fn class_metadata_is_shared_within_each_environment_and_keeps_bindings_distinct() {
    let mut source = CallContext::new(CallOptions::default());
    let first = new(&mut source, &template("Scope")).unwrap();
    let second = new(&mut source, &template("Scope")).unwrap();
    let definition = template("Captured");
    let left = bind(&mut source, &definition, &first);
    let right = bind(&mut source, &definition, &second);
    assert!(!left.same_binding(&right));
    let mut target = CallContext::new(CallOptions::default());
    let mut instances = Buffer::empty();
    for class in [&left, &left, &right, &left, &right] {
        let instance = new(&mut target, class).unwrap();
        instances.push(&mut target, instance).unwrap();
    }
    assert!(Arc::ptr_eq(
        instances.data[0].class(),
        instances.data[1].class()
    ));
    assert!(Arc::ptr_eq(
        instances.data[0].class(),
        instances.data[3].class()
    ));
    assert!(Arc::ptr_eq(
        instances.data[2].class(),
        instances.data[4].class()
    ));
    assert!(
        !instances.data[0]
            .class()
            .same_binding(instances.data[2].class())
    );
    drop(instances);
    finish(&mut target).unwrap();
    assert_eq!(target.stats().retained_memory_bytes, 0);
    drop(left);
    drop(right);
    drop(first);
    drop(second);
    finish(&mut source).unwrap();
    assert_eq!(source.stats().retained_memory_bytes, 0);
}

#[test]
fn collected_unreachable_namespace_cycles_release_environment_and_metadata() {
    let mut ctx = CallContext::new(CallOptions::default());
    let keep = new(&mut ctx, &template("Keep")).unwrap();
    let baseline = ctx.stats().retained_memory_bytes;
    for _ in 0..96 {
        let environment = new(&mut ctx, &template("Scope")).unwrap();
        let captured = bind(&mut ctx, &template("Captured"), &environment);
        let instance = new(&mut ctx, &captured).unwrap();
        set(
            &mut ctx,
            &environment,
            "class",
            &Value(Kind::Namespace(captured)),
        )
        .unwrap();
        set(
            &mut ctx,
            &environment,
            "instance",
            &Value(Kind::Instance(instance)),
        )
        .unwrap();
    }
    finish(&mut ctx).unwrap();
    assert!(ctx.stats().retained_memory_bytes <= baseline);
    drop(keep);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn captured_environment_imports_observe_limits_and_cleanup_failed_graphs() {
    let mut source = CallContext::new(CallOptions::default());
    let environment = new(&mut source, &template("Scope")).unwrap();
    let captured = bind(&mut source, &template("Captured"), &environment);
    set(
        &mut source,
        &environment,
        "payload",
        &Value::bytes(vec![b'x'; 1 << 20]),
    )
    .unwrap();
    set(
        &mut source,
        &environment,
        "self",
        &Value(Kind::Namespace(captured.clone())),
    )
    .unwrap();
    let incoming = Value(Kind::Namespace(captured));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    for (options, kind) in [
        (
            CallOptions {
                cancellation,
                ..CallOptions::default()
            },
            ErrorKind::Cancelled,
        ),
        (
            CallOptions {
                limits: Limits {
                    steps: Some(4),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Steps,
        ),
        (
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(32768),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Memory,
        ),
    ] {
        let mut target = CallContext::new(options);
        assert_eq!(target.import(&incoming).unwrap_err().kind, kind);
        assert!(target.exhausted());
        assert!(!target.importing_objects);
        cleanup(&mut target);
        assert_eq!(target.stats().retained_memory_bytes, 0);
    }
    drop(incoming);
    drop(environment);
    finish(&mut source).unwrap();
    assert_eq!(source.stats().retained_memory_bytes, 0);
}

#[test]
fn promoting_nested_namespaces_keeps_stable_retained_storage() {
    let mut ctx = CallContext::new(CallOptions::default());
    let environment = new(&mut ctx, &template("Scope")).unwrap();
    let captured = bind(&mut ctx, &template("Captured"), &environment);
    let kept = new(&mut ctx, &template("Holder")).unwrap();
    let values = Value::array(vec![Value::hash(vec![(
        b"class".to_vec(),
        Value(Kind::Namespace(captured.clone())),
    )])]);
    set(&mut ctx, &kept, "nested", &values).unwrap();
    drop(values);
    drop(captured);
    drop(environment);
    let baseline = ctx.stats().retained_memory_bytes;
    for _ in 0..1000 {
        let value = field(&mut ctx, &kept, "nested").unwrap().unwrap();
        let nested = &value.as_array().unwrap()[0];
        let Kind::Hash(hash) = &nested.0 else {
            panic!("expected hash");
        };
        assert!(namespace(&hash.buffer.data[0].1).environment.is_some());
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
    }
    drop(kept);
    finish(&mut ctx).unwrap();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn concurrent_imports_copy_captured_state_independently() {
    let mut source = CallContext::new(CallOptions::default());
    let environment = new(&mut source, &template("Scope")).unwrap();
    set(&mut source, &environment, "value", &Value::int(7)).unwrap();
    let captured = Value(Kind::Namespace(bind(
        &mut source,
        &template("Captured"),
        &environment,
    )));
    drop(environment);
    finish(&mut source).unwrap();
    std::thread::scope(|threads| {
        for index in 0..8 {
            let captured = &captured;
            threads.spawn(move || {
                let mut ctx = CallContext::new(CallOptions::default());
                let copied = ctx.import(captured).unwrap();
                let environment = namespace(&copied).environment.as_ref().unwrap();
                assert_eq!(
                    field(&mut ctx, environment, "value")
                        .unwrap()
                        .unwrap()
                        .as_int(),
                    Some(7)
                );
                set(&mut ctx, environment, "value", &Value::int(index)).unwrap();
                assert_eq!(
                    field(&mut ctx, environment, "value")
                        .unwrap()
                        .unwrap()
                        .as_int(),
                    Some(index)
                );
                drop(copied);
                finish(&mut ctx).unwrap();
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            });
        }
    });
    assert_eq!(
        field(
            &mut source,
            namespace(&captured).environment.as_ref().unwrap(),
            "value"
        )
        .unwrap()
        .unwrap()
        .as_int(),
        Some(7)
    );
    drop(captured);
    assert_eq!(source.stats().retained_memory_bytes, 0);
}

#[test]
fn deeply_nested_class_environments_fail_with_a_recoverable_depth_error() {
    let mut source = CallContext::new(CallOptions::default());
    let mut environment = new(&mut source, &template("Scope")).unwrap();
    let definition = template("Captured");
    for _ in 0..MAX_VALUE_DEPTH + 8 {
        let class = bind(&mut source, &definition, &environment);
        environment = new(&mut source, &class).unwrap();
    }
    let mut target = CallContext::new(CallOptions::default());
    let error = import(&mut target, &environment).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    assert!(
        error
            .message
            .contains("namespace environment nesting too deep")
    );
    assert!(!target.exhausted());
    assert!(!target.importing_objects);
    assert_eq!(target.namespace_depth, 0);
    cleanup(&mut target);
    assert_eq!(target.stats().retained_memory_bytes, 0);
    let small = new(&mut target, &template("Small")).unwrap();
    set(&mut target, &small, "value", &Value::int(1)).unwrap();
    drop(small);
    finish(&mut target).unwrap();
    assert_eq!(target.stats().retained_memory_bytes, 0);
    drop(environment);
    finish(&mut source).unwrap();
    assert_eq!(source.stats().retained_memory_bytes, 0);
}
