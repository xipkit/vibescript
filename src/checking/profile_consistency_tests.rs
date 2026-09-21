//! Cross-field consistency of the protected match profiles contracts admit.
//!
//! Every named capture value is one of the captures array's elements, and a
//! match without a single group has an empty named map. A contract that
//! refines the two fields into a pair no runtime match can carry must not
//! keep its rescue arm reachable. These tests pin that pruning against the
//! runtime boundary and guard the real profiles that survive it.

use super::{
    collection_tests::analyze,
    facts::{Atom, Certainty, Facts, HashKind, Node},
    flow::IssueKind,
    normalization_tests,
};
use crate::{CallContext, CallOptions, Engine, Value, bytecode, hash::Tag, syntax};

const RETURN_VIOLATION: &str = "return value for run expected int, got string";

fn runtime(source: &str, input: &Value) -> std::result::Result<String, String> {
    Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", std::slice::from_ref(input), CallOptions::default())
        .map(|result| result.value.to_string())
        .map_err(|error| error.to_string().lines().next().unwrap().to_owned())
}

fn produced(body: &str) -> Value {
    Engine::new()
        .compile(&format!("def make; {body}; end"))
        .unwrap()
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value
}

fn matched(subject: &str, pattern: &str) -> Value {
    produced(&format!("{subject:?}.match({pattern:?})"))
}

fn entry(name: &[u8], value: Value) -> Value {
    Value::hash(vec![(name.to_vec(), value)])
}

fn fields(captures: Value, named: Value) -> Value {
    Value::hash(vec![
        (b"captures".to_vec(), captures),
        (b"named_captures".to_vec(), named),
    ])
}

/// Checks `run` for its declared parameter types: the report must be complete
/// and its only diagnostics, if any, return-contract violations reached
/// through a rescued protected mutation.
fn general(source: &str, rescued_return: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    let returns = report
        .issues
        .data
        .iter()
        .filter(|located| matches!(located.issue.kind, IssueKind::Return { .. }))
        .count();
    assert_eq!(returns, report.issues.data.len(), "{source}: {report:?}");
    assert_eq!(returns > 0, rescued_return, "{source}: {report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn annotation(ctx: &mut CallContext, facts: &mut Facts, source: &str) -> super::facts::Fact {
    let ty = syntax::parse_type(source).unwrap();
    facts.annotation(ctx, &ty, |_, _| Ok(None)).unwrap()
}

/// Impossible capture/named pairs: the runtime rejects every match object at
/// the boundary, so a rescued write through such a contract must not report
/// a return the runtime can never produce.
#[test]
fn impossible_capture_named_pairs_cannot_reach_their_rescue_arm() {
    for (source, plain) in [
        // Zero captures cannot carry a required named field.
        (
            "def run(h:{captures:array<int>,named_captures:{x:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            fields(Value::array(vec![]), entry(b"x", Value::bytes(b"s"))),
        ),
        // Non-nullable captures cannot hold a nil named value.
        (
            "def run(h:{captures:array<string>,named_captures:{x:nil},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            fields(
                Value::array(vec![Value::bytes(b"a")]),
                entry(b"x", Value::nil()),
            ),
        ),
        // Nil-only captures cannot hold a string named value.
        (
            "def run(h:{captures:array<nil>,named_captures:{x:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            fields(
                Value::array(vec![Value::nil()]),
                entry(b"x", Value::bytes(b"s")),
            ),
        ),
        // The union of an empty-only and a nil-only array arm is still
        // incompatible with a required string named value.
        (
            "def run(h:{captures:array<int>|array<nil>,named_captures:{x:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            fields(
                Value::array(vec![Value::nil()]),
                entry(b"x", Value::bytes(b"s")),
            ),
        ),
    ] {
        // The plain hash passes the boundary and its write succeeds, so the
        // rescue arm never runs.
        assert_eq!(runtime(source, &plain).as_deref(), Ok("0"), "{source}");
        // No match object satisfies both fields, so the runtime rejects
        // every match input at the argument boundary.
        for input in [
            matched("a", "a"),
            matched("a", "(a)"),
            matched("a", "(?<x>a)"),
            matched("", "(?<x>a)?"),
        ] {
            assert!(
                runtime(source, &input)
                    .unwrap_err()
                    .starts_with("argument h expected"),
                "{source}"
            );
        }
        general(source, false);
    }
}

/// The match bit and the admitted variant survive exactly the contracts
/// whose captures and named fields a runtime match can carry together.
#[test]
fn capture_named_consistency_prunes_only_impossible_match_profiles() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for (source, admitted) in [
        ("{captures:array<int>,named_captures:{x:string},...}", false),
        ("{captures:array<string>,named_captures:{x:nil},...}", false),
        ("{captures:array<nil>,named_captures:{x:string},...}", false),
        (
            "{captures:array<int>|array<nil>,named_captures:{x:string},...}",
            false,
        ),
        (
            "{captures:array<string?>,named_captures:{x:string},...}",
            true,
        ),
        ("{captures:array<string?>,named_captures:{x:nil},...}", true),
        ("{captures:array<nil>,named_captures:{x:nil},...}", true),
        (
            "{captures:array<string>,named_captures:{x:string},...}",
            true,
        ),
        (
            "{captures:array<int>|array<string>,named_captures:{x:string},...}",
            true,
        ),
        ("{named_captures:{x:string},...}", true),
        ("{captures:array<int>,...}", true),
        ("{captures:array<int>,named_captures:{x?:int},...}", true),
        ("{captures:array<int>,named_captures:hash,...}", true),
        (
            "{captures:array<int>,named_captures:hash<string,int>,...}",
            true,
        ),
    ] {
        let contract = annotation(&mut ctx, &mut facts, source);
        let kind = facts.hash_mode(contract);
        assert_eq!(kind.has(HashKind::MATCH), admitted, "{source}: {kind:?}");
        // None of these contracts admit a rescued error, and the pass must
        // not touch the error provenance.
        assert!(!kind.has(HashKind::ERROR), "{source}: {kind:?}");
        let variant = facts
            .protected_variant(&mut ctx, contract, Tag::Match)
            .unwrap();
        assert_eq!(variant != Atom::Never.fact(), admitted, "{source}");
        if admitted {
            assert!(
                matches!(
                    facts.node(variant),
                    Node::Protected(_, Tag::Match, Certainty::Contract)
                ),
                "{source}: {:?}",
                facts.node(variant)
            );
        }
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

/// Real profiles keep their readonly rescue path: the runtime returns the
/// rescued string and the checker reports the return violation.
#[test]
fn compatible_profiles_keep_their_rescued_readonly_path() {
    for (source, input) in [
        (
            "def run(h:{captures:array<string?>,named_captures:{x:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "(?<x>a)"),
        ),
        (
            "def run(h:{captures:array<string?>,named_captures:{x:nil},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("", "(?<x>a)?"),
        ),
        (
            "def run(h:{captures:array<nil>,named_captures:{x:nil},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("", "(?<x>a)?"),
        ),
        (
            "def run(h:{captures:array<nil>,named_captures:{x?:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{captures:array<string>,named_captures:{x?:nil},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{captures:array<string>,named_captures:{x:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "(?<x>a)"),
        ),
        (
            "def run(h:{captures:array<int>|array<string>,named_captures:{x:string},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "(?<x>a)"),
        ),
        (
            "def run(h:{captures:array<int>,named_captures:hash,...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{captures:array<int>,named_captures:{x?:int},...})->int;begin;h[:y]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
    ] {
        assert_eq!(
            runtime(source, &input),
            Err(RETURN_VIOLATION.to_owned()),
            "{source}"
        );
        general(source, true);
    }
}

/// A required named field dies with incompatible captures while an optional
/// one survives as absent, refining the named map to the empty map.
#[test]
fn optional_named_fields_survive_as_absent_and_required_ones_prune() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for (source, admitted) in [
        ("{captures:array<nil>,named_captures:{x:string},...}", false),
        ("{captures:array<nil>,named_captures:{x?:string},...}", true),
        ("{captures:array<string>,named_captures:{x:nil},...}", false),
        ("{captures:array<string>,named_captures:{x?:nil},...}", true),
        ("{captures:array<int>,named_captures:{x:string},...}", false),
        ("{captures:array<int>,named_captures:{x?:string},...}", true),
    ] {
        let contract = annotation(&mut ctx, &mut facts, source);
        let variant = facts
            .protected_variant(&mut ctx, contract, Tag::Match)
            .unwrap();
        assert_eq!(variant != Atom::Never.fact(), admitted, "{source}");
        if !admitted {
            continue;
        }
        let Node::Protected(shape, Tag::Match, Certainty::Contract) = facts.node(variant) else {
            panic!("{source}: {:?}", facts.node(variant));
        };
        let (named, _) = facts
            .selected_field(&mut ctx, *shape, b"named_captures")
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                facts.node(named),
                Node::Shape(fields, false, ..) if fields.data.is_empty()
            ),
            "{source}: {:?}",
            facts.node(named)
        );
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

/// A zero-capture match has an empty named map, so rescued reads of the
/// named captures stay precise instead of inventing a reachable `else`.
#[test]
fn empty_captures_refine_the_named_map_for_rescued_reads() {
    for source in [
        "def run(h:{captures:array<int>,named_captures:hash,...})->int;begin;h[:y]=1;0;rescue RuntimeError;if h.named_captures.empty?;0;else;'bad';end;end;end",
        "def run(h:{captures:array<int>,named_captures:hash<string,string?>,...})->int;begin;h[:y]=1;0;rescue RuntimeError;if h.named_captures.empty?;0;else;'bad';end;end;end",
    ] {
        // The only matches that pass have no named captures, and plain
        // hashes never reach the rescue, so `else` is unreachable.
        assert_eq!(
            runtime(source, &matched("a", "a")).as_deref(),
            Ok("0"),
            "{source}"
        );
        assert_eq!(
            runtime(source, &fields(Value::array(vec![]), Value::hash(vec![]))).as_deref(),
            Ok("0"),
            "{source}"
        );
        assert!(
            runtime(source, &matched("a", "(?<x>a)"))
                .unwrap_err()
                .starts_with("argument h expected"),
            "{source}"
        );
        general(source, false);
    }
    // The admitted variant's named map is refined to the runtime empty map.
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let contract = annotation(
        &mut ctx,
        &mut facts,
        "{captures:array<int>,named_captures:hash,...}",
    );
    let variant = facts
        .protected_variant(&mut ctx, contract, Tag::Match)
        .unwrap();
    assert_ne!(variant, Atom::Never.fact());
    let Node::Protected(shape, ..) = facts.node(variant) else {
        panic!("{:?}", facts.node(variant));
    };
    let (named, _) = facts
        .selected_field(&mut ctx, *shape, b"named_captures")
        .unwrap()
        .unwrap();
    assert!(matches!(
        facts.node(named),
        Node::Shape(fields, false, ..) if fields.data.is_empty()
    ));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

/// A resolved nominal|int key union rejects the profile's string keys, so
/// only the empty named map passes and rescued reads stay precise.
#[test]
fn nominal_key_unions_admit_only_the_empty_named_map() {
    let source = "class Key;end;def run(h:{named_captures:hash<Key|int,any>,...})->int;begin;h[:y]=1;0;rescue RuntimeError;if h.named_captures.empty?;0;else;'bad';end;end;end";
    // Matches without named groups pass; named groups are rejected because
    // their string keys satisfy neither union arm.
    assert_eq!(runtime(source, &matched("a", "a")).as_deref(), Ok("0"));
    assert_eq!(runtime(source, &matched("a", "(a)")).as_deref(), Ok("0"));
    assert!(
        runtime(source, &matched("a", "(?<x>a)"))
            .unwrap_err()
            .starts_with("argument h expected")
    );
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = normalization_tests::analyze(&mut ctx, &mut facts, &program).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

/// The consistency pass keeps the memo valid: repeated dispatch returns the
/// same refined fact, including for pruned contracts.
#[test]
fn consistent_alternatives_are_memoized_with_contract_certainty() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for (source, admitted) in [
        (
            "{captures:array<string?>,named_captures:{x:string},...}",
            true,
        ),
        ("{captures:array<int>,named_captures:{x:string},...}", false),
    ] {
        let contract = annotation(&mut ctx, &mut facts, source);
        let variant = facts
            .protected_variant(&mut ctx, contract, Tag::Match)
            .unwrap();
        assert_eq!(variant != Atom::Never.fact(), admitted, "{source}");
        let before = ctx.stats().steps;
        assert_eq!(
            facts
                .protected_variant(&mut ctx, contract, Tag::Match)
                .unwrap(),
            variant,
            "{source}"
        );
        assert!(ctx.stats().steps - before < 16, "{source}");
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
