use super::{
    facts::{Atom, Fact, Facts, Node},
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, syntax};

fn annotation(facts: &mut Facts, ctx: &mut CallContext, source: &str) -> Fact {
    let ty = syntax::parse_type(source).unwrap();
    facts.annotation(ctx, &ty, |_, _| Ok(None)).unwrap()
}

#[test]
fn facts_are_canonical_without_losing_known_arms_beside_unknowns() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let unknown = Atom::Unknown.fact();
    let int = Atom::Int.fact();
    let string = Atom::String.fact();
    let a = facts.union(&mut ctx, &[int, unknown]).unwrap();
    let b = facts
        .union(&mut ctx, &[string, a, int, Atom::Never.fact()])
        .unwrap();
    let c = facts.union(&mut ctx, &[unknown, int, string]).unwrap();
    assert_eq!(b, c);
    assert_eq!(
        facts.relation(&mut ctx, b, int).unwrap(),
        Relation::Rejected
    );
    assert_eq!(facts.union(&mut ctx, &[]).unwrap(), Atom::Never.fact());
    assert_eq!(facts.union(&mut ctx, &[int, int]).unwrap(), int);
    let known_and_any = facts.union(&mut ctx, &[Atom::Any.fact(), string]).unwrap();
    assert_eq!(
        facts.relation(&mut ctx, known_and_any, int).unwrap(),
        Relation::Rejected
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn boolean_and_symbol_facts_widen_without_duplicate_alternatives() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let yes = facts.boolean(&mut ctx, true).unwrap();
    let no = facts.boolean(&mut ctx, false).unwrap();
    let red = facts.symbol(&mut ctx, b"red").unwrap();
    assert_eq!(facts.boolean(&mut ctx, true).unwrap(), yes);
    assert_eq!(facts.symbol(&mut ctx, b"red").unwrap(), red);
    assert_eq!(
        facts.union(&mut ctx, &[yes, no]).unwrap(),
        Atom::Bool.fact()
    );
    assert_eq!(
        facts.union(&mut ctx, &[yes, Atom::Bool.fact()]).unwrap(),
        Atom::Bool.fact()
    );
    assert_eq!(
        facts.union(&mut ctx, &[red, Atom::Symbol.fact()]).unwrap(),
        Atom::Symbol.fact()
    );
    assert_eq!(
        facts.relation(&mut ctx, yes, Atom::Bool.fact()).unwrap(),
        Relation::Accepted
    );
    assert_eq!(
        facts.relation(&mut ctx, yes, no).unwrap(),
        Relation::Rejected
    );
    assert_eq!(
        facts.relation(&mut ctx, Atom::Bool.fact(), yes).unwrap(),
        Relation::Gradual
    );
}

#[test]
fn annotation_translation_preserves_nested_types_and_nominal_resolution() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = annotation(
        &mut facts,
        &mut ctx,
        "array<{ b?: int | string, a: hash<symbol, number?>, ... }>?",
    );
    let b = annotation(
        &mut facts,
        &mut ctx,
        "nil | array<{ a: hash<symbol, nil | float | int>, b?: string | int, ... }>",
    );
    assert_eq!(a, b);
    for (source, atom) in [
        ("any", Atom::Any),
        ("nil", Atom::Nil),
        ("bool", Atom::Bool),
        ("int", Atom::Int),
        ("float", Atom::Float),
        ("string", Atom::String),
        ("symbol", Atom::Symbol),
        ("duration", Atom::Duration),
        ("time", Atom::Time),
        ("money", Atom::Money),
        ("range", Atom::Range),
    ] {
        assert_eq!(annotation(&mut facts, &mut ctx, source), atom.fact());
    }
    let class = facts.nominal(&mut ctx, 7, 3, b"Widget", None).unwrap();
    let ty = syntax::parse_type("array<Widget>?").unwrap();
    let mut names = Vec::new();
    let resolved = facts
        .annotation(&mut ctx, &ty, |_, name| {
            names.push(name.to_owned());
            Ok(Some(class))
        })
        .unwrap();
    let array = facts.array(&mut ctx, class).unwrap();
    assert_eq!(
        resolved,
        facts.choice(&mut ctx, &[array, Atom::Nil.fact()]).unwrap()
    );
    assert_eq!(names, ["Widget"]);
    let unresolved = annotation(&mut facts, &mut ctx, "Missing");
    assert_eq!(
        facts.relation(&mut ctx, unresolved, class).unwrap(),
        Relation::Gradual
    );
    let Node::Nominal { name, .. } = facts.node(class) else {
        panic!()
    };
    assert_eq!(name.as_bytes(), Some(b"Widget".as_slice()));
}

#[test]
fn closed_shapes_keep_last_duplicate_keys_and_canonical_field_order() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let fields = [
        (b"b".as_slice(), Atom::Int.fact(), false),
        (b"a", Atom::Nil.fact(), true),
        (b"b", Atom::String.fact(), true),
        (b"b", Atom::Bool.fact(), false),
    ];
    let source = facts.shape(&mut ctx, &fields, false).unwrap();
    let expected = facts
        .shape(
            &mut ctx,
            &[
                (b"a", Atom::Nil.fact(), true),
                (b"b", Atom::Bool.fact(), false),
            ],
            false,
        )
        .unwrap();
    assert_eq!(source, expected);
    let Node::Shape(fields, open, _, _) = facts.node(source) else {
        panic!()
    };
    assert!(!open);
    assert_eq!(fields.data.len(), 2);
    assert_eq!(fields.data[0].name.as_bytes(), Some(b"a".as_slice()));
    assert_eq!(fields.data[1].value, Atom::Bool.fact());
}

#[test]
fn boundaries_reject_known_alternatives_and_keep_dynamic_inputs_gradual() {
    use Relation::*;
    let cases = [
        ("int", "number", Accepted),
        ("number", "int", Rejected),
        ("int | string", "int", Rejected),
        ("any | string", "int", Rejected),
        ("any | int", "int", Gradual),
        ("int?", "int", Rejected),
        ("int", "int?", Accepted),
        ("any", "array<int>", Gradual),
        ("array<any>", "array<int>", Gradual),
        ("array<int | string>", "array<int>", Rejected),
        (
            "array<int | string>",
            "array<int> | array<string>",
            Rejected,
        ),
        ("array<int>", "array<int> | array<string>", Accepted),
        ("array", "array<int>", Gradual),
        ("array<int>", "array", Accepted),
        ("hash<string, int | string>", "hash<symbol, int>", Rejected),
        ("hash<symbol, int>", "hash<string, number>", Accepted),
        ("hash<string, int>", "hash<int, int>", Rejected),
        ("hash", "hash<string, int>", Gradual),
        ("hash<string, int>", "hash", Accepted),
        ("{ count: int }", "hash", Accepted),
        ("{ count: int | string }", "{ count: int }", Rejected),
        ("{ count: int }", "{ count: number }", Accepted),
        ("{ count?: int }", "{ count: int }", Rejected),
        ("{ count: int }", "{ count?: int }", Accepted),
        ("{ count: int }", "{ count: int, extra: string }", Rejected),
        ("{ count: int }", "{ count: int, extra?: string }", Accepted),
        ("{ count: int, extra: string }", "{ count: int }", Rejected),
        (
            "{ count: int, extra: string }",
            "{ count: int, ... }",
            Accepted,
        ),
        ("{ count: int, ... }", "{ count: int }", Gradual),
        ("{ ... }", "{ count: int }", Gradual),
        ("{ count: any, extra: string }", "{ count: int }", Rejected),
        ("{ count: int }", "hash<symbol, int>", Gradual),
        ("{ count: int }", "hash<int, int>", Gradual),
        ("{ count: int | string }", "hash<string, int>", Rejected),
        ("{ count: int, ... }", "hash<string, int>", Gradual),
        ("hash<string, int>", "{ count: int }", Gradual),
        ("bool", "int", Rejected),
        ("duration", "int", Rejected),
        ("money", "number", Rejected),
        ("time", "time?", Accepted),
    ];
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for (source, target, expected) in cases {
        let source_fact = annotation(&mut facts, &mut ctx, source);
        let target_fact = annotation(&mut facts, &mut ctx, target);
        assert_eq!(
            facts.relation(&mut ctx, source_fact, target_fact).unwrap(),
            expected,
            "{source} -> {target}"
        );
    }
}

#[test]
fn empty_literal_containers_are_not_unknown_containers() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let empty = facts.tuple(&mut ctx, &[]).unwrap();
    let ints = annotation(&mut facts, &mut ctx, "array<int>");
    assert_eq!(
        facts.relation(&mut ctx, empty, ints).unwrap(),
        Relation::Accepted
    );
    let unknown = annotation(&mut facts, &mut ctx, "array");
    assert_ne!(empty, unknown);
    assert_eq!(
        facts.relation(&mut ctx, unknown, ints).unwrap(),
        Relation::Gradual
    );
    let mixed = facts
        .tuple(&mut ctx, &[Atom::Int.fact(), Atom::String.fact()])
        .unwrap();
    assert_eq!(
        facts.relation(&mut ctx, mixed, ints).unwrap(),
        Relation::Rejected
    );
    let empty_hash = facts.shape(&mut ctx, &[], false).unwrap();
    let typed_hash = annotation(&mut facts, &mut ctx, "hash<int, bool>");
    assert_eq!(
        facts.relation(&mut ctx, empty_hash, typed_hash).unwrap(),
        Relation::Rejected
    );
}

#[test]
fn nominal_facts_distinguish_sources_and_validate_exact_enum_symbols() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = facts
        .nominal(&mut ctx, 1, 0, b"Status", Some(&[b"draft", b"sent"]))
        .unwrap();
    let b = facts
        .nominal(&mut ctx, 2, 0, b"Status", Some(&[b"draft", b"sent"]))
        .unwrap();
    let c = facts.nominal(&mut ctx, 1, 1, b"Other", None).unwrap();
    assert_eq!(
        facts
            .nominal(&mut ctx, 1, 0, b"Alias", Some(&[b"draft", b"sent"]))
            .unwrap(),
        a
    );
    assert_ne!(a, b);
    assert_eq!(facts.relation(&mut ctx, a, b).unwrap(), Relation::Rejected);
    assert_eq!(facts.relation(&mut ctx, c, a).unwrap(), Relation::Rejected);
    let good = facts.symbol(&mut ctx, b"draft").unwrap();
    let bad = facts.symbol(&mut ctx, b"missing").unwrap();
    assert_eq!(
        facts.relation(&mut ctx, good, a).unwrap(),
        Relation::Accepted
    );
    assert_eq!(
        facts.relation(&mut ctx, bad, a).unwrap(),
        Relation::Rejected
    );
    assert_eq!(
        facts.relation(&mut ctx, Atom::Symbol.fact(), a).unwrap(),
        Relation::Gradual
    );
    let alternatives = facts.union(&mut ctx, &[good, bad]).unwrap();
    assert_eq!(
        facts.relation(&mut ctx, alternatives, a).unwrap(),
        Relation::Rejected
    );
}

#[test]
fn structural_union_coverage_checks_all_finite_alternatives() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let source = annotation(&mut facts, &mut ctx, "{ value: int | string }");
    let target = annotation(&mut facts, &mut ctx, "{ value: int } | { value: string }");
    assert_eq!(
        facts.relation(&mut ctx, source, target).unwrap(),
        Relation::Accepted
    );
    let element = annotation(&mut facts, &mut ctx, "int | string");
    let source = facts.tuple(&mut ctx, &[element]).unwrap();
    let target = annotation(&mut facts, &mut ctx, "array<int> | array<string>");
    assert_eq!(
        facts.relation(&mut ctx, source, target).unwrap(),
        Relation::Accepted
    );
    let source = facts.tuple(&mut ctx, &[element, element]).unwrap();
    assert_eq!(
        facts.relation(&mut ctx, source, target).unwrap(),
        Relation::Rejected
    );
    for (source, target, expected) in [
        (
            "{ x: int | string, y: int | string }",
            "{ x: int, y: int } | { x: string, y: string }",
            Relation::Rejected,
        ),
        (
            "{ x?: int }",
            "{ x: int } | { y: string }",
            Relation::Rejected,
        ),
        ("{ x?: int }", "{ ... } | { x: int }", Relation::Accepted),
        (
            "{ x: { y: int | string } }",
            "{ x: { y: int } } | { x: { y: string } }",
            Relation::Accepted,
        ),
        (
            "{ x: { y: int | string }, z: bool }",
            "{ x: { y: int }, z: bool } | { x: { y: string }, z: bool }",
            Relation::Accepted,
        ),
    ] {
        let source = annotation(&mut facts, &mut ctx, source);
        let target = annotation(&mut facts, &mut ctx, target);
        assert_eq!(facts.relation(&mut ctx, source, target).unwrap(), expected);
    }
}

#[test]
fn a_missing_required_field_ends_optional_field_expansion() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let names: Vec<_> = (0..40).map(|i| format!("field{i}")).collect();
    let optional: Vec<_> = names
        .iter()
        .map(|name| (name.as_bytes(), Atom::Int.fact(), true))
        .collect();
    let required: Vec<_> = names
        .iter()
        .map(|name| (name.as_bytes(), Atom::Int.fact(), false))
        .collect();
    let source = facts.shape(&mut ctx, &optional, false).unwrap();
    let required = facts.shape(&mut ctx, &required, false).unwrap();
    let target = facts
        .union(&mut ctx, &[required, Atom::String.fact()])
        .unwrap();
    let before = ctx.stats().steps;
    assert_eq!(
        facts.relation(&mut ctx, source, target).unwrap(),
        Relation::Rejected
    );
    assert!(ctx.stats().steps - before < 4_000);
}

#[test]
fn deep_shared_fact_graphs_compare_and_drop_on_the_default_stack() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut a = Atom::Int.fact();
    let mut b = Atom::String.fact();
    for _ in 0..10_000 {
        a = facts.array(&mut ctx, a).unwrap();
        b = facts.array(&mut ctx, b).unwrap();
    }
    assert_eq!(facts.relation(&mut ctx, a, b).unwrap(), Relation::Rejected);
    assert_eq!(facts.relation(&mut ctx, a, a).unwrap(), Relation::Accepted);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn relation_memoization_charges_unique_pairs_in_shared_dags() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut a = Atom::Int.fact();
    let mut b = Atom::String.fact();
    for _ in 0..80 {
        a = facts
            .shape(&mut ctx, &[(b"x", a, false), (b"y", a, false)], false)
            .unwrap();
        b = facts
            .shape(&mut ctx, &[(b"x", b, false), (b"y", b, false)], false)
            .unwrap();
    }
    let before = ctx.stats();
    assert_eq!(facts.relation(&mut ctx, a, b).unwrap(), Relation::Rejected);
    assert!(ctx.stats().steps - before.steps < 10_000);
    assert_eq!(
        ctx.stats().retained_memory_bytes,
        before.retained_memory_bytes
    );
}

fn accounting_work(ctx: &mut CallContext) -> Result<usize> {
    let mut facts = Facts::new(ctx)?;
    let mut array = Atom::Int.fact();
    for _ in 0..60 {
        array = facts.array(ctx, array)?;
    }
    let wide = facts.union(ctx, &[array, Atom::Any.fact(), Atom::String.fact()])?;
    assert_eq!(facts.relation(ctx, wide, array)?, Relation::Rejected);
    let source = syntax::parse_type("array<{ x?: int, y: string | nil, ... }>").unwrap();
    facts.annotation(ctx, &source, |_, _| Ok(None))?;
    let source = syntax::parse_type("{ x: int | string, y?: bool }").unwrap();
    let target = syntax::parse_type("{ x: int, y?: bool } | { x: string, y?: bool }").unwrap();
    let source = facts.annotation(ctx, &source, |_, _| Ok(None))?;
    let target = facts.annotation(ctx, &target, |_, _| Ok(None))?;
    assert_eq!(facts.relation(ctx, source, target)?, Relation::Accepted);
    facts.symbol(ctx, &vec![b'x'; 12_000])?;
    Ok(facts.len())
}

#[test]
fn checker_fact_work_honors_exact_memory_and_step_limits() {
    let mut ctx = CallContext::new(CallOptions::default());
    let count = accounting_work(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    assert!(count > 60);
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
        let result = accounting_work(&mut ctx);
        assert_eq!(result.as_ref().err().map(|e| e.kind), error);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn failed_fact_allocations_release_all_owned_buffers() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting_work(&mut ctx).unwrap();
    for limit in (0..ctx.stats().peak_memory_bytes).step_by(251) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(limit),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            accounting_work(&mut ctx).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0, "limit {limit}");
    }
}

#[test]
fn cancellation_and_deadlines_are_observed_on_cached_fact_paths() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let fact = facts.array(&mut ctx, Atom::Int.fact()).unwrap();
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
        assert_eq!(
            facts.array(&mut ctx, Atom::Int.fact()).unwrap_err().kind,
            expected
        );
        assert_eq!(facts.union(&mut ctx, &[fact]).unwrap_err().kind, expected);
        assert_eq!(
            facts.relation(&mut ctx, fact, fact).unwrap_err().kind,
            expected
        );
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn boundary_decisions_match_the_pinned_go_reference() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-boundaries.json")).unwrap();
    let mut differences = Vec::new();
    let cases = fixtures["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 1156);
    for case in cases {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let source = case["source"].as_str().unwrap();
        let target = case["target"].as_str().unwrap();
        let source_fact = annotation(&mut facts, &mut ctx, source);
        let target_fact = annotation(&mut facts, &mut ctx, target);
        let relation = facts.relation(&mut ctx, source_fact, target_fact).unwrap();
        let rejected = case["rejected"].as_bool().unwrap();
        if (relation == Relation::Rejected) != rejected {
            differences.push(format!(
                "{source} -> {target}: {relation:?}; Go rejected={rejected}"
            ));
        }
    }
    assert!(
        differences.is_empty(),
        "{} differences of {} cases: {}",
        differences.len(),
        cases.len(),
        differences[..differences.len().min(40)].join("\n")
    );
}
