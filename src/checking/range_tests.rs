use super::{
    arguments::Arguments,
    builtins,
    collection_tests::literal_fact,
    facts::{Atom, Fact, Facts, Node},
    integers::Bounds,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, ErrorKind, Limits, Value, arguments,
    bytecode::{CallSite, Method},
    value::Kind,
};

const MAX: i64 = i64::MAX;
const MIN: i64 = i64::MIN;

fn exact_fact(ctx: &mut CallContext, facts: &mut Facts, value: &Value) -> Fact {
    match value.0 {
        Kind::Range(ref range) => facts
            .range(ctx, range.start, range.end, range.exclusive)
            .unwrap(),
        Kind::Float(number) => facts.float(ctx, number).unwrap(),
        _ => literal_fact(ctx, facts, value),
    }
}

fn receivers() -> Vec<Value> {
    [
        (Some(1), Some(3), false),
        (Some(1), Some(3), true),
        (Some(3), Some(1), false),
        (Some(3), Some(1), true),
        (Some(1), Some(1), true),
        (Some(-2), Some(2), false),
        (Some(1), None, false),
        (None, Some(3), true),
        (None, None, false),
        (Some(MAX - 1), None, false),
        (Some(MIN), Some(MAX), false),
        (Some(MIN), Some(MAX - 1), true),
        (Some(0), Some(300), false),
    ]
    .into_iter()
    .map(|(start, end, exclusive)| Value::range(start, end, exclusive))
    .collect()
}

fn argument_lists() -> Vec<Vec<Value>> {
    vec![
        vec![],
        vec![Value::int(0)],
        vec![Value::int(2)],
        vec![Value::int(-1)],
        vec![Value::int(3)],
        vec![Value::int(MAX)],
        vec![Value::int(MIN)],
        vec![Value::float(2.0)],
        vec![Value::float(2.5)],
        vec![Value::float(-0.5)],
        vec![Value::float(f64::NAN)],
        vec![Value::float(1e300)],
        vec![Value::bytes("x")],
        vec![Value::nil()],
        vec![Value::array(vec![])],
        vec![Value::int(1), Value::int(2)],
    ]
}

/// Generalizes one fact: literal ranges lose their endpoints, and literal
/// integers keep bounds that still describe them or become general.
fn general(ctx: &mut CallContext, facts: &mut Facts, value: Fact, bounded: bool) -> Fact {
    match *facts.node(value) {
        Node::Range(..) => Atom::Range.fact(),
        Node::Integer(n) if bounded => facts
            .integer_range(
                ctx,
                Bounds {
                    min: Some(n.saturating_sub(1).min(n)),
                    max: Some(n.saturating_add(2)),
                },
            )
            .unwrap(),
        Node::Integer(_) => Atom::Int.fact(),
        Node::Float(_) => Atom::Float.fact(),
        _ => value,
    }
}

#[test]
fn range_member_contracts_contain_runtime_results() {
    let names = [
        "first",
        "last",
        "size",
        "length",
        "to_a",
        "include?",
        "cover?",
        "member?",
        "exclude_end?",
    ];
    let mut cases = 0;
    for receiver in receivers() {
        for name in names {
            for values in argument_lists() {
                for keywords in [false, true] {
                    for mode in 0..4 {
                        let label = format!(
                            "{receiver:?}.{name}({values:?}), keywords={keywords}, mode={mode}"
                        );
                        let mut ctx = CallContext::new(CallOptions::default());
                        let mut facts = Facts::new(&mut ctx).unwrap();
                        let mut root = exact_fact(&mut ctx, &mut facts, &receiver);
                        if mode & 1 != 0 {
                            root = general(&mut ctx, &mut facts, root, false);
                        }
                        let mut inputs = Arguments::new();
                        let mut runtime = CallContext::new(CallOptions::default());
                        let mut actual = arguments::Arguments::empty();
                        for value in &values {
                            let fact = exact_fact(&mut ctx, &mut facts, value);
                            let fact = if mode & 2 != 0 {
                                general(&mut ctx, &mut facts, fact, mode & 1 == 0)
                            } else {
                                fact
                            };
                            inputs.positional.push(&mut ctx, fact).unwrap();
                            let value = runtime.import(value).unwrap();
                            actual.positional.push(&mut runtime, value).unwrap();
                        }
                        if keywords {
                            let key = facts.symbol(&mut ctx, b"k").unwrap();
                            let yes = facts.boolean(&mut ctx, true).unwrap();
                            inputs.keyword(&mut ctx, key, yes).unwrap();
                            let key = runtime.bytes(b"k").unwrap();
                            actual
                                .keywords
                                .insert(&mut runtime, key, Value::boolean(true))
                                .unwrap();
                        }
                        let site = CallSite {
                            name: 0,
                            method: Method::parse(name),
                            auto: values.is_empty(),
                            parenthesized: !values.is_empty(),
                            scope: false,
                        };
                        let inferred =
                            builtins::member(&mut ctx, &mut facts, root, site, name, &inputs)
                                .unwrap_or_else(|error| panic!("{label}: {error}"))
                                .unwrap_or_else(|| panic!("{label}: not a range member"));
                        assert!(!inferred.incomplete, "{label}");
                        let value = runtime.import(&receiver).unwrap();
                        match crate::members::call_keywords(
                            &mut runtime,
                            site,
                            name,
                            value,
                            &actual,
                        ) {
                            Ok((_, value)) => {
                                assert!(inferred.failures.data.is_empty(), "{label}: {value:?}");
                                let concrete = exact_fact(&mut ctx, &mut facts, &value);
                                assert_ne!(
                                    facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                                    Relation::Rejected,
                                    "{label}: inferred={:?}, actual={value:?}",
                                    facts.node(inferred.value)
                                );
                            }
                            // Exhausting the runtime's own budget is not a script outcome.
                            Err(error)
                                if matches!(error.kind, ErrorKind::Steps | ErrorKind::Memory) => {}
                            Err(error) => {
                                let class = error.class().unwrap();
                                assert_ne!(
                                    inferred.throws & (1 << class as u8),
                                    0,
                                    "{label}: {error} was not predicted"
                                );
                                // Literal inputs that always fail are contradictions,
                                // apart from the size guard's limit error.
                                if mode == 0 && class != crate::ErrorClass::Limit {
                                    assert!(!inferred.failures.data.is_empty(), "{label}: {error}");
                                    assert_eq!(inferred.value, Atom::Never.fact(), "{label}");
                                }
                            }
                        }
                        drop((inputs, inferred, facts, actual));
                        assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
                        assert_eq!(runtime.stats().retained_memory_bytes, 0, "{label}");
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 14_976);
}

#[test]
fn literal_ranges_fold_endpoints_lengths_and_membership() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let site = |name| CallSite {
        name: 0,
        method: Method::parse(name),
        auto: false,
        parenthesized: true,
        scope: false,
    };
    for (range, name, argument, expected) in [
        ((Some(1), Some(3), true), "last", None, "3"),
        ((Some(1), Some(1), true), "first", None, "1"),
        ((Some(3), Some(1), true), "size", None, "2"),
        ((Some(1), Some(3), true), "exclude_end?", None, "true"),
        ((Some(1), Some(3), false), "first", Some(2), "[1, 2]"),
        ((Some(3), Some(1), false), "last", Some(2), "[2, 1]"),
        (
            (Some(MAX - 1), None, false),
            "first",
            Some(9),
            "[9223372036854775806, 9223372036854775807]",
        ),
        ((Some(1), Some(3), true), "include?", Some(3), "false"),
        ((Some(3), Some(1), true), "include?", Some(1), "false"),
        ((None, Some(3), false), "cover?", Some(MIN), "true"),
    ] {
        let receiver = facts.range(&mut ctx, range.0, range.1, range.2).unwrap();
        let mut inputs = Arguments::new();
        if let Some(argument) = argument {
            let argument = facts.integer(&mut ctx, argument).unwrap();
            inputs.positional.push(&mut ctx, argument).unwrap();
        }
        let inferred = builtins::member(&mut ctx, &mut facts, receiver, site(name), name, &inputs)
            .unwrap()
            .unwrap();
        assert_eq!(inferred.throws, 0, "{range:?}.{name}");
        let actual = crate::members::call(
            &mut CallContext::new(CallOptions::default()),
            site(name),
            name,
            Value::range(range.0, range.1, range.2),
            &argument.map(Value::int).into_iter().collect::<Vec<_>>(),
        )
        .unwrap()
        .1;
        assert_eq!(actual.to_string(), expected, "{range:?}.{name}");
        let concrete = exact_fact(&mut ctx, &mut facts, &actual);
        // Exact facts are singletons, so the actual value is the inferred fact itself.
        assert_eq!(
            concrete,
            inferred.value,
            "{range:?}.{name}: {:?}",
            facts.node(inferred.value)
        );
    }
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(n: int) -> int
      r = (1.5..300.9)
      window = r.first(2) + r.last(n) + (-3...0).to_a
      (r.include?(n) ? r.size : r.first) + window.length + (r.exclude_end? ? 1 : 0)
    end";
    let result = super::collection_tests::analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty(), "{result:?}");
    assert!(result.issues.data.is_empty(), "{result:?}");
    Ok(())
}

#[test]
fn range_member_analysis_obeys_exact_limits_and_releases_failed_storage() {
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
fn range_members_observe_latched_cancellation_and_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let receiver = facts.range(&mut ctx, Some(1), Some(9), false).unwrap();
        let count = facts.integer(&mut ctx, 3).unwrap();
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
            ("first", vec![count]),
            ("size", vec![]),
            ("cover?", vec![count]),
        ] {
            let error = facts
                .range_member(&mut ctx, receiver, name, &args)
                .err()
                .unwrap();
            assert_eq!(error.kind, expected, "{name}");
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
