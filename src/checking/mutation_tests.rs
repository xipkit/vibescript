use super::{
    collection_tests::{literal_fact, literal_values},
    facts::{Atom, Facts, Node},
    mutations::Mutation,
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value, bytecode::Method};

fn contains(ctx: &mut CallContext, facts: &mut Facts, inferred: Mutation, actual: (Value, Value)) {
    assert!(!inferred.rejected, "rejected {actual:?}: {inferred:?}");
    assert!(
        !inferred.unsupported,
        "unsupported {actual:?}: {inferred:?}"
    );
    for (actual, expected) in [(actual.0, inferred.receiver), (actual.1, inferred.value)] {
        let value = literal_fact(ctx, facts, &actual);
        assert_ne!(
            facts.relation(ctx, value, expected).unwrap(),
            Relation::Rejected,
            "{actual:?} excluded from {:?}",
            facts.node(expected)
        );
    }
}

#[test]
fn mutation_results_and_receivers_contain_runtime_values() {
    let mut cases = 0;
    let mut successes = 0;
    for receiver in literal_values() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let root = literal_fact(&mut ctx, &mut facts, &receiver);
        for method in [
            Method::Push,
            Method::Prepend,
            Method::Insert,
            Method::Pop,
            Method::Shift,
            Method::Delete,
            Method::Clear,
            Method::Fill,
            Method::Store,
            Method::Replace,
        ] {
            let arguments = [
                vec![],
                vec![Value::nil()],
                vec![Value::boolean(true)],
                vec![Value::int(-9)],
                vec![Value::int(-1)],
                vec![Value::int(0)],
                vec![Value::int(1)],
                vec![Value::int(2)],
                vec![Value::int(7)],
                vec![Value::float(0.5)],
                vec![Value::float(f64::NAN)],
                vec![Value::bytes(b"yes")],
                vec![Value::bytes(b"x")],
                vec![Value::symbol(b"x")],
                vec![Value::array(vec![])],
                vec![Value::hash(vec![(b"x".to_vec(), Value::int(7))])],
                vec![Value::int(0), Value::bytes(b"yes")],
                vec![Value::int(-1), Value::bytes(b"yes")],
                vec![Value::int(4), Value::bytes(b"yes")],
                vec![Value::symbol(b"x"), Value::int(8)],
                vec![Value::bytes(b"yes"), Value::int(0), Value::int(1)],
                vec![Value::bytes(b"yes"), Value::int(4), Value::int(0)],
                vec![Value::bytes(b"yes"), Value::int(4), Value::int(1)],
                vec![Value::bytes(b"yes"), Value::nil(), Value::int(-1)],
                vec![Value::bytes(b"yes"), Value::nil(), Value::int(0)],
                vec![Value::bytes(b"yes"), Value::bytes(b"bad"), Value::int(-1)],
            ];
            for args in arguments {
                let args_facts: Vec<_> = args
                    .iter()
                    .map(|arg| literal_fact(&mut ctx, &mut facts, arg))
                    .collect();
                let inferred = facts
                    .collection_mutate(&mut ctx, root, method, &args_facts)
                    .unwrap();
                assert!(!inferred.unsupported, "{receiver:?}.{method:?}({args:?})");
                let mut runtime = CallContext::new(CallOptions::default());
                // The gradual checker models the ADR-004 language, which pads.
                runtime.legacy = true;
                match crate::mutate::call(&mut runtime, method, "", receiver.clone(), &args) {
                    Ok(actual) => {
                        contains(&mut ctx, &mut facts, inferred, actual);
                        successes += 1;
                    }
                    Err(error)
                        if facts.singleton(root)
                            && args_facts.iter().all(|&arg| facts.singleton(arg)) =>
                    {
                        assert!(
                            inferred.rejected,
                            "missed {receiver:?}.{method:?}({args:?}): {error}"
                        );
                    }
                    Err(_) => (),
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 3120);
    assert_eq!(successes, 258);
}

#[test]
fn fill_windows_match_runtime_for_numeric_and_range_selectors() {
    let mut cases = 0;
    for length in 0..4 {
        let receiver = Value::array((0..length).map(Value::int).collect());
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let root = literal_fact(&mut ctx, &mut facts, &receiver);
        for start in -5..=5 {
            for end in -5..=5 {
                for mode in 0..3 {
                    let mut runtime = CallContext::new(CallOptions::default());
                    let args = if mode == 0 {
                        vec![Value::bytes(b"filled"), Value::int(start), Value::int(end)]
                    } else {
                        let range = crate::range::Range::new(
                            &mut runtime,
                            Some(start),
                            Some(end),
                            mode == 2,
                        )
                        .unwrap();
                        vec![
                            Value::bytes(b"filled"),
                            Value(crate::value::Kind::Range(range)),
                        ]
                    };
                    let args_facts: Vec<_> = args
                        .iter()
                        .map(|arg| literal_fact(&mut ctx, &mut facts, arg))
                        .collect();
                    let inferred = facts
                        .collection_mutate(&mut ctx, root, Method::Fill, &args_facts)
                        .unwrap();
                    assert!(!inferred.unsupported);
                    if let Ok(actual) = crate::mutate::call(
                        &mut runtime,
                        Method::Fill,
                        "fill",
                        receiver.clone(),
                        &args,
                    ) {
                        contains(&mut ctx, &mut facts, inferred, actual);
                    }
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 1452);
}

#[test]
fn generalized_mutations_include_concrete_execution_results() {
    let mut cases = 0;
    for receiver in literal_values() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let literal = literal_fact(&mut ctx, &mut facts, &receiver);
        let root = match facts.node(literal) {
            Node::Tuple(_) => {
                let element = facts.elements(&mut ctx, literal).unwrap();
                facts.array(&mut ctx, element).unwrap()
            }
            Node::Shape(..) => {
                let values = facts.shape_values(&mut ctx, literal, false).unwrap();
                facts
                    .hash_kind(&mut ctx, Atom::String.fact(), values, true)
                    .unwrap()
            }
            Node::String(_) => Atom::String.fact(),
            _ => literal,
        };
        for method in [
            Method::Push,
            Method::Pop,
            Method::Shift,
            Method::Delete,
            Method::Store,
            Method::Fill,
            Method::Insert,
            Method::Replace,
        ] {
            for args in [
                vec![],
                vec![Value::int(1)],
                vec![Value::bytes(b"x")],
                vec![Value::int(0), Value::bytes(b"new")],
                vec![Value::bytes(b"x"), Value::int(7)],
                vec![Value::bytes(b"new"), Value::int(1), Value::int(2)],
            ] {
                let args_facts: Vec<_> = args
                    .iter()
                    .map(|arg| {
                        let literal = literal_fact(&mut ctx, &mut facts, arg);
                        facts.atom(literal).map_or(literal, |atom| atom.fact())
                    })
                    .collect();
                let inferred = facts
                    .collection_mutate(&mut ctx, root, method, &args_facts)
                    .unwrap();
                assert!(!inferred.unsupported);
                let mut runtime = CallContext::new(CallOptions::default());
                if let Ok(actual) =
                    crate::mutate::call(&mut runtime, method, "", receiver.clone(), &args)
                {
                    contains(&mut ctx, &mut facts, inferred, actual);
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 576);
}

#[test]
fn indexed_write_results_and_receivers_contain_runtime_values() {
    let mut cases = 0;
    for receiver in literal_values() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let root = literal_fact(&mut ctx, &mut facts, &receiver);
        for index in [
            Value::nil(),
            Value::boolean(true),
            Value::int(i64::MIN),
            Value::int(-1),
            Value::int(0),
            Value::int(1),
            Value::int(i64::MAX),
            Value::float(0.5),
            Value::float(f64::NAN),
            Value::bytes(b"x"),
            Value::symbol(b"x"),
            Value::array(vec![]),
        ] {
            let key = literal_fact(&mut ctx, &mut facts, &index);
            for value in [
                Value::int(8),
                Value::bytes(b"changed"),
                Value::array(vec![]),
            ] {
                let rhs = literal_fact(&mut ctx, &mut facts, &value);
                let inferred = facts.collection_write(&mut ctx, root, key, rhs).unwrap();
                assert!(!inferred.unsupported, "{receiver:?}[{index:?}]={value:?}");
                let mut runtime = CallContext::new(CallOptions::default());
                match crate::ops::set_index(
                    &mut runtime,
                    receiver.clone(),
                    index.clone(),
                    value.clone(),
                ) {
                    Ok(updated) => contains(&mut ctx, &mut facts, inferred, (updated, value)),
                    Err(error) if facts.singleton(root) && facts.singleton(key) => {
                        assert!(
                            inferred.rejected,
                            "missed {receiver:?}[{index:?}]={value:?}: {error}"
                        );
                    }
                    Err(_) => (),
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 432);
}

#[test]
fn tuple_writes_keep_exact_positions_and_pop_has_a_separate_result() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let minus_one = facts.integer(&mut ctx, -1).unwrap();
    let text = facts.string(&mut ctx, b"changed").unwrap();
    let original = facts.tuple(&mut ctx, &[one, two]).unwrap();
    let replaced = facts
        .collection_write(&mut ctx, original, minus_one, text)
        .unwrap();
    assert_eq!(replaced.value, text);
    assert_eq!(
        replaced.receiver,
        facts.tuple(&mut ctx, &[one, text]).unwrap()
    );
    assert_eq!(original, facts.tuple(&mut ctx, &[one, two]).unwrap());
    let result = facts
        .collection_mutate(&mut ctx, replaced.receiver, Method::Pop, &[])
        .unwrap();
    assert_eq!(result.value, text);
    assert_eq!(result.receiver, facts.tuple(&mut ctx, &[one]).unwrap());
    let result = facts
        .collection_mutate(&mut ctx, original, Method::Shift, &[one])
        .unwrap();
    assert_eq!(result.value, facts.tuple(&mut ctx, &[one]).unwrap());
    assert_eq!(result.receiver, facts.tuple(&mut ctx, &[two]).unwrap());
    let invalid = facts.union(&mut ctx, &[minus_one, text]).unwrap();
    let result = facts
        .collection_write(&mut ctx, original, invalid, one)
        .unwrap();
    assert!(result.rejected);
    assert!(!result.unsupported);
    assert_eq!(result.receiver, facts.tuple(&mut ctx, &[one, one]).unwrap());
}

#[test]
fn delete_compares_values_without_confusing_type_facts_with_identity() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let first = facts.tuple(&mut ctx, &[one]).unwrap();
    let second = facts.tuple(&mut ctx, &[two]).unwrap();
    let original = facts.tuple(&mut ctx, &[first, second, first]).unwrap();
    let result = facts
        .collection_mutate(&mut ctx, original, Method::Delete, &[first])
        .unwrap();
    assert_eq!(result.receiver, facts.tuple(&mut ctx, &[second]).unwrap());
    assert_eq!(result.value, first);
    let numbers = facts
        .tuple(&mut ctx, &[Atom::Int.fact(), Atom::Int.fact()])
        .unwrap();
    let result = facts
        .collection_mutate(&mut ctx, numbers, Method::Delete, &[Atom::Int.fact()])
        .unwrap();
    assert_eq!(
        result.receiver,
        facts.array(&mut ctx, Atom::Int.fact()).unwrap()
    );
    assert_eq!(
        result.value,
        facts
            .union(&mut ctx, &[Atom::Int.fact(), Atom::Nil.fact()])
            .unwrap()
    );
    let original = facts
        .tuple(&mut ctx, &[one, Atom::Nil.fact(), first])
        .unwrap();
    let result = facts
        .collection_mutate(&mut ctx, original, Method::Delete, &[Atom::Nil.fact()])
        .unwrap();
    assert_eq!(
        result.receiver,
        facts.tuple(&mut ctx, &[one, first]).unwrap()
    );
    assert_eq!(result.value, Atom::Nil.fact());
}

#[test]
fn dynamic_hash_writes_preserve_plain_dispatch_and_do_not_change_annotations() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let text = facts.string(&mut ctx, b"changed").unwrap();
    let original = facts.shape(&mut ctx, &[(b"x", one, false)], false).unwrap();
    let result = facts
        .collection_write(&mut ctx, original, Atom::String.fact(), text)
        .unwrap();
    assert!(facts.plain_hash(result.receiver));
    let element = facts.union(&mut ctx, &[one, text]).unwrap();
    let annotation = facts.hash(&mut ctx, Atom::String.fact(), element).unwrap();
    assert_ne!(result.receiver, annotation);
    assert!(!facts.plain_hash(annotation));
    let object_write = facts
        .collection_write(&mut ctx, annotation, Atom::String.fact(), text)
        .unwrap();
    assert!(!object_write.unsupported && !object_write.rejected);
    assert!(!facts.plain_hash(object_write.receiver));
    assert_eq!(object_write.value, text);
    assert_eq!(
        facts
            .relation(&mut ctx, result.receiver, annotation)
            .unwrap(),
        Relation::Accepted
    );
    assert!(
        facts
            .collection_mutate(&mut ctx, annotation, Method::Clear, &[])
            .unwrap()
            .unsupported
    );
    for (name, expected) in [("keys", Atom::String.fact()), ("values", element)] {
        let site = crate::bytecode::CallSite {
            name: 0,
            method: Method::parse(name),
            auto: true,
            parenthesized: false,
            scope: false,
        };
        let projected = facts
            .collection_member(&mut ctx, result.receiver, site, name, &[])
            .unwrap();
        assert!(!projected.rejected && !projected.unsupported);
        assert_eq!(projected.value, facts.array(&mut ctx, expected).unwrap());
    }
    let key = facts.symbol(&mut ctx, b"x").unwrap();
    let replaced = facts
        .collection_write(&mut ctx, original, key, text)
        .unwrap();
    assert_eq!(
        replaced.receiver,
        facts
            .shape(&mut ctx, &[(b"x", text, false)], false)
            .unwrap()
    );
    let deleted = facts
        .collection_mutate(&mut ctx, replaced.receiver, Method::Delete, &[key])
        .unwrap();
    assert_eq!(deleted.value, text);
    assert_eq!(deleted.receiver, facts.shape(&mut ctx, &[], false).unwrap());
    let deleted = facts
        .collection_mutate(&mut ctx, original, Method::Delete, &[Atom::String.fact()])
        .unwrap();
    assert_eq!(
        deleted.receiver,
        facts.shape(&mut ctx, &[(b"x", one, true)], false).unwrap()
    );
}

#[test]
fn string_methods_keep_the_original_receiver_and_validate_all_known_arms() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let original = facts.string(&mut ctx, b"old").unwrap();
    let replacement = facts.string(&mut ctx, b"new").unwrap();
    let mixed = facts
        .union(&mut ctx, &[replacement, Atom::Int.fact()])
        .unwrap();
    let result = facts
        .collection_mutate(&mut ctx, original, Method::Replace, &[mixed])
        .unwrap();
    assert!(result.rejected && !result.unsupported);
    assert_eq!(result.receiver, original);
    assert_eq!(result.value, replacement);
    let result = facts
        .collection_mutate(&mut ctx, original, Method::Clear, &[])
        .unwrap();
    assert_eq!(result.receiver, original);
    assert_eq!(result.value, facts.string(&mut ctx, b"").unwrap());
}

#[test]
fn large_insert_and_fill_windows_do_not_materialize_runtime_sized_fact_vectors() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let array = facts.tuple(&mut ctx, &[]).unwrap();
    let index = facts.integer(&mut ctx, i64::MAX).unwrap();
    let window_end = facts.integer(&mut ctx, isize::MAX as i64).unwrap();
    let zero = facts.integer(&mut ctx, 0).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let before = ctx.stats();
    let inserted = facts
        .collection_mutate(&mut ctx, array, Method::Insert, &[index, one])
        .unwrap();
    assert!(!inserted.unsupported && !inserted.rejected);
    assert_eq!(inserted.value, inserted.receiver);
    assert!(matches!(facts.node(inserted.receiver), Node::Array(_)));
    let overflow = facts
        .collection_mutate(&mut ctx, array, Method::Fill, &[one, index, one])
        .unwrap();
    assert!(overflow.rejected);
    let gap = facts
        .collection_mutate(&mut ctx, array, Method::Fill, &[one, window_end, zero])
        .unwrap();
    assert_eq!(
        gap.receiver,
        facts.array(&mut ctx, Atom::Nil.fact()).unwrap()
    );
    assert!(ctx.stats().steps - before.steps < 500);
    assert!(ctx.stats().peak_memory_bytes - before.peak_memory_bytes < 8192);
}

#[test]
fn unknown_and_known_mutation_arms_remain_distinct() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let array = facts.tuple(&mut ctx, &[Atom::Int.fact()]).unwrap();
    let receiver = facts
        .union(&mut ctx, &[array, Atom::Unknown.fact(), Atom::Bool.fact()])
        .unwrap();
    let result = facts
        .collection_mutate(&mut ctx, receiver, Method::Push, &[Atom::String.fact()])
        .unwrap();
    assert!(result.rejected && !result.unsupported);
    let ints = facts.array(&mut ctx, Atom::Int.fact()).unwrap();
    assert_eq!(
        facts.relation(&mut ctx, result.receiver, ints).unwrap(),
        Relation::Rejected
    );
    let result = facts
        .collection_mutate(
            &mut ctx,
            array,
            Method::Fill,
            &[Atom::String.fact(), Atom::Nil.fact(), Atom::Nil.fact()],
        )
        .unwrap();
    assert_eq!(
        result.receiver,
        facts.tuple(&mut ctx, &[Atom::String.fact()]).unwrap()
    );
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let one = facts.integer(ctx, 1)?;
    let array = facts.tuple(ctx, &[one, Atom::String.fact(), Atom::Nil.fact()])?;
    let hash = facts.shape(ctx, &[(b"x", one, false), (b"y", array, false)], false)?;
    let key = facts.string(ctx, b"x")?;
    for _ in 0..3 {
        let changed = facts.collection_write(ctx, hash, key, array)?;
        let _ = facts.collection_mutate(
            ctx,
            changed.receiver,
            Method::Delete,
            &[Atom::String.fact()],
        )?;
        let _ = facts.collection_mutate(ctx, array, Method::Fill, &[hash, one, one])?;
        let _ = facts.collection_mutate(ctx, array, Method::Pop, &[one])?;
        let _ = facts.collection_mutate(ctx, array, Method::Delete, &[Atom::Nil.fact()])?;
        let _ = facts.collection_mutate(ctx, key, Method::Insert, &[one, key])?;
    }
    Ok(())
}

#[test]
fn mutation_inference_has_exact_quotas_and_releases_failed_allocations() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let stats = ctx.stats();
    for (memory, steps, error) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let result = accounting(&mut ctx);
        assert_eq!(result.as_ref().err().map(|e| e.kind), error);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by(137) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(accounting(&mut ctx).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn mutation_cache_hits_observe_cancellation_deadlines_and_latched_exhaustion() {
    for error in [
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
        ErrorKind::Steps,
        ErrorKind::Memory,
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let array = facts.tuple(&mut ctx, &[Atom::Int.fact()]).unwrap();
        let zero = facts.integer(&mut ctx, 0).unwrap();
        facts
            .collection_write(&mut ctx, array, zero, Atom::Int.fact())
            .unwrap();
        match error {
            ErrorKind::Cancelled => ctx.cancellation().cancel(),
            ErrorKind::Deadline => ctx.options.deadline = Some(std::time::Instant::now()),
            ErrorKind::Steps => {
                ctx.options.limits.steps = Some(ctx.stats().steps);
                ctx.charge(1).unwrap_err();
            }
            ErrorKind::Memory => {
                ctx.options.limits.memory_bytes = Some(ctx.stats().retained_memory_bytes);
                ctx.bytes(b"fail").unwrap_err();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            facts
                .collection_write(&mut ctx, array, zero, Atom::Int.fact())
                .unwrap_err()
                .kind,
            error
        );
        assert_eq!(
            facts
                .collection_mutate(&mut ctx, array, Method::Clear, &[])
                .unwrap_err()
                .kind,
            error
        );
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
