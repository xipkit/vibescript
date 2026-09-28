use super::*;
use crate::{CallOptions, CancellationToken, Limits, namespace::Definition};

fn class(name: &str) -> Arc<Namespace> {
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

fn graph(count: usize) -> (CallContext, Arc<Instance>) {
    let mut source = CallContext::new(CallOptions::default());
    let class = class("Node");
    let mut nodes = Vec::new();
    for index in 0..count {
        let node = new(&mut source, &class).unwrap();
        set(&mut source, &node, "value", &Value::int(index as i64)).unwrap();
        nodes.push(node);
    }
    for index in 0..count {
        let next = Value(Kind::Instance(nodes[(index + 1) % count].clone()));
        let mut hash = Hash::empty();
        hash.insert(&mut source, Value::bytes("next"), next.clone())
            .unwrap();
        hash.insert(&mut source, Value::bytes("again"), next)
            .unwrap();
        hash.object = true;
        hash.tag = crate::hash::Tag::Match;
        let hash = Value::from_hash(&mut source, hash).unwrap();
        set(
            &mut source,
            &nodes[index],
            "links",
            &Value::array(vec![hash]),
        )
        .unwrap();
    }
    let root = nodes.swap_remove(0);
    drop(nodes);
    finish(&mut source).unwrap();
    (source, root)
}

fn next(ctx: &mut CallContext, node: &Arc<Instance>) -> Arc<Instance> {
    let links = field(ctx, node, "links").unwrap().unwrap();
    let Kind::Hash(hash) = &links.as_array().unwrap()[0].0 else {
        panic!("hash")
    };
    assert!(hash.object);
    assert_eq!(hash.tag, crate::hash::Tag::Match);
    let Kind::Instance(first) = &hash.buffer.data[0].1.0 else {
        panic!("instance")
    };
    let Kind::Instance(again) = &hash.buffer.data[1].1.0 else {
        panic!("instance")
    };
    assert!(first.same(again));
    first.clone()
}

#[test]
fn nested_container_imports_preserve_graphs_tags_and_release_source_storage() {
    let (source, original) = graph(40);
    let mut target = CallContext::new(CallOptions::default());
    let copied = import(&mut target, &original).unwrap();
    assert!(!copied.same(&original));
    let mut cursor = copied.clone();
    for index in 0..40 {
        assert_eq!(
            field(&mut target, &cursor, "value")
                .unwrap()
                .unwrap()
                .as_int(),
            Some(index)
        );
        cursor = next(&mut target, &cursor);
    }
    assert!(cursor.same(&copied));
    set(&mut target, &copied, "value", &Value::int(99)).unwrap();
    let again = import(&mut target, &original).unwrap();
    assert!(again.same(&copied));
    assert_eq!(
        field(&mut target, &again, "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(99)
    );
    drop(original);
    assert_eq!(source.stats().retained_memory_bytes, 0);
    drop((cursor, again));
    finish(&mut target).unwrap();
    drop(copied);
    assert_eq!(target.stats().retained_memory_bytes, 0);
}

#[test]
fn nested_captured_namespaces_import_their_environment_once() {
    let mut source = CallContext::new(CallOptions::default());
    let environment = environment(&mut source).unwrap();
    let original_class = Namespace::import(&mut source, &class("Counter")).unwrap();
    let captured =
        Namespace::with_environment(&mut source, &original_class, environment.clone()).unwrap();
    drop(original_class);
    set(&mut source, &environment, "value", &Value::int(5)).unwrap();
    set(
        &mut source,
        &environment,
        "self",
        &Value(Kind::Namespace(captured.clone())),
    )
    .unwrap();
    let root = new(&mut source, &class("Holder")).unwrap();
    let value = Value(Kind::Namespace(captured.clone()));
    set(
        &mut source,
        &root,
        "classes",
        &Value::array(vec![Value::hash(vec![
            (b"first".to_vec(), value.clone()),
            (b"again".to_vec(), value),
        ])]),
    )
    .unwrap();
    drop((captured, environment));
    finish(&mut source).unwrap();
    let mut target = CallContext::new(CallOptions::default());
    let copied = import(&mut target, &root).unwrap();
    let classes = field(&mut target, &copied, "classes").unwrap().unwrap();
    let Kind::Hash(hash) = &classes.as_array().unwrap()[0].0 else {
        panic!("hash")
    };
    let Kind::Namespace(first) = &hash.buffer.data[0].1.0 else {
        panic!("namespace")
    };
    let Kind::Namespace(again) = &hash.buffer.data[1].1.0 else {
        panic!("namespace")
    };
    assert!(first.same_binding(again));
    let environment = first.environment.as_ref().unwrap();
    let cycle = field(&mut target, environment, "self").unwrap().unwrap();
    assert!(matches!(&cycle.0, Kind::Namespace(value) if value.same_binding(first)));
    set(&mut target, environment, "value", &Value::int(6)).unwrap();
    assert_eq!(
        field(&mut target, again.environment.as_ref().unwrap(), "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(6)
    );
    drop(root);
    assert_eq!(source.stats().retained_memory_bytes, 0);
    drop((cycle, classes));
    finish(&mut target).unwrap();
    drop(copied);
    assert_eq!(target.stats().retained_memory_bytes, 0);
}

#[test]
fn field_snapshots_keep_source_references_alive_until_import_finishes() {
    let (mut source, original) = graph(2);
    let mut target = CallContext::new(CallOptions::default());
    let fields = bindings(&mut target, &original).unwrap();
    set(&mut source, &original, "links", &Value::nil()).unwrap();
    collect(&mut source, &original.heap().unwrap(), false).unwrap();
    let links = &fields
        .data
        .iter()
        .find(|(key, _)| key.as_bytes() == Some(b"links"))
        .unwrap()
        .1;
    let copied = target.import_rooted(links).unwrap();
    let Kind::Hash(hash) = &copied.as_array().unwrap()[0].0 else {
        panic!("hash")
    };
    let Kind::Instance(child) = &hash.buffer.data[0].1.0 else {
        panic!("instance")
    };
    let Kind::Instance(again) = &hash.buffer.data[1].1.0 else {
        panic!("instance")
    };
    assert!(child.same(again));
    assert_eq!(
        field(&mut target, child, "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(1)
    );
    let parent = next(&mut target, child);
    assert_eq!(
        field(&mut target, &parent, "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(0)
    );
    assert!(matches!(
        field(&mut target, &parent, "links").unwrap().unwrap().0,
        Kind::Nil
    ));
    drop((original, fields));
    assert_eq!(source.stats().retained_memory_bytes, 0);
    drop(parent);
    finish(&mut target).unwrap();
    drop(copied);
    assert_eq!(target.stats().retained_memory_bytes, 0);
}

#[test]
fn deepest_fields_keep_cycles_alive_across_import_and_exhausted_cleanup() {
    crate::ops::testing::on_small_stack(|| {
        let mut source = CallContext::new(CallOptions::default());
        let root = new(&mut source, &class("Node")).unwrap();
        let mut value = Value(Kind::Instance(root.clone()));
        for _ in 0..MAX_VALUE_DEPTH {
            value = Value::array(vec![value]);
        }
        set(&mut source, &root, "links", &value).unwrap();
        drop(value);
        finish(&mut source).unwrap();

        let mut target = CallContext::new(CallOptions::default());
        let copied = import(&mut target, &root).unwrap();
        let links = field(&mut target, &copied, "links").unwrap().unwrap();
        let mut leaf = &links;
        for _ in 0..MAX_VALUE_DEPTH {
            leaf = &leaf.as_array().unwrap()[0];
        }
        let Kind::Instance(back) = &leaf.0 else {
            panic!("missing instance")
        };
        assert!(back.same(&copied));
        drop(links);
        assert_eq!(target.charge(u64::MAX).unwrap_err().kind, ErrorKind::Steps);
        cleanup(&mut target);
        drop(copied);
        assert_eq!(target.stats().retained_memory_bytes, 0);
        drop(root);
        assert_eq!(source.stats().retained_memory_bytes, 0);
    });
}

#[test]
fn failure_cleanup_releases_workspace_for_discarded_deep_fields() {
    let mut ctx = CallContext::new(CallOptions::default());
    let class = class("Node");
    let kept = new(&mut ctx, &class).unwrap();
    let discarded = new(&mut ctx, &class).unwrap();
    let mut value = Value::int(7);
    for _ in 0..MAX_VALUE_DEPTH - 1 {
        value = Value::array(vec![value]);
    }
    set(&mut ctx, &discarded, "data", &value).unwrap();
    drop((value, discarded));
    assert_eq!(ctx.charge(u64::MAX).unwrap_err().kind, ErrorKind::Steps);
    cleanup(&mut ctx);
    assert!(
        ctx.stats().retained_memory_bytes < 8192,
        "{:?}",
        ctx.stats()
    );
    assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    drop(kept);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn nested_container_imports_obey_exact_limits_and_clean_interrupted_graphs() {
    let (mut source, original) = graph(8);
    let measure = || {
        let mut target = CallContext::new(CallOptions::default());
        let copied = import(&mut target, &original).unwrap();
        let stats = target.stats();
        drop(copied);
        cleanup(&mut target);
        assert_eq!(target.stats().retained_memory_bytes, 0);
        stats
    };
    let measured = measure();
    let repeated = measure();
    assert_eq!(measured.steps, repeated.steps);
    assert_eq!(measured.peak_memory_bytes, repeated.peak_memory_bytes);
    let mut exact = CallContext::new(CallOptions {
        limits: Limits {
            steps: Some(measured.steps),
            memory_bytes: Some(measured.peak_memory_bytes),
            ..Default::default()
        },
        ..Default::default()
    });
    let copied = import(&mut exact, &original).unwrap();
    drop(copied);
    cleanup(&mut exact);
    assert_eq!(exact.stats().retained_memory_bytes, 0);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let mut failures = vec![
        (
            CallOptions {
                cancellation: cancelled,
                ..Default::default()
            },
            ErrorKind::Cancelled,
        ),
        (
            CallOptions {
                limits: Limits {
                    steps: Some(measured.steps - 1),
                    ..Default::default()
                },
                ..Default::default()
            },
            ErrorKind::Steps,
        ),
        (
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(measured.peak_memory_bytes - 1),
                    ..Default::default()
                },
                ..Default::default()
            },
            ErrorKind::Memory,
        ),
    ];
    for steps in (0..measured.steps).step_by((measured.steps / 16).max(1) as usize) {
        failures.push((
            CallOptions {
                limits: Limits {
                    steps: Some(steps),
                    ..Default::default()
                },
                ..Default::default()
            },
            ErrorKind::Steps,
        ));
    }
    for bytes in (0..measured.peak_memory_bytes).step_by((measured.peak_memory_bytes / 16).max(1)) {
        failures.push((
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(bytes),
                    ..Default::default()
                },
                ..Default::default()
            },
            ErrorKind::Memory,
        ));
    }
    for (options, kind) in failures {
        let mut target = CallContext::new(options);
        assert_eq!(import(&mut target, &original).unwrap_err().kind, kind);
        assert!(!target.importing_objects);
        assert!(target.pending_objects.data.is_empty());
        cleanup(&mut target);
        assert_eq!(target.stats().retained_memory_bytes, 0);
        assert_eq!(target.checkpoint().unwrap_err().kind, kind);
    }
    assert_eq!(
        field(&mut source, &original, "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(0)
    );
    drop(original);
    assert_eq!(source.stats().retained_memory_bytes, 0);
}
