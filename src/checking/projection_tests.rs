use super::{
    collection_tests::{literal_fact, literal_values},
    facts::{Atom, Fact, Facts, Node},
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Value, value::Kind};

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

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run -> int
      kept = [1, 2, 3, 2] - [2]
      (kept & [3, 1, 3]).length
    end";
    let result = super::collection_tests::analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
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
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let one = facts.integer(&mut ctx, 1).unwrap();
        let row = facts.tuple(&mut ctx, &[one, one]).unwrap();
        let rows = facts.tuple(&mut ctx, &[row, row]).unwrap();
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
        let error = facts
            .scalar_binary(&mut ctx, "-", rows, rows)
            .err()
            .unwrap();
        assert_eq!(error.kind, expected);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
