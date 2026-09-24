use super::*;
use crate::{CallOptions, ErrorClass, hash::Tag};
use std::{thread, time::Instant};

/// Native stack for the deep cases: far below what depth-proportional glue would need.
const STACK: usize = 256 << 10;
const DEEP: usize = 10_001;

fn on_small_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    // WASI has no threads, so this runs within the default wasm stack.
    if cfg!(target_os = "wasi") {
        return work();
    }
    thread::Builder::new()
        .stack_size(STACK)
        .spawn(work)
        .unwrap()
        .join()
        .unwrap()
}

fn context() -> CallContext {
    CallContext::new(CallOptions::default())
}

fn array_chain(levels: usize) -> Value {
    let mut value = Value::int(0);
    for _ in 0..levels {
        value = Value::array(vec![value]);
    }
    value
}

fn hash_chain(levels: usize) -> Value {
    let mut value = Value::int(0);
    for _ in 0..levels {
        value = Value::hash(vec![(b"k".to_vec(), value)]);
    }
    value
}

/// Alternates array and hash levels; the outermost level is an array when `levels` is odd.
fn mixed_chain(levels: usize) -> Value {
    let mut value = Value::int(0);
    for level in 0..levels {
        value = if level % 2 == 0 {
            Value::array(vec![value])
        } else {
            Value::hash(vec![(b"k".to_vec(), value)])
        };
    }
    value
}

fn array_ptr(value: &Value) -> *const Value {
    value.as_array().unwrap().as_ptr()
}

#[test]
fn deep_host_values_drop_on_a_small_stack() {
    on_small_stack(|| {
        assert_eq!(array_chain(DEEP).depth(), DEEP);
        assert_eq!(hash_chain(DEEP).depth(), DEEP);
        drop(mixed_chain(DEEP));
        // A partially consumed scratch vector holding deep siblings.
        let mut pending = vec![array_chain(DEEP), hash_chain(DEEP), mixed_chain(DEEP)];
        pending.pop();
        drop(pending);
        // A deep pair dropped through the hash entry path.
        let mut entries = vec![(Value::bytes("k"), mixed_chain(DEEP))];
        entries.push((Value::bytes("j"), array_chain(DEEP)));
        drop(entries);
    });
}

#[test]
fn shared_branches_are_unlinked_once_by_their_last_owner() {
    on_small_stack(|| {
        let shared = array_chain(DEEP);
        let mut upper = shared.clone();
        for _ in 0..DEEP {
            upper = Value::hash(vec![(b"k".to_vec(), upper)]);
        }
        let other = Value::array(vec![shared.clone(), shared]);
        drop(upper);
        let inner = &other.as_array().unwrap()[0];
        assert_eq!(inner.depth(), DEEP);
        assert_eq!(inner.as_array().unwrap().len(), 1);
        drop(other);

        // A directed acyclic graph with 2^40 paths but only 41 nodes.
        let mut ladder = Value::int(0);
        for _ in 0..40 {
            ladder = Value::array(vec![ladder.clone(), ladder]);
        }
        assert_eq!(ladder.depth(), 40);
        drop(ladder);
    });
}

#[test]
#[cfg_attr(target_os = "wasi", ignore = "WASI has no threads")]
fn concurrent_last_owners_release_deep_subtrees() {
    for _ in 0..8 {
        let value = mixed_chain(DEEP);
        let mut owners: [Vec<Value>; 4] = std::array::from_fn(|_| Vec::new());
        let mut node = &value;
        for depth in 0..DEEP {
            owners[depth % 4].push(node.clone());
            node = match &node.0 {
                Kind::Array(array) => &array.buffer.data[0],
                Kind::Hash(hash) => &hash.buffer.data[0].1,
                _ => unreachable!(),
            };
        }
        drop(value);
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = owners
            .into_iter()
            .map(|values| {
                let barrier = barrier.clone();
                thread::Builder::new()
                    .stack_size(STACK)
                    .spawn(move || {
                        barrier.wait();
                        drop(values);
                    })
                    .unwrap()
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    }
}

#[test]
fn host_container_cleanup_tolerates_internal_weak_observers() {
    on_small_stack(|| {
        let child = array_chain(DEEP);
        let Kind::Array(array) = &child.0 else {
            unreachable!()
        };
        let weak = Arc::downgrade(array);
        drop(Value::array(vec![child]));
        assert!(weak.upgrade().is_none());

        let child = hash_chain(DEEP);
        let Kind::Hash(hash) = &child.0 else {
            unreachable!()
        };
        let weak = Arc::downgrade(hash);
        drop(Value::hash(vec![(b"k".to_vec(), child)]));
        assert!(weak.upgrade().is_none());
    });
}

#[test]
fn charged_values_release_storage_when_dropped_elsewhere_or_after_exhaustion() {
    let mut ctx = context();
    let charged = ctx.import(&mixed_chain(MAX_VALUE_DEPTH)).unwrap();
    assert_eq!(charged.depth(), MAX_VALUE_DEPTH);
    assert!(ctx.stats().retained_memory_bytes > 0);
    on_small_stack(move || drop(charged));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);

    let charged = ctx.import(&hash_chain(MAX_VALUE_DEPTH)).unwrap();
    assert_eq!(ctx.charge(u64::MAX).unwrap_err().kind, ErrorKind::Steps);
    drop(charged);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
}

#[test]
fn partially_built_buffers_release_deep_elements() {
    let mut ctx = context();
    let mut buffer = Buffer::with_capacity(&mut ctx, 4).unwrap();
    for _ in 0..3 {
        let element = ctx.import(&hash_chain(MAX_VALUE_DEPTH - 1)).unwrap();
        buffer.data.push(element);
    }
    assert!(ctx.stats().retained_memory_bytes > 0);
    drop(buffer);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn display_and_debug_render_deep_values_without_recursion() {
    on_small_stack(|| {
        let text = array_chain(DEEP).to_string();
        assert_eq!(text, format!("{}0{}", "[".repeat(DEEP), "]".repeat(DEEP)));
        let text = hash_chain(DEEP).to_string();
        assert_eq!(
            text,
            format!("{}0{}", "{k: ".repeat(DEEP), "}".repeat(DEEP))
        );
        let debug = format!("{:?}", array_chain(DEEP));
        assert_eq!(
            debug,
            format!(
                "{}Value(Int(0)){}",
                "Value(Array([".repeat(DEEP),
                "]))".repeat(DEEP)
            )
        );
        let debug = format!("{:?}", mixed_chain(DEEP));
        assert!(debug.starts_with("Value(Array([Value(Hash({Value(Bytes("));
        assert_eq!(debug.matches("Value(Int(0))").count(), 1);
        assert_eq!(debug.matches("Value(Array([").count(), DEEP.div_ceil(2));
        assert_eq!(debug.matches("Value(Hash({").count(), DEEP / 2);
        assert!(debug.ends_with("]))"));
    });
}

#[test]
fn rendering_keeps_existing_display_shapes_and_leaf_debug_forms() {
    let value = Value::array(vec![
        Value::int(1),
        Value::hash(vec![
            (b"a".to_vec(), Value::bytes("x")),
            (b"b".to_vec(), Value::array(vec![])),
        ]),
        Value::nil(),
        Value::object(vec![]),
    ]);
    assert_eq!(value.to_string(), "[1, {a: x, b: []}, nil, <object>]");
    assert_eq!(Value::hash(vec![]).to_string(), "{}");
    assert_eq!(format!("{:?}", Value::int(1)), "Value(Int(1))");
    assert_eq!(format!("{:?}", Value::nil()), "Value(Nil)");
    assert_eq!(
        format!(
            "{:?}",
            Value::array(vec![Value::nil(), Value::boolean(true)])
        ),
        "Value(Array([Value(Nil), Value(Bool(true))]))"
    );
    assert_eq!(format!("{:?}", Value::object(vec![])), "Value(Object({}))");

    let mut ctx = context();
    let key = ctx.bytes(b"to_s").unwrap();
    let mut hash = Hash::empty();
    hash.insert(&mut ctx, key, Value::bytes("shown")).unwrap();
    hash.tag = Tag::Match;
    let protected = Value::from_hash(&mut ctx, hash).unwrap();
    assert_eq!(protected.to_string(), "shown");
    assert!(format!("{protected:?}").starts_with("Value(Hash(Match, {"));
    assert_eq!(Value::array(vec![protected.clone()]).to_string(), "[shown]");

    let mut other = context();
    let imported = other.import(&protected).unwrap();
    let Kind::Hash(hash) = &imported.0 else {
        unreachable!()
    };
    assert_eq!(hash.tag, Tag::Match);
    assert_eq!(imported.to_string(), "shown");
    let object = other
        .import(&Value::object(vec![(b"a".to_vec(), Value::int(1))]))
        .unwrap();
    assert_eq!(object.type_name(), "object");
    assert_eq!(object.to_string(), "<object>");
}

#[test]
fn imports_enforce_exact_work_and_memory_boundaries() {
    let host = mixed_chain(MAX_VALUE_DEPTH);
    let mut ctx = context();
    let imported = ctx.import(&host).unwrap();
    assert_eq!(imported.depth(), MAX_VALUE_DEPTH);
    assert_eq!(imported.to_string(), host.to_string());
    let stats = ctx.stats();
    assert!(stats.retained_memory_bytes > 0);
    // Traversal frames were charged while the copy was built and released afterwards.
    assert!(stats.peak_memory_bytes > stats.retained_memory_bytes);
    drop(imported);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);

    for (steps, memory) in [(true, false), (false, true)] {
        for exact in [true, false] {
            let mut ctx = context();
            if steps {
                ctx.options.limits.steps = Some(stats.steps - usize::from(!exact) as u64);
            }
            if memory {
                ctx.options.limits.memory_bytes =
                    Some(stats.peak_memory_bytes - usize::from(!exact));
            }
            let result = ctx.import(&host);
            if exact {
                let imported = result.unwrap();
                assert_eq!(ctx.stats().steps, stats.steps);
                assert_eq!(ctx.stats().peak_memory_bytes, stats.peak_memory_bytes);
                drop(imported);
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    error.kind,
                    if steps {
                        ErrorKind::Steps
                    } else {
                        ErrorKind::Memory
                    }
                );
                assert_eq!(ctx.checkpoint().unwrap_err(), error);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn interrupted_imports_release_partial_frames_and_deep_siblings() {
    let host = Value::array((0..3).map(|_| mixed_chain(MAX_VALUE_DEPTH - 1)).collect());
    let mut ctx = context();
    let imported = ctx.import(&host).unwrap();
    let stats = ctx.stats();
    drop(imported);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    for limit in [stats.steps / 3, stats.steps / 2, stats.steps - 1] {
        let mut ctx = context();
        ctx.options.limits.steps = Some(limit);
        assert_eq!(ctx.import(&host).unwrap_err().kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert!(ctx.stats().steps > limit);
    }
    let mut ctx = context();
    ctx.options.limits.memory_bytes = Some(stats.peak_memory_bytes / 2);
    assert_eq!(ctx.import(&host).unwrap_err().kind, ErrorKind::Memory);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn same_budget_containers_are_shared_unless_rooted() {
    let mut ctx = context();
    let owned = ctx.import(&mixed_chain(5)).unwrap();
    let stats = ctx.stats();
    let alias = ctx.import(&owned).unwrap();
    assert_eq!(array_ptr(&owned), array_ptr(&alias));
    assert_eq!(
        ctx.stats().retained_memory_bytes,
        stats.retained_memory_bytes
    );
    assert_eq!(ctx.stats().steps, stats.steps + 1);

    let wrapper = Value::array(vec![owned.clone(), Value::int(7)]);
    let imported = ctx.import(&wrapper).unwrap();
    let inner = &imported.as_array().unwrap()[0];
    assert_eq!(array_ptr(inner), array_ptr(&owned));
    assert_eq!(imported.as_array().unwrap()[1].as_int(), Some(7));

    let retained = ctx.stats().retained_memory_bytes;
    let copied = ctx.import_rooted(&owned).unwrap();
    assert_ne!(array_ptr(&copied), array_ptr(&owned));
    assert_eq!(copied.to_string(), owned.to_string());
    assert!(ctx.stats().retained_memory_bytes > retained);
    drop((alias, imported, copied, owned, wrapper));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn imports_keep_independent_per_call_accounting() {
    let host = hash_chain(16);
    let mut first = context();
    let mut second = context();
    let a = first.import(&host).unwrap();
    let b = second.import(&a).unwrap();
    assert_ne!(a.as_hash().unwrap().as_ptr(), b.as_hash().unwrap().as_ptr());
    let first_retained = first.stats().retained_memory_bytes;
    let second_retained = second.stats().retained_memory_bytes;
    assert!(first_retained > 0 && second_retained > 0);
    drop(a);
    assert_eq!(first.stats().retained_memory_bytes, 0);
    assert_eq!(second.stats().retained_memory_bytes, second_retained);
    assert_eq!(b.to_string(), host.to_string());
    drop(b);
    assert_eq!(second.stats().retained_memory_bytes, 0);
}

#[test]
fn imports_reject_values_above_the_container_limit_before_copying() {
    on_small_stack(|| {
        for levels in [MAX_VALUE_DEPTH + 1, DEEP] {
            let mut ctx = context();
            let error = ctx.import(&array_chain(levels)).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.class(), Some(ErrorClass::Limit));
            assert_eq!(ctx.stats().steps, 1);
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
            // Limit guards do not latch: the call may continue with other work.
            ctx.charge(1).unwrap();
            assert_eq!(
                ctx.import(&hash_chain(levels)).unwrap_err().kind,
                ErrorKind::Recursion
            );
        }
    });
}

#[test]
fn imports_observe_cancellation_deadlines_and_latched_exhaustion() {
    let mut ctx = context();
    ctx.cancellation().cancel();
    assert_eq!(
        ctx.import(&mixed_chain(4)).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    let mut ctx = context();
    ctx.options.deadline = Some(Instant::now());
    assert_eq!(
        ctx.import(&mixed_chain(4)).unwrap_err().kind,
        ErrorKind::Deadline
    );
    let mut ctx = context();
    let error = ctx.charge(u64::MAX).unwrap_err();
    assert_eq!(ctx.import(&Value::int(1)).unwrap_err(), error);
    assert_eq!(ctx.import(&mixed_chain(4)).unwrap_err(), error);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn scalar_runs_import_at_one_step_per_element_under_every_quota() {
    let mut items = Vec::new();
    for i in 0..5000 {
        items.push(match i % 97 {
            0 => Value::bytes(format!("s{i}")),
            1 => Value::array(vec![Value::int(i), Value::nil()]),
            2 => Value::float(i as f64 / 2.0),
            3 => Value::nil(),
            4 => Value::boolean(i % 2 == 0),
            _ => Value::int(i),
        });
    }
    let host = Value::array(items);
    let mut ctx = context();
    let imported = ctx.import(&host).unwrap();
    let stats = ctx.stats();
    assert!(crate::ops::equal(&mut ctx, &imported, &host, 0).unwrap());
    drop(imported);
    for limit in (0..=stats.steps)
        .step_by(97)
        .chain([stats.steps - 1, stats.steps])
    {
        let mut ctx = context();
        ctx.options.limits.steps = Some(limit);
        match ctx.import(&host) {
            Ok(imported) => {
                assert_eq!(limit, stats.steps);
                assert_eq!(ctx.stats().steps, stats.steps);
                assert_eq!(ctx.stats().peak_memory_bytes, stats.peak_memory_bytes);
                drop(imported);
            }
            // Charging a run at once fails where charging each element would.
            Err(error) => {
                assert_eq!(error.kind, ErrorKind::Steps);
                assert_eq!(ctx.stats().steps, limit + 1, "{limit}");
            }
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
