//! An instance's heap slot and a field's position are vibescript's own
//! bookkeeping, so one out of range is an internal error, not a panic. The
//! tests corrupt that bookkeeping directly, since no script can reach it.

use super::*;
use crate::{CallOptions, Engine, namespace::Definition};

fn class() -> Arc<Namespace> {
    Namespace::untracked(Definition::new(
        0,
        "Node".into(),
        Vec::new(),
        Vec::new(),
        Some((0, false)),
        Vec::new(),
        None,
    ))
}

fn context() -> CallContext {
    CallContext::new(CallOptions::default())
}

fn assert_internal<T>(result: Result<T>) {
    let error = result.err().expect("the access must fail");
    assert_eq!(error.kind, ErrorKind::Internal, "{error}");
    assert!(error.message.contains("out of range"), "{error}");
}

/// An instance of a class that declares `@a`, imported into a new context.
fn declared() -> (CallContext, Arc<Instance>) {
    let script = Engine::new()
        .compile("class Row\n  @a: int = 1\nend\ndef run -> Row\n  Row.new\nend\n")
        .unwrap();
    let result = script.call("run", &[], CallOptions::default()).unwrap();
    let mut ctx = context();
    let value = ctx.import(&result.value).unwrap();
    let Kind::Instance(instance) = &value.0 else {
        panic!("instance")
    };
    let instance = instance.clone();
    (ctx, instance)
}

#[test]
fn an_instance_slot_out_of_range_is_an_internal_error() {
    let mut ctx = context();
    let instance = new(&mut ctx, &class()).unwrap();
    let slot = instance.identity.slot.swap(usize::MAX, Ordering::Relaxed);
    assert_internal(field(&mut ctx, &instance, "x"));
    assert_internal(get_slot(&mut ctx, &instance, 0));
    assert_internal(address_slot(&mut ctx, &instance, 0));
    assert_internal(with_field_names(&instance, |_| ()));
    assert_internal(children(&mut ctx, &instance, &mut Buffer::empty()));
    assert_internal(bindings(&mut ctx, &instance));
    assert_internal(set(&mut ctx, &instance, "x", &Value::int(1)));
    assert_internal(address(&mut ctx, &instance, "x"));
    assert_internal(field_name(&instance, 0));
    assert_internal(field_slot(&mut ctx, &instance, "x"));
    assert_internal(set_slot(&mut ctx, &instance, 0, Value::int(1)));
    instance.identity.slot.store(slot, Ordering::Relaxed);
    assert_eq!(field_slot(&mut ctx, &instance, "x").unwrap(), None);
}

#[test]
fn storing_a_reference_with_a_slot_out_of_range_is_an_internal_error() {
    let mut ctx = context();
    let class = class();
    let holder = new(&mut ctx, &class).unwrap();
    let held = new(&mut ctx, &class).unwrap();
    let slot = held.identity.slot.swap(usize::MAX, Ordering::Relaxed);
    let value = Value(Kind::Instance(held.clone()));
    assert_internal(set(&mut ctx, &holder, "link", &value));
    held.identity.slot.store(slot, Ordering::Relaxed);
}

#[test]
fn collecting_past_a_slot_out_of_range_is_an_internal_error() {
    let mut ctx = context();
    let class = class();
    let holder = new(&mut ctx, &class).unwrap();
    let held = new(&mut ctx, &class).unwrap();
    set(
        &mut ctx,
        &holder,
        "link",
        &Value(Kind::Instance(held.clone())),
    )
    .unwrap();
    let identity = held.identity.clone();
    drop(held);
    let slot = identity.slot.swap(usize::MAX, Ordering::Relaxed);
    let heap = local(&mut ctx).unwrap();
    assert_internal(collect(&mut ctx, &heap, false));
    // Cleanup after a failure carries on without the reclaim.
    cleanup(&mut ctx);
    identity.slot.store(slot, Ordering::Relaxed);
}

#[test]
fn a_mapped_position_out_of_range_is_an_internal_error() {
    let mut ctx = context();
    let array = Value::from_array(&mut ctx, Buffer::empty()).unwrap();
    let mut frame = MapFrame::new(&array);
    assert_internal(frame.assign(&mut ctx, 0, Value::nil()));
    let hash = Value::from_hash(&mut ctx, Hash::empty()).unwrap();
    let mut frame = MapFrame::new(&hash);
    assert_internal(frame.assign(&mut ctx, 0, Value::nil()));
}

#[test]
fn a_named_field_out_of_range_is_an_internal_error() {
    let mut ctx = context();
    let instance = new(&mut ctx, &class()).unwrap();
    assert_internal(field_name(&instance, 0));
    assert_internal(set_slot(&mut ctx, &instance, 0, Value::int(1)));
}

#[test]
fn a_slot_past_the_class_layout_is_an_internal_error_with_a_name_to_report() {
    let (mut ctx, instance) = declared();
    assert_eq!(instance.class().field_layout().unwrap().len(), 1);
    assert!(matches!(
        field_name(&instance, 0).unwrap().as_bytes(),
        Some(b"a")
    ));
    assert_internal(field_name(&instance, 3));
    assert!(
        FieldName::Declared(instance.class().clone(), 3)
            .as_bytes()
            .is_none()
    );
    set_slot(&mut ctx, &instance, 3, Value::int(2)).unwrap();
    assert_internal(bindings(&mut ctx, &instance));
    // A lookup-failure suggestion lists the names it can and leaves out the rest.
    let names = with_field_names(&instance, |names| {
        names.map(<[u8]>::to_vec).collect::<Vec<_>>()
    })
    .unwrap();
    assert_eq!(names, [b"a".to_vec()]);
}

#[test]
fn a_host_callback_cannot_swallow_an_internal_error_from_an_import() {
    let mut source = context();
    let stale = new(&mut source, &class()).unwrap();
    let slot = stale.identity.slot.swap(usize::MAX, Ordering::Relaxed);
    let value = Value(Kind::Instance(stale.clone()));
    let mut engine = Engine::new();
    engine.register("probe", move |ctx, _| {
        assert_internal(ctx.import(&value));
        assert_internal(ctx.checkpoint());
        Ok(Value::nil())
    });
    let script = engine
        .compile("def run -> int\nbegin\nprobe()\n1\nrescue\n2\nend\nend")
        .unwrap();
    assert_internal(script.call("run", &[], CallOptions::default()));
    stale.identity.slot.store(slot, Ordering::Relaxed);
}
