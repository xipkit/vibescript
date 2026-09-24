use super::{
    collection_tests::{literal_fact, literal_values},
    facts::{Atom, Fact, Facts, Node},
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, ErrorClass, ErrorKind, Limits, Value,
    bytecode::{CallSite, Method},
    value::Kind,
};

fn exact_fact(ctx: &mut CallContext, facts: &mut Facts, value: &Value) -> Fact {
    if let Kind::Range(range) = &value.0 {
        return facts
            .range(ctx, range.start, range.end, range.exclusive)
            .unwrap();
    }
    if let Kind::Float(number) = value.0 {
        return facts.float(ctx, number).unwrap();
    }
    literal_fact(ctx, facts, value)
}

/// Replaces one literal level with its general contract: tuples become arrays
/// of their joined elements, shapes become hashes and ranges lose their bounds.
fn general(ctx: &mut CallContext, facts: &mut Facts, value: Fact) -> Fact {
    match facts.node(value) {
        Node::Tuple(_) => {
            let element = facts.elements(ctx, value).unwrap();
            facts.array(ctx, element).unwrap()
        }
        Node::Shape(..) => {
            let values = facts.shape_values(ctx, value, false).unwrap();
            let keys = Atom::String.fact();
            facts.hash_kind(ctx, keys, values, true).unwrap()
        }
        Node::Range(..) => Atom::Range.fact(),
        _ => value,
    }
}

fn receivers() -> Vec<Value> {
    let ints = |values: &[i64]| Value::array(values.iter().copied().map(Value::int).collect());
    let mut values = literal_values();
    values.extend([
        Value::array(vec![ints(&[1, 2]), ints(&[3, 4])]),
        Value::array(vec![ints(&[1]), ints(&[2, 3])]),
        Value::array(vec![
            Value::array(vec![Value::int(1), Value::array(vec![ints(&[2])])]),
            Value::int(4),
        ]),
        Value::array(vec![ints(&[]), ints(&[])]),
        Value::array(vec![Value::nil(), Value::int(1), Value::nil()]),
        ints(&[1, 2, 2, 3]),
        Value::array(vec![Value::float(1.0), Value::int(2), Value::bytes("a")]),
        Value::hash(vec![
            (b"a".to_vec(), Value::nil()),
            (b"b".to_vec(), Value::int(1)),
            (b"c".to_vec(), Value::array(vec![Value::nil()])),
        ]),
        Value::range(Some(1), Some(3), false),
        Value::range(Some(3), Some(1), false),
        Value::range(Some(1), Some(1), true),
        Value::range(Some(-2), Some(2), true),
        Value::range(Some(1), None, false),
    ]);
    values
}

#[test]
fn inferred_array_set_operators_contain_runtime_results() {
    let mut cases = 0;
    let values = receivers();
    for left in &values {
        for right in &values {
            for op in ["-", "&"] {
                for generalize in [false, true] {
                    let label = format!("{left:?} {op} {right:?}, general={generalize}");
                    let mut ctx = CallContext::new(CallOptions::default());
                    let mut facts = Facts::new(&mut ctx).unwrap();
                    let mut a = exact_fact(&mut ctx, &mut facts, left);
                    let mut b = exact_fact(&mut ctx, &mut facts, right);
                    if generalize {
                        a = general(&mut ctx, &mut facts, a);
                        b = general(&mut ctx, &mut facts, b);
                    }
                    let (inferred, _) = facts.scalar_binary(&mut ctx, op, a, b).unwrap();
                    cases += 1;
                    if inferred.unsupported {
                        assert!(
                            !matches!(facts.node(a), Node::Array(_) | Node::Tuple(_)),
                            "{label}"
                        );
                        continue;
                    }
                    let mut runtime = CallContext::new(CallOptions::default());
                    let x = runtime.import(left).unwrap();
                    let y = runtime.import(right).unwrap();
                    match crate::ops::binary(&mut runtime, op, x, y) {
                        Ok(actual) => {
                            assert!(!inferred.rejected, "{label}: rejected {actual:?}");
                            let actual = exact_fact(&mut ctx, &mut facts, &actual);
                            assert_ne!(
                                facts.relation(&mut ctx, actual, inferred.value).unwrap(),
                                Relation::Rejected,
                                "{label}: inferred={:?}, actual={:?}",
                                facts.node(inferred.value),
                                facts.node(actual)
                            );
                        }
                        Err(error) => assert!(
                            inferred.rejected || inferred.value == Atom::Unknown.fact(),
                            "{label}: {error} was not predicted"
                        ),
                    }
                }
            }
        }
    }
    assert_eq!(cases, 2_500);
}

fn argument_lists() -> Vec<Vec<Value>> {
    let ints = |values: &[i64]| Value::array(values.iter().copied().map(Value::int).collect());
    vec![
        vec![],
        vec![Value::bytes(",")],
        vec![Value::symbol("sep")],
        vec![Value::nil()],
        vec![Value::int(0)],
        vec![Value::int(1)],
        vec![Value::int(-1)],
        vec![Value::int(3)],
        vec![Value::float(1.5)],
        vec![ints(&[1, 2])],
        vec![ints(&[])],
        vec![ints(&[1, 2]), Value::array(vec![Value::bytes("a")])],
        vec![Value::int(0), Value::int(-1)],
        vec![Value::int(0), Value::int(5)],
        vec![Value::symbol("a"), Value::bytes("b")],
        vec![Value::symbol("x"), Value::symbol("x")],
        vec![Value::bytes("size")],
        vec![Value::range(Some(0), Some(1), false)],
        vec![Value::range(Some(1), Some(5), true)],
        vec![Value::range(Some(-2), Some(-1), false)],
        vec![Value::range(Some(-9), Some(0), false)],
        vec![Value::range(Some(1), None, false)],
        vec![Value::hash(vec![])],
    ]
}

/// Compares each member's summary with execution for every literal receiver
/// and argument list, exactly and with the receiver generalized one level.
/// Returns the number of cases and how many of them the summary models.
fn member_contracts(names: &[&str]) -> (usize, usize) {
    let mut cases = 0;
    let mut modeled = 0;
    for receiver in receivers() {
        for &name in names {
            for values in argument_lists() {
                for generalize in [false, true] {
                    let label = format!("{receiver:?}.{name}({values:?}), general={generalize}");
                    let mut ctx = CallContext::new(CallOptions::default());
                    let mut facts = Facts::new(&mut ctx).unwrap();
                    let mut root = exact_fact(&mut ctx, &mut facts, &receiver);
                    if generalize {
                        root = general(&mut ctx, &mut facts, root);
                    }
                    let args: Vec<_> = values
                        .iter()
                        .map(|value| exact_fact(&mut ctx, &mut facts, value))
                        .collect();
                    let site = CallSite {
                        name: 0,
                        method: Method::parse(name),
                        auto: values.is_empty(),
                        parenthesized: !values.is_empty(),
                        scope: false,
                    };
                    let inferred = facts
                        .collection_member(&mut ctx, root, site, name, &args)
                        .unwrap_or_else(|error| panic!("{label}: {error}"));
                    cases += 1;
                    if inferred.unsupported {
                        continue;
                    }
                    modeled += 1;
                    let mut runtime = CallContext::new(CallOptions::default());
                    let actual_receiver = runtime.import(&receiver).unwrap();
                    let actual_args: Vec<_> = values
                        .iter()
                        .map(|value| runtime.import(value).unwrap())
                        .collect();
                    match crate::members::call(
                        &mut runtime,
                        site,
                        name,
                        actual_receiver,
                        &actual_args,
                    ) {
                        Ok((_, actual)) => {
                            assert!(!inferred.rejected, "{label}: rejected {actual:?}");
                            assert_ne!(inferred.value, Atom::Never.fact(), "{label}: no result");
                            let actual = exact_fact(&mut ctx, &mut facts, &actual);
                            assert_ne!(
                                facts.relation(&mut ctx, actual, inferred.value).unwrap(),
                                Relation::Rejected,
                                "{label}: inferred={:?}, actual={:?}",
                                facts.node(inferred.value),
                                facts.node(actual)
                            );
                        }
                        // Depth guards are reported by the walker, not the summary.
                        Err(error) if error.class() == Some(ErrorClass::Limit) => (),
                        Err(error) => assert!(
                            inferred.rejected || inferred.throws,
                            "{label}: {error} was not predicted: {:?}",
                            facts.node(inferred.value)
                        ),
                    }
                    drop((inferred, facts));
                    assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
                }
            }
        }
    }
    (cases, modeled)
}

#[test]
fn nested_walk_members_contain_runtime_results() {
    assert_eq!(
        member_contracts(&["join", "inspect", "flatten"]),
        (3_450, 1_656)
    );
}

#[test]
fn regrouping_members_contain_runtime_results() {
    assert_eq!(
        member_contracts(&["zip", "transpose", "window"]),
        (3_450, 1_656)
    );
}

#[test]
fn projection_members_contain_runtime_results() {
    let names = ["values_at", "slice", "except", "compact", "to_a"];
    assert_eq!(member_contracts(&names), (5_750, 2_760));
}

#[test]
fn keyed_lookup_and_set_members_contain_runtime_results() {
    let names = [
        "dig",
        "value?",
        "remap_keys",
        "flatten",
        "union",
        "difference",
        "to_s",
        "byteslice",
    ];
    assert_eq!(member_contracts(&names), (9_200, 3_312));
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run -> string
      rows = [[1, 2], [3, 4]].transpose.zip([[5], [6]]).flatten(1)
      picked = {a: 1, b: nil, c: 3}.compact.slice(:a, :c).except(:c)
      (rows - [1]).values_at(0, -1, 0..1).join(',') + picked.inspect + (1..3).to_a.join
    end";
    let result = super::collection_tests::analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
    assert_eq!(result.returns, Atom::String.fact());
    Ok(())
}

#[test]
fn projection_analysis_obeys_exact_limits_and_releases_failed_storage() {
    let mut baseline = CallContext::new(CallOptions::default());
    accounting(&mut baseline).unwrap();
    let stats = baseline.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
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
    for memory in (0..stats.peak_memory_bytes).step_by(97) {
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
fn projection_walks_observe_latched_cancellation_and_deadlines() {
    let site = |name| CallSite {
        name: 0,
        method: Method::parse(name),
        auto: false,
        parenthesized: true,
        scope: false,
    };
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let one = facts.integer(&mut ctx, 1).unwrap();
        let row = facts.tuple(&mut ctx, &[one, one]).unwrap();
        let rows = facts.tuple(&mut ctx, &[row, row]).unwrap();
        let comma = facts.string(&mut ctx, b",").unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        let expected = if deadline {
            ErrorKind::Deadline
        } else {
            ErrorKind::Cancelled
        };
        for (name, args) in [
            ("join", vec![comma]),
            ("transpose", vec![]),
            ("zip", vec![rows]),
            ("flatten", vec![]),
            ("values_at", vec![one]),
        ] {
            let error = facts
                .collection_member(&mut ctx, rows, site(name), name, &args)
                .err()
                .unwrap();
            assert_eq!(error.kind, expected, "{name}");
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        let error = facts
            .scalar_binary(&mut ctx, "-", rows, rows)
            .err()
            .unwrap();
        assert_eq!(error.kind, expected);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
