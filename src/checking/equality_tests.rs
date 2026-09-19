use super::{
    collection_tests::literal_fact,
    facts::{Atom, Fact, Facts, Field, HashKind, Node},
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value, budget::MAX_VALUE_DEPTH};

fn exact(ctx: &mut CallContext, facts: &mut Facts, value: &Value) -> Fact {
    use crate::{budget::Buffer, value::Kind};
    match &value.0 {
        Kind::Float(n) => facts.float(ctx, *n).unwrap(),
        Kind::Array(values) => {
            let items: Vec<_> = values
                .buffer
                .data
                .iter()
                .map(|v| exact(ctx, facts, v))
                .collect();
            facts.tuple(ctx, &items).unwrap()
        }
        Kind::Hash(hash) => {
            let mut fields = Buffer::empty();
            for (name, value) in &hash.buffer.data {
                let value = exact(ctx, facts, value);
                fields
                    .push(
                        ctx,
                        Field {
                            name: name.clone(),
                            value,
                            optional: false,
                        },
                    )
                    .unwrap();
            }
            facts
                .shape_fields(
                    ctx,
                    fields,
                    false,
                    Atom::String.fact(),
                    if hash.object {
                        HashKind::Object
                    } else {
                        HashKind::Plain
                    },
                )
                .unwrap()
        }
        _ => literal_fact(ctx, facts, value),
    }
}

#[test]
fn structural_equality_matches_runtime_numeric_keys_and_container_kinds() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let scalars = [
        Value::nil(),
        Value::boolean(false),
        Value::boolean(true),
        Value::int(0),
        Value::int(1),
        Value::int(9_007_199_254_740_993),
        Value::int(i64::MIN),
        Value::int(i64::MAX),
        Value::float(-0.0),
        Value::float(1.0),
        Value::float(9_007_199_254_740_992.0),
        Value::float(i64::MIN as f64),
        Value::float(i64::MAX as f64),
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
        Value::bytes(b"a"),
        Value::symbol(b"a"),
    ];
    let mut values = Vec::new();
    for value in scalars {
        values.push(value.clone());
        values.push(Value::array(vec![value.clone()]));
        values.push(Value::hash(vec![(b"a".to_vec(), value.clone())]));
        values.push(Value::object(vec![(b"a".to_vec(), value)]));
    }
    for left in &values {
        for right in &values {
            let a = exact(&mut ctx, &mut facts, left);
            let b = exact(&mut ctx, &mut facts, right);
            let (value, limit) = facts.value_equal(&mut ctx, a, b).unwrap();
            assert!(!limit);
            let expected = crate::ops::equal(&mut ctx, left, right, 0).unwrap();
            assert!(
                matches!(facts.node(value), Node::Boolean(actual) if *actual == expected),
                "{left:?} == {right:?}: {:?}",
                facts.node(value)
            );
        }
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn structural_equality_keeps_abstract_values_and_hash_kinds_uncertain() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let enumeration = crate::Engine::new()
        .compile("enum E; A; B; end; E")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let enumeration = facts.enumeration(&mut ctx, &enumeration).unwrap();
    let member = facts.enum_member(&mut ctx, enumeration, 0).unwrap();
    let choice = facts.choice(&mut ctx, &[member, Atom::Any.fact()]).unwrap();
    assert!(matches!(facts.node(choice), Node::Choice(_)));
    let unresolved = facts.tuple(&mut ctx, &[choice]).unwrap();
    let known = facts.tuple(&mut ctx, &[member]).unwrap();
    for (left, right) in [(unresolved, known), (known, unresolved)] {
        assert_eq!(
            facts.value_equal(&mut ctx, left, right).unwrap(),
            (Atom::Bool.fact(), false)
        );
        assert_eq!(
            facts.set_equal(&mut ctx, left, right).unwrap(),
            Atom::Bool.fact()
        );
    }
    let tuple = facts.tuple(&mut ctx, &[Atom::Int.fact()]).unwrap();
    let array = facts.array(&mut ctx, Atom::Int.fact()).unwrap();
    let shape = facts
        .shape(&mut ctx, &[(b"a", Atom::Int.fact(), false)], false)
        .unwrap();
    let optional = facts
        .shape(&mut ctx, &[(b"a", Atom::Nil.fact(), true)], false)
        .unwrap();
    let open = facts
        .shape(&mut ctx, &[(b"a", Atom::Int.fact(), false)], true)
        .unwrap();
    let any_kind = facts
        .hash_kind(
            &mut ctx,
            Atom::String.fact(),
            Atom::Int.fact(),
            HashKind::Any,
        )
        .unwrap();
    let either = facts.union(&mut ctx, &[Atom::Nil.fact(), tuple]).unwrap();
    for (value, guarded) in [
        (tuple, false),
        (array, false),
        (shape, false),
        (optional, false),
        (open, true),
        (any_kind, false),
        (either, false),
        (Atom::Any.fact(), true),
    ] {
        assert_eq!(
            facts.value_equal(&mut ctx, value, value).unwrap(),
            (Atom::Bool.fact(), guarded)
        );
    }
    let protected = facts
        .protected(&mut ctx, shape, crate::hash::Tag::Match)
        .unwrap();
    assert_eq!(
        facts.value_equal(&mut ctx, protected, shape).unwrap(),
        (Atom::Bool.fact(), false)
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn structural_equality_depth_guards_follow_array_order_and_memo_depth() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let mut deep = one;
    for _ in 0..MAX_VALUE_DEPTH {
        deep = facts.tuple(&mut ctx, &[deep, deep]).unwrap();
    }
    let (value, limit) = facts.value_equal(&mut ctx, deep, deep).unwrap();
    assert!(!limit && matches!(facts.node(value), Node::Boolean(true)));
    let extra = facts.tuple(&mut ctx, &[deep]).unwrap();
    assert_eq!(
        facts.value_equal(&mut ctx, extra, extra).unwrap(),
        (Atom::Never.fact(), true)
    );
    for (left, right, expected, limit) in [
        ([one, deep], [two, deep], Some(false), false),
        ([deep, one], [deep, two], None, true),
    ] {
        let a = facts.tuple(&mut ctx, &left).unwrap();
        let b = facts.tuple(&mut ctx, &right).unwrap();
        let (value, guarded) = facts.value_equal(&mut ctx, a, b).unwrap();
        assert_eq!(guarded, limit);
        assert_eq!(
            match facts.node(value) {
                Node::Boolean(value) => Some(*value),
                Node::Atom(Atom::Never) => None,
                other => panic!("{other:?}"),
            },
            expected
        );
    }
    // The same subgraph succeeds at one depth and guards when reached one layer deeper.
    let mut shared = one;
    for _ in 0..MAX_VALUE_DEPTH - 1 {
        shared = facts.tuple(&mut ctx, &[shared]).unwrap();
    }
    let wrapped = facts.tuple(&mut ctx, &[shared]).unwrap();
    let both = facts.tuple(&mut ctx, &[shared, wrapped]).unwrap();
    assert_eq!(
        facts.value_equal(&mut ctx, both, both).unwrap(),
        (Atom::Never.fact(), true)
    );
    for _ in 0..4000 {
        deep = facts.tuple(&mut ctx, &[deep, deep]).unwrap();
    }
    assert_eq!(
        facts.value_equal(&mut ctx, deep, deep).unwrap(),
        (Atom::Never.fact(), true)
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn structural_hash_equality_retains_unknown_insertion_order_at_depth_guards() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut deep = facts.integer(&mut ctx, 1).unwrap();
    for _ in 0..MAX_VALUE_DEPTH {
        deep = facts.tuple(&mut ctx, &[deep]).unwrap();
    }
    let a = facts
        .shape(
            &mut ctx,
            &[(b"a", deep, false), (b"b", Atom::Nil.fact(), false)],
            false,
        )
        .unwrap();
    let b = facts
        .shape(
            &mut ctx,
            &[(b"a", deep, false), (b"c", Atom::Nil.fact(), false)],
            false,
        )
        .unwrap();
    let (value, limit) = facts.value_equal(&mut ctx, a, b).unwrap();
    assert!(limit && matches!(facts.node(value), Node::Boolean(false)));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let one = facts.integer(ctx, 1)?;
    let float = facts.float(ctx, 1.0)?;
    let choice = facts.union(ctx, &[one, Atom::Unknown.fact()])?;
    let mut a = facts.shape(ctx, &[(b"a", choice, false), (b"b", one, false)], false)?;
    let mut b = facts.shape(ctx, &[(b"a", float, false), (b"b", float, false)], false)?;
    for _ in 0..20 {
        a = facts.tuple(ctx, &[a, a])?;
        b = facts.tuple(ctx, &[b, b])?;
    }
    assert_eq!(facts.value_equal(ctx, a, b)?, (Atom::Bool.fact(), false));
    Ok(())
}

#[test]
fn structural_equality_accounts_for_interrupted_graph_walks_and_releases_storage() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, expected) in [
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
        assert_eq!(result.as_ref().err().map(|e| e.kind), expected);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for (limit, kind) in [
        (stats.peak_memory_bytes, ErrorKind::Memory),
        (usize::try_from(stats.steps).unwrap(), ErrorKind::Steps),
    ] {
        for amount in (0..limit).step_by(127) {
            let mut limits = Limits::default();
            if kind == ErrorKind::Memory {
                limits.memory_bytes = Some(amount);
            } else {
                limits.steps = Some(amount as u64);
            }
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let error = accounting(&mut ctx).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.options.cancellation.cancel();
        }
        let error = accounting(&mut ctx).unwrap_err();
        assert_eq!(
            error.kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
