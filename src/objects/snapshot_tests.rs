use super::*;
use crate::{CallOptions, CancellationToken, Engine, Limits};

fn graph(ctx: &mut CallContext, count: usize) -> Value {
    let mut nodes = Vec::new();
    for index in 0..count {
        let node = environment(ctx).unwrap();
        set(ctx, &node, "value", &Value::int(index as i64)).unwrap();
        nodes.push(node);
    }
    for index in 0..count {
        let next = Value(Kind::Instance(nodes[(index + 1) % count].clone()));
        let mut links = Hash::empty();
        links
            .insert(ctx, Value::bytes("next"), next.clone())
            .unwrap();
        links.insert(ctx, Value::bytes("alias"), next).unwrap();
        links.object = true;
        links.tag = crate::hash::Tag::Match;
        let links = Value::from_hash(ctx, links).unwrap();
        let links = ctx.array(&[links]).unwrap();
        set(ctx, &nodes[index], "links", &links).unwrap();
    }
    Value(Kind::Instance(nodes[0].clone()))
}

fn instance(value: &Value) -> &Arc<Instance> {
    let Kind::Instance(value) = &value.0 else {
        panic!("instance required");
    };
    value
}

fn next(ctx: &mut CallContext, node: &Arc<Instance>) -> Arc<Instance> {
    let links = field(ctx, node, "links").unwrap().unwrap();
    let Kind::Hash(links) = &links.as_array().unwrap()[0].0 else {
        panic!("hash required");
    };
    assert!(links.object);
    assert_eq!(links.tag, crate::hash::Tag::Match);
    let next = instance(&links.buffer.data[0].1);
    assert!(next.same(instance(&links.buffer.data[1].1)));
    next.clone()
}

#[test]
fn snapshots_copy_same_call_cycles_and_keep_their_memo_temporary() {
    let mut ctx = CallContext::new(CallOptions::default());
    let original = graph(&mut ctx, 40);
    let copied = ctx.snapshot(&original).unwrap();
    let another = ctx.snapshot(&original).unwrap();
    assert!(!instance(&copied).same(instance(&original)));
    assert!(!instance(&copied).same(instance(&another)));
    assert!(ctx.snapshot_objects.is_none());
    assert!(ctx.pending_objects.data.is_empty());
    assert!(!ctx.importing_objects);
    let mut cursor = instance(&copied).clone();
    for index in 0..40 {
        assert_eq!(
            field(&mut ctx, &cursor, "value").unwrap().unwrap().as_int(),
            Some(index)
        );
        cursor = next(&mut ctx, &cursor);
    }
    assert!(cursor.same(instance(&copied)));
    set(&mut ctx, instance(&original), "value", &Value::int(99)).unwrap();
    assert_eq!(
        field(&mut ctx, instance(&copied), "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(0)
    );
    assert!(instance(&ctx.import(&original).unwrap()).same(instance(&original)));
    drop((original, copied, another, cursor));
    finish(&mut ctx).unwrap();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn snapshots_preserve_shared_class_namespace_and_function_environments() {
    let mut ctx = CallContext::new(CallOptions::default());
    let environment = environment(&mut ctx).unwrap();
    let script = Engine::new().compile("class Box; end; nil").unwrap();
    let unbound = Namespace::import(
        &mut ctx,
        &Namespace::untracked(script.inner.code.program.namespaces[0].clone()),
    )
    .unwrap();
    let namespace = Namespace::with_environment(&mut ctx, &unbound, environment.clone()).unwrap();
    drop(unbound);
    let node = new(&mut ctx, &namespace).unwrap();
    let function =
        crate::exports::Function::new(&mut ctx, script.inner.code.clone(), environment.clone(), 0)
            .unwrap();
    set(
        &mut ctx,
        &environment,
        "node",
        &Value(Kind::Instance(node.clone())),
    )
    .unwrap();
    set(
        &mut ctx,
        &environment,
        "namespace",
        &Value(Kind::Namespace(namespace.clone())),
    )
    .unwrap();
    set(
        &mut ctx,
        &environment,
        "function",
        &Value(Kind::Function(function.clone())),
    )
    .unwrap();
    let input = ctx
        .array(&[
            Value(Kind::Instance(node)),
            Value(Kind::Namespace(namespace)),
            Value(Kind::Function(function)),
        ])
        .unwrap();
    let output = ctx.snapshot(&input).unwrap();
    let values = output.as_array().unwrap();
    let node = instance(&values[0]);
    let Kind::Namespace(namespace) = &values[1].0 else {
        panic!("namespace")
    };
    let Kind::Function(function) = &values[2].0 else {
        panic!("function")
    };
    let copied = namespace.environment.as_ref().unwrap();
    assert!(!copied.same(&environment));
    assert!(copied.same(&function.environment));
    assert!(copied.same(node.class().environment.as_ref().unwrap()));
    assert!(node.same(instance(&field(&mut ctx, copied, "node").unwrap().unwrap())));
    let Kind::Namespace(cycle) = field(&mut ctx, copied, "namespace").unwrap().unwrap().0 else {
        panic!("namespace")
    };
    assert!(cycle.same_binding(namespace));
    let Kind::Function(cycle_fn) = field(&mut ctx, copied, "function").unwrap().unwrap().0 else {
        panic!("function")
    };
    assert!(cycle_fn.same(function));
    drop((cycle, cycle_fn, input, output, environment));
    finish(&mut ctx).unwrap();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn a_snapshot_does_not_reuse_or_replace_foreign_import_memo_entries() {
    let mut source = CallContext::new(CallOptions::default());
    let original = graph(&mut source, 4);
    let mut ctx = CallContext::new(CallOptions::default());
    let ordinary = ctx.import(&original).unwrap();
    set(&mut ctx, instance(&ordinary), "value", &Value::int(99)).unwrap();
    let copied = ctx.snapshot(&original).unwrap();
    assert_eq!(
        field(&mut ctx, instance(&copied), "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(0)
    );
    assert!(instance(&ctx.import(&original).unwrap()).same(instance(&ordinary)));
    drop(original);
    finish(&mut source).unwrap();
    assert_eq!(source.stats().retained_memory_bytes, 0);
    drop((ordinary, copied));
    finish(&mut ctx).unwrap();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn snapshot_traversal_and_drop_support_the_full_container_depth() {
    let mut ctx = CallContext::new(CallOptions::default());
    let original = graph(&mut ctx, 2);
    let mut input = original.clone();
    for _ in 0..MAX_VALUE_DEPTH {
        input = Value::array(vec![input]);
    }
    let output = ctx.snapshot(&input).unwrap();
    let mut leaf = &output;
    for _ in 0..MAX_VALUE_DEPTH {
        leaf = &leaf.as_array().unwrap()[0];
    }
    assert!(!instance(leaf).same(instance(&original)));
    let child = next(&mut ctx, instance(leaf));
    assert!(next(&mut ctx, &child).same(instance(leaf)));
    drop(child);
    drop((original, input, output));
    finish(&mut ctx).unwrap();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn snapshot_limits_latch_and_release_partial_graphs_without_changing_sources() {
    let mut source = CallContext::new(CallOptions::default());
    let original = graph(&mut source, 8);
    let mut measure = CallContext::new(CallOptions::default());
    let copied = measure.snapshot(&original).unwrap();
    let stats = measure.stats();
    drop(copied);
    cleanup(&mut measure);
    assert_eq!(measure.stats().retained_memory_bytes, 0);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let mut cases = vec![(
        CallOptions {
            cancellation: cancelled,
            ..CallOptions::default()
        },
        ErrorKind::Cancelled,
    )];
    for steps in (0..stats.steps)
        .step_by((stats.steps / 16).max(1) as usize)
        .chain([stats.steps - 1])
    {
        cases.push((
            CallOptions {
                limits: Limits {
                    steps: Some(steps),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Steps,
        ));
    }
    for bytes in (0..stats.peak_memory_bytes)
        .step_by((stats.peak_memory_bytes / 16).max(1))
        .chain([stats.peak_memory_bytes - 1])
    {
        cases.push((
            CallOptions {
                limits: Limits {
                    memory_bytes: Some(bytes),
                    ..Limits::default()
                },
                ..CallOptions::default()
            },
            ErrorKind::Memory,
        ));
    }
    for (options, kind) in cases {
        let mut ctx = CallContext::new(options);
        assert_eq!(ctx.snapshot(&original).unwrap_err().kind, kind);
        assert!(ctx.snapshot_objects.is_none());
        assert!(ctx.pending_objects.data.is_empty());
        assert!(!ctx.importing_objects);
        assert_eq!(ctx.snapshot(&original).unwrap_err().kind, kind);
        cleanup(&mut ctx);
        assert_eq!(ctx.stats().retained_memory_bytes, 0, "{kind:?}");
    }
    let mut exact = CallContext::new(CallOptions {
        limits: Limits {
            steps: Some(stats.steps),
            memory_bytes: Some(stats.peak_memory_bytes),
            ..Limits::default()
        },
        ..CallOptions::default()
    });
    let copy = exact.snapshot(&original).unwrap();
    assert_eq!(exact.stats().steps, stats.steps);
    drop(copy);
    cleanup(&mut exact);
    assert_eq!(exact.stats().retained_memory_bytes, 0);
    assert_eq!(
        field(&mut source, instance(&original), "value")
            .unwrap()
            .unwrap()
            .as_int(),
        Some(0)
    );
}
