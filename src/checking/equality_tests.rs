use super::{
    collection_tests::literal_fact,
    equality::Policy,
    facts::{Atom, Fact, Facts, Field, HashKind, InstanceKind, Node},
};
use crate::{
    CallContext, CallOptions, ErrorKind, Limits, Result, Value, budget::MAX_VALUE_DEPTH,
    builtin::Builtin,
};

const POLICIES: [Policy; 4] = [Policy::Value, Policy::Set, Policy::Strict, Policy::Identity];

fn decided(facts: &Facts, value: Fact) -> Option<bool> {
    match facts.node(value) {
        Node::Boolean(value) => Some(*value),
        Node::Atom(Atom::Bool) => None,
        other => panic!("{other:?}"),
    }
}

fn runtime_helper(ctx: &mut CallContext, name: &str, left: &Value, right: &Value) -> bool {
    crate::members::equality::invoke(
        ctx,
        false,
        name,
        left,
        std::slice::from_ref(right),
        false,
        false,
    )
    .unwrap()
    .truthy()
}

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
    accounting_policy(ctx, Policy::Value)
}

fn accounting_policy(ctx: &mut CallContext, policy: Policy) -> Result<()> {
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
    let (value, limit) = facts.policy_equal(ctx, a, b, policy)?;
    assert!(!limit);
    // Only the strict helper can settle the comparison: every nested integer
    // field meets a float. The other policies keep the unknown field open.
    let expected = if policy == Policy::Strict {
        Some(false)
    } else {
        None
    };
    assert_eq!(decided(&facts, value), expected);
    Ok(())
}

fn interrupted_walks_release_storage(run: impl Fn(&mut CallContext) -> Result<()>) {
    let mut ctx = CallContext::new(CallOptions::default());
    run(&mut ctx).unwrap();
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
        let result = run(&mut ctx);
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
            let error = run(&mut ctx).unwrap_err();
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
        let error = run(&mut ctx).unwrap_err();
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

#[test]
fn structural_equality_accounts_for_interrupted_graph_walks_and_releases_storage() {
    interrupted_walks_release_storage(accounting);
}

#[test]
fn helper_equality_accounts_for_interrupted_graph_walks_and_releases_storage() {
    for policy in [Policy::Strict, Policy::Identity] {
        interrupted_walks_release_storage(|ctx| accounting_policy(ctx, policy));
    }
}

fn unlimited() -> CallContext {
    CallContext::new(CallOptions {
        limits: Limits {
            steps: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    })
}

#[test]
fn helper_equality_matches_runtime_strict_and_identity_results() {
    let mut ctx = unlimited();
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
        Value::float(0.0),
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
        values.push(Value::array(vec![Value::array(vec![value.clone()])]));
        values.push(Value::hash(vec![(b"a".to_vec(), value.clone())]));
        values.push(Value::object(vec![(b"a".to_vec(), value)]));
    }
    for left in &values {
        for right in &values {
            let a = exact(&mut ctx, &mut facts, left);
            let b = exact(&mut ctx, &mut facts, right);
            for (name, strict) in [("eql?", true), ("equal?", false)] {
                let expected = runtime_helper(&mut ctx, name, left, right);
                let (value, limit) = facts.helper_equal(&mut ctx, a, b, strict).unwrap();
                assert!(!limit, "{left:?}.{name}({right:?})");
                assert_eq!(
                    decided(&facts, value),
                    Some(expected),
                    "{left:?}.{name}({right:?})"
                );
            }
        }
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn assert_policies(
    ctx: &mut CallContext,
    facts: &mut Facts,
    rows: &[(Fact, Fact, [Option<bool>; 4])],
) {
    for &(left, right, expected) in rows {
        for (policy, expected) in POLICIES.into_iter().zip(expected) {
            let (value, limit) = facts.policy_equal(ctx, left, right, policy).unwrap();
            let label = format!(
                "{policy:?}: {:?} / {:?}",
                facts.node(left),
                facts.node(right)
            );
            assert!(!limit, "{label}");
            assert_eq!(decided(facts, value), expected, "{label}");
            if policy == Policy::Set {
                let set = facts.set_equal(ctx, left, right).unwrap();
                assert_eq!(decided(facts, set), expected, "{label}");
            }
        }
    }
}

#[test]
fn helper_equality_policies_separate_numeric_kinds_nan_and_shared_facts() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let one_float = facts.float(&mut ctx, 1.0).unwrap();
    let nan = facts.float(&mut ctx, f64::NAN).unwrap();
    let ints = facts.tuple(&mut ctx, &[one]).unwrap();
    let floats = facts.tuple(&mut ctx, &[one_float]).unwrap();
    let nans = facts.tuple(&mut ctx, &[nan]).unwrap();
    let general = facts.tuple(&mut ctx, &[Atom::Int.fact()]).unwrap();
    let int_shape = facts
        .shape(&mut ctx, &[(b"a", ints, false)], false)
        .unwrap();
    let float_shape = facts
        .shape(&mut ctx, &[(b"a", floats, false)], false)
        .unwrap();
    let object = exact(
        &mut ctx,
        &mut facts,
        &Value::object(vec![(b"a".to_vec(), Value::int(1))]),
    );
    let plain = exact(
        &mut ctx,
        &mut facts,
        &Value::hash(vec![(b"a".to_vec(), Value::int(1))]),
    );
    let (int, float) = (Atom::Int.fact(), Atom::Float.fact());
    let (yes, no) = (Some(true), Some(false));
    // Columns follow POLICIES: value, set, strict, identity.
    assert_policies(
        &mut ctx,
        &mut facts,
        &[
            (one, one, [yes; 4]),
            (one, one_float, [yes, no, no, no]),
            (one_float, one, [yes, no, no, no]),
            (nan, nan, [no, yes, no, yes]),
            (nans, nans, [no; 4]),
            (ints, floats, [yes, yes, no, yes]),
            (floats, ints, [yes, yes, no, yes]),
            (int_shape, float_shape, [yes, yes, no, yes]),
            // Shared general facts never prove equality: a big integer or NaN may hide behind them.
            (int, int, [None; 4]),
            (float, float, [None; 4]),
            (int, float, [None, no, no, no]),
            (int, one_float, [None, no, no, no]),
            (int, one, [None; 4]),
            (general, general, [None; 4]),
            (general, floats, [None, None, no, None]),
            (general, ints, [None; 4]),
            (float, nan, [no, None, no, None]),
            (nan, float, [no, None, no, None]),
            (object, plain, [no, None, no, no]),
            // Set membership does not compare object shapes structurally.
            (object, object, [yes, None, yes, yes]),
            (Atom::String.fact(), Atom::Symbol.fact(), [no; 4]),
            (Atom::Time.fact(), Atom::Time.fact(), [None; 4]),
            (Atom::Time.fact(), Atom::Duration.fact(), [no; 4]),
            (Atom::Unknown.fact(), one, [None; 4]),
            (Atom::Any.fact(), ints, [None; 4]),
        ],
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn helper_equality_rejects_known_runtime_type_mismatches_at_typed_positions() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let class = facts.nominal(&mut ctx, 0, 0, b"C", None).unwrap();
    let first = facts.instance(&mut ctx, class, 0).unwrap();
    let second = facts.instance(&mut ctx, class, 1).unwrap();
    let symbolic = facts
        .instance_kind(&mut ctx, class, 2, InstanceKind::Symbolic)
        .unwrap();
    let enumeration = crate::Engine::new()
        .compile("enum E; A; B; end; E")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let enumeration = facts.enumeration(&mut ctx, &enumeration).unwrap();
    let member_a = facts.enum_member(&mut ctx, enumeration, 0).unwrap();
    let member_b = facts.enum_member(&mut ctx, enumeration, 1).unwrap();
    let parse = facts.builtin(&mut ctx, Builtin::JsonParse).unwrap();
    let stringify = facts.builtin(&mut ctx, Builtin::JsonStringify).unwrap();
    let positions = facts.nullable(&mut ctx, Atom::Int.fact()).unwrap();
    let positions = facts.array(&mut ctx, positions).unwrap();
    let offset = facts.offset(&mut ctx, positions).unwrap();
    let empty = facts.tuple(&mut ctx, &[]).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let firsts = facts.tuple(&mut ctx, &[first]).unwrap();
    let empties = facts.tuple(&mut ctx, &[empty]).unwrap();
    let generals = facts.tuple(&mut ctx, &[Atom::Int.fact()]).unwrap();
    let shape = facts
        .shape(&mut ctx, &[(b"a", Atom::Int.fact(), false)], false)
        .unwrap();
    let protected = facts
        .protected(&mut ctx, shape, crate::hash::Tag::Match)
        .unwrap();
    let (yes, no) = (Some(true), Some(false));
    // Columns follow POLICIES: value, set, strict, identity.
    assert_policies(
        &mut ctx,
        &mut facts,
        &[
            (first, first, [yes; 4]),
            (first, second, [no; 4]),
            (symbolic, symbolic, [yes; 4]),
            (first, symbolic, [no; 4]),
            (firsts, firsts, [yes; 4]),
            // Instances never invoke user methods; a nested instance keeps ordinary identity.
            (first, empty, [no; 4]),
            (first, one, [no; 4]),
            (firsts, empties, [no; 4]),
            // Only typed positions can reject an instance against a general fact.
            (first, Atom::Int.fact(), [None, no, no, no]),
            (symbolic, empty, [None, no, no, no]),
            (firsts, generals, [None, None, no, None]),
            (enumeration, enumeration, [yes; 4]),
            (member_a, member_a, [yes; 4]),
            (member_a, member_b, [no; 4]),
            (enumeration, member_a, [no; 4]),
            (member_a, one, [no; 4]),
            (member_a, Atom::Unknown.fact(), [None; 4]),
            (parse, parse, [yes; 4]),
            (parse, stringify, [no; 4]),
            (parse, offset, [None, no, no, no]),
            (offset, offset, [None; 4]),
            (offset, Atom::Nil.fact(), [None, no, no, no]),
            (protected, shape, [None; 4]),
            (protected, protected, [None; 4]),
            (protected, Atom::Nil.fact(), [no; 4]),
            (protected, empty, [no; 4]),
        ],
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn helper_equality_depth_guards_follow_array_order_and_policy() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let one_float = facts.float(&mut ctx, 1.0).unwrap();
    let mut deep = one;
    for _ in 0..MAX_VALUE_DEPTH {
        deep = facts.tuple(&mut ctx, &[deep, deep]).unwrap();
    }
    let extra = facts.tuple(&mut ctx, &[deep]).unwrap();
    let guarded = |facts: &Facts, (value, limit): (Fact, bool)| -> (Option<Option<bool>>, bool) {
        let value = if value == Atom::Never.fact() {
            None
        } else {
            Some(decided(facts, value))
        };
        (value, limit)
    };
    for policy in [Policy::Strict, Policy::Identity] {
        let result = facts.policy_equal(&mut ctx, deep, deep, policy).unwrap();
        assert_eq!(
            guarded(&facts, result),
            (Some(Some(true)), false),
            "{policy:?}"
        );
        let result = facts.policy_equal(&mut ctx, extra, extra, policy).unwrap();
        assert_eq!(guarded(&facts, result), (None, true), "{policy:?}");
        for (left, right, expected) in [
            ([one, deep], [two, deep], (Some(Some(false)), false)),
            ([deep, one], [deep, two], (None, true)),
        ] {
            let a = facts.tuple(&mut ctx, &left).unwrap();
            let b = facts.tuple(&mut ctx, &right).unwrap();
            let result = facts.policy_equal(&mut ctx, a, b, policy).unwrap();
            assert_eq!(guarded(&facts, result), expected, "{policy:?}");
        }
    }
    // A leading numeric-kind mismatch stops the strict helper before the guard, while
    // identity compares the numbers by value and then reaches the guarded element.
    let a = facts.tuple(&mut ctx, &[one, deep]).unwrap();
    let b = facts.tuple(&mut ctx, &[one_float, deep]).unwrap();
    let result = facts.policy_equal(&mut ctx, a, b, Policy::Strict).unwrap();
    assert_eq!(guarded(&facts, result), (Some(Some(false)), false));
    let result = facts
        .policy_equal(&mut ctx, a, b, Policy::Identity)
        .unwrap();
    assert_eq!(guarded(&facts, result), (None, true));
    // Set membership keeps walking the abstract graph without a cutoff.
    for _ in 0..4000 {
        deep = facts.tuple(&mut ctx, &[deep, deep]).unwrap();
    }
    let set = facts.set_equal(&mut ctx, deep, deep).unwrap();
    assert!(matches!(facts.node(set), Node::Boolean(true)));
    for policy in [Policy::Strict, Policy::Identity] {
        let result = facts.policy_equal(&mut ctx, deep, deep, policy).unwrap();
        assert_eq!(guarded(&facts, result), (None, true), "{policy:?}");
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn helper_hash_equality_retains_unknown_insertion_order_at_depth_guards() {
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
    for policy in [Policy::Value, Policy::Strict, Policy::Identity] {
        let (value, limit) = facts.policy_equal(&mut ctx, a, b, policy).unwrap();
        assert!(limit, "{policy:?}");
        assert_eq!(decided(&facts, value), Some(false), "{policy:?}");
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
