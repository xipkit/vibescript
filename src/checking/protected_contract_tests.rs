//! Structural contracts that may admit protected match or error objects.
//!
//! A `hash` or shape parameter without a concrete value can be a plain hash,
//! a host object, a match object or a rescued error at runtime. The checker
//! expands only the protected profiles the contract actually admits, keeps
//! their mutation failures as ordinary runtime paths, and never reports them
//! as static contradictions.

use super::{
    addresses::{Address, Attached},
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Certainty, Facts, HashKind, Node},
    flow::IssueKind,
    native_tests::witness,
    objects,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Value, bytecode, hash::Tag, syntax,
};

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

fn rescued_error() -> Value {
    produced("begin; raise 'bad'; rescue => e; e; end")
}

fn entry(name: &[u8], value: Value) -> Value {
    Value::hash(vec![(name.to_vec(), value)])
}

const RETURN_VIOLATION: &str = "return value for run expected int, got string";

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

/// Checks that every runtime outcome of `run` is admitted by a clean, complete
/// general report: the possibly protected receiver keeps both continuations.
fn both_paths(source: &str, cases: &[(Value, &str)]) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert!(report.issues.data.is_empty(), "{source}: {report:?}");
    for (input, expected) in cases {
        assert_eq!(runtime(source, input).as_deref(), Ok(*expected), "{source}");
        let actual = Engine::new()
            .compile(source)
            .unwrap()
            .call("run", std::slice::from_ref(input), CallOptions::default())
            .unwrap()
            .value;
        let concrete = literal_fact(&mut ctx, &mut facts, &actual);
        assert_ne!(
            facts.relation(&mut ctx, concrete, report.returns).unwrap(),
            Relation::Rejected,
            "{source}: {report:?}"
        );
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn annotation(ctx: &mut CallContext, facts: &mut Facts, source: &str) -> super::facts::Fact {
    let ty = syntax::parse_type(source).unwrap();
    facts.annotation(ctx, &ty, |_, _| Ok(None)).unwrap()
}

#[test]
fn contracts_admitting_protected_objects_keep_their_rescued_return_paths() {
    let error = rescued_error();
    for (source, input) in [
        (
            "def run(h:{captures:array<int>,...})->int;begin;h.captures.push(1);0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{captures:array<string>,...})->int;begin;h.captures.push(\"x\");0;rescue RuntimeError;'bad';end;end",
            matched("a", "(a)"),
        ),
        (
            "def run(h:hash<string,string|array<string>>)->int;begin;h.clear;0;rescue RuntimeError;'bad';end;end",
            error.clone(),
        ),
        (
            "def run(h:hash)->int;begin;h.clear;0;rescue RuntimeError;'bad';end;end",
            error,
        ),
        (
            "def run(h:hash)->int;begin;h.clear;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{captures:array<string?>,...})->int;begin;h[:captures].push(begin;h={};\"x\";end);0;rescue RuntimeError;'bad';end;end",
            matched("a", "(a)"),
        ),
        (
            "def run(h:{begin:any,captures:array<string?>,end:any,named_captures:hash,post_match:string,pre_match:string,to_s:string,extra?:int})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "(a)"),
        ),
        (
            "def run(h:hash<symbol,any>)->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "(a)"),
        ),
        (
            "def run(h:{named_captures:{x?:int},...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{to_s:string,...})->int;begin;h.store(:x,1);0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
    ] {
        assert_eq!(
            runtime(source, &input).as_deref().map_err(String::as_str),
            Err(RETURN_VIOLATION),
            "{source}"
        );
        general(source, true);
    }
    // The match profile lacks the string-or-array value contract, so only the
    // error object reaches the rejected clear; the match is refused earlier.
    assert!(
        runtime(
            "def run(h:hash<string,string|array<string>>)->int;begin;h.clear;0;rescue RuntimeError;'bad';end;end",
            &matched("a", "a"),
        )
        .unwrap_err()
        .starts_with("argument h expected")
    );
}

#[test]
fn contracts_excluding_protected_objects_stay_clean_and_complete() {
    for (source, input) in [
        (
            "def run(h:{captures:int,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            entry(b"captures", Value::int(1)),
        ),
        (
            "def run(h:{captures:array<string?>})->int;begin;h.clear;0;rescue RuntimeError;'bad';end;end",
            entry(b"captures", Value::array(vec![Value::bytes(b"a")])),
        ),
        (
            "def run(h:{begin:int,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            entry(b"begin", Value::int(1)),
        ),
        (
            "def run(h:{named_captures:array,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            entry(b"named_captures", Value::array(vec![])),
        ),
        (
            "def run(h:{named_captures:{x:int},...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            entry(b"named_captures", entry(b"x", Value::int(1))),
        ),
        (
            "def run(h:hash<string,int>)->int;begin;h['x']=1;0;rescue RuntimeError;'bad';end;end",
            Value::hash(vec![]),
        ),
        // Child extraction detaches the ancestor: the copied captures array is
        // mutable even when the parent was a match object.
        (
            "def run(h:{captures:array<string>,...})->int;begin;c=h.captures;c.push(\"x\");0;rescue RuntimeError;'bad';end;end",
            matched("a", "(a)"),
        ),
    ] {
        assert_eq!(runtime(source, &input).as_deref(), Ok("0"), "{source}");
        general(source, false);
    }
    for (source, input) in [
        (
            "def run(h:{captures:int,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:{begin:int,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            matched("a", "a"),
        ),
        (
            "def run(h:hash<string,int>)->int;begin;h['x']=1;0;rescue RuntimeError;'bad';end;end",
            rescued_error(),
        ),
    ] {
        assert!(
            runtime(source, &input)
                .unwrap_err()
                .starts_with("argument h expected"),
            "{source}"
        );
    }
}

#[test]
fn unsplit_possibly_protected_receivers_keep_both_continuations() {
    both_paths(
        "def run(h:hash<symbol,any>)->int;begin;h[:x]=1;1;rescue RuntimeError;2;end;end",
        &[(Value::hash(vec![]), "1"), (matched("a", "(a)"), "2")],
    );
    both_paths(
        "def run(h:{captures:array<string?>,...})->int;begin;h.captures.push(nil);1;rescue RuntimeError;2;end;end",
        &[
            (entry(b"captures", Value::array(vec![])), "1"),
            (matched("a", "(a)"), "2"),
        ],
    );
    both_paths(
        "def run(h:hash)->int;begin;h.clear;1;rescue RuntimeError;2;end;end",
        &[
            (Value::hash(vec![]), "1"),
            (matched("a", "a"), "2"),
            (rescued_error(), "2"),
        ],
    );
}

#[test]
fn resolved_class_fields_exclude_protected_objects_while_unresolved_names_stay_gradual() {
    let source = "class C;end;def make;{to_s:C.new};end;def run(h:{to_s:C,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end";
    let script = Engine::new().compile(source).unwrap();
    let input = script
        .call("make", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(
        script
            .call("run", &[input], CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "0"
    );
    assert!(
        runtime(source, &matched("a", "a"))
            .unwrap_err()
            .starts_with("argument h expected")
    );
    // With the class declaration resolved, a string field can never satisfy
    // the nominal contract, so no protected object is admitted and the rescue
    // is unreachable.
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = super::normalization_tests::analyze(&mut ctx, &mut facts, &program).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    // Without a resolver the same name is an unresolved gradual node: the
    // contract keeps admitting the match profile, and the general check stops
    // at the argument boundary with a possible runtime throw instead of a
    // static diagnostic.
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let contract = annotation(&mut ctx, &mut facts, "{to_s:C,...}");
    assert!(facts.unresolved(contract));
    assert!(facts.hash_mode(contract).has(HashKind::MATCH));
    assert_ne!(
        facts
            .protected_variant(&mut ctx, contract, Tag::Match)
            .unwrap(),
        Atom::Never.fact()
    );
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert_ne!(report.throws, 0, "{report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn gradual_named_capture_contracts_admit_empty_maps_and_reject_named_strings() {
    witness(
        "def read(h:{named_captures:{x?:int},...})->int;0;end;def run;m=/a/.match(\"a\");if m;read(m);else;9;end;end",
        Some("0"),
        false,
    );
    witness(
        "def read(h:{named_captures:{x?:int},...})->int;0;end;def run;m=/(?<x>a)/.match(\"a\");if m;begin;read(m);rescue;7;end;else;9;end;end",
        Some("7"),
        true,
    );
}

#[test]
fn contract_pruning_follows_the_runtime_boundary() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for (source, matches, errors) in [
        ("hash", true, true),
        ("hash<string,any>", true, true),
        ("hash<symbol,any>", true, true),
        ("hash<int,string>", false, false),
        ("hash<string,string|array<string>>", false, true),
        ("hash<string,int>", false, false),
        ("{captures:array<string?>,...}", true, false),
        ("{captures:array<string>,...}", true, false),
        ("{captures:array<int>,...}", true, false),
        ("{captures:int,...}", false, false),
        ("{captures?:int,...}", false, true),
        ("{captures:array<string?>}", false, false),
        ("{begin:int,...}", false, false),
        ("{begin:any,...}", true, false),
        ("{named_captures:array,...}", false, false),
        ("{named_captures:{x?:int},...}", true, false),
        ("{named_captures:{x:int},...}", false, false),
        ("{named_captures:{x:string},...}", true, false),
        ("{backtrace:array<string>,...}", false, true),
        ("{backtrace:array<int>,...}", false, true),
        ("{to_s:string,...}", true, true),
        ("{to_s:int,...}", false, false),
        ("{extra:int,...}", false, false),
        ("{extra?:int,...}", true, true),
        (
            "{begin:any,captures:array<string?>,end:any,named_captures:hash,post_match:string,pre_match:string,to_s:string,extra?:int}",
            true,
            false,
        ),
        (
            "{type:string,class:string,message:string,to_s:string,code_frame:string,backtrace:array<string>}",
            false,
            true,
        ),
    ] {
        let contract = annotation(&mut ctx, &mut facts, source);
        let kind = facts.hash_mode(contract);
        assert!(kind.has(HashKind::PLAIN.join(HashKind::OBJECT)), "{source}");
        assert_eq!(kind.has(HashKind::MATCH), matches, "{source}: {kind:?}");
        assert_eq!(kind.has(HashKind::ERROR), errors, "{source}: {kind:?}");
        for (tag, admitted) in [(Tag::Match, matches), (Tag::Error, errors)] {
            let variant = facts.protected_variant(&mut ctx, contract, tag).unwrap();
            assert_eq!(variant != Atom::Never.fact(), admitted, "{source}");
        }
    }
    // Nested contracts are pruned on their own before the outer one.
    let contract = annotation(&mut ctx, &mut facts, "array<{captures:int,...}>");
    let Node::Array(element) = facts.node(contract) else {
        panic!("{:?}", facts.node(contract));
    };
    assert!(!facts.hash_mode(*element).tagged());
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn admitted_profiles_are_refined_to_the_contract_domain() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let site = crate::bytecode::CallSite {
        name: 0,
        method: None,
        auto: true,
        parenthesized: false,
        scope: false,
    };
    let field = |facts: &Facts, ctx: &mut CallContext, variant: super::facts::Fact, name: &[u8]| {
        let shape = match facts.node(variant) {
            Node::Protected(shape, Tag::Match, Certainty::Contract) => *shape,
            other => panic!("{other:?}"),
        };
        facts.selected_field(ctx, shape, name).unwrap().unwrap().0
    };
    // Only a zero-capture match satisfies `captures:array<int>`.
    let contract = annotation(&mut ctx, &mut facts, "{captures:array<int>,...}");
    let variant = facts
        .protected_variant(&mut ctx, contract, Tag::Match)
        .unwrap();
    let captures = field(&facts, &mut ctx, variant, b"captures");
    assert_eq!(captures, facts.tuple(&mut ctx, &[]).unwrap());
    let begin = field(&facts, &mut ctx, variant, b"begin");
    assert!(matches!(facts.node(begin), Node::Offset(_)));
    assert_eq!(
        field(&facts, &mut ctx, variant, b"to_s"),
        Atom::String.fact()
    );
    // Only an empty named-capture map satisfies `{x?:int}`, while a required
    // string field keeps the capture and its possible absence is refined away.
    for (source, empty) in [
        ("{named_captures:{x?:int},...}", true),
        ("{named_captures:{x:string},...}", false),
    ] {
        let contract = annotation(&mut ctx, &mut facts, source);
        let variant = facts
            .protected_variant(&mut ctx, contract, Tag::Match)
            .unwrap();
        let named = field(&facts, &mut ctx, variant, b"named_captures");
        let Node::Shape(fields, false, ..) = facts.node(named) else {
            panic!("{source}: {:?}", facts.node(named));
        };
        assert_eq!(fields.data.is_empty(), empty, "{source}");
        let result = facts
            .collection_member(&mut ctx, named, site, "empty?", &[])
            .unwrap();
        assert_eq!(result.value, facts.boolean(&mut ctx, empty).unwrap());
    }
    // A nominal key contract admits only the empty map: the profile's string
    // keys can never satisfy it, so the key domain collapses.
    let key = facts.nominal(&mut ctx, 0, 0, b"Key", None).unwrap();
    let named = facts
        .hash_kind(&mut ctx, key, Atom::Any.fact(), false)
        .unwrap();
    let declared = facts
        .shape(&mut ctx, &[(b"named_captures", named, false)], true)
        .unwrap();
    let contract = facts.hash_as(&mut ctx, declared, HashKind::ANY).unwrap();
    let pruned = facts.pruned_contract(&mut ctx, contract).unwrap();
    assert_eq!(
        facts.hash_mode(pruned),
        HashKind::PLAIN.join(HashKind::OBJECT).join(HashKind::MATCH)
    );
    let variant = facts
        .protected_variant(&mut ctx, pruned, Tag::Match)
        .unwrap();
    let named = field(&facts, &mut ctx, variant, b"named_captures");
    assert!(matches!(
        facts.node(named),
        Node::Shape(fields, false, keys, _) if fields.data.is_empty() && *keys == Atom::Never.fact()
    ));
    // A value contract that accepts every profile field leaves the profile
    // intact, and repeated lookups are memoized.
    let contract = annotation(&mut ctx, &mut facts, "hash<symbol,any>");
    let variant = facts
        .protected_variant(&mut ctx, contract, Tag::Match)
        .unwrap();
    let profile = facts.tag_profile(&mut ctx, Tag::Match).unwrap();
    assert_eq!(
        variant,
        facts
            .protected_as(&mut ctx, profile, Tag::Match, Certainty::Contract)
            .unwrap()
    );
    let before = ctx.stats().steps;
    assert_eq!(
        facts
            .protected_variant(&mut ctx, contract, Tag::Match)
            .unwrap(),
        variant
    );
    assert_eq!(facts.tag_profile(&mut ctx, Tag::Match).unwrap(), profile);
    assert!(ctx.stats().steps - before < 16);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn contract_alternatives_are_read_only_without_being_reportable() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let contract = annotation(&mut ctx, &mut facts, "{captures:array<string?>,...}");
    let key = facts.symbol(&mut ctx, b"captures").unwrap();
    // An unsplit receiver is possibly read-only and its child inherits that.
    let mut unsplit = Address::new(Some(0), contract);
    let protection = unsplit.protection(&mut ctx, &facts).unwrap();
    assert_eq!(
        (protection.readonly, protection.report),
        (Attached::Maybe, false)
    );
    let result = unsplit.index(&mut ctx, &mut facts, &[key]).unwrap();
    assert!(!result.rejected && !result.unsupported);
    let protection = unsplit.protection(&mut ctx, &facts).unwrap();
    assert_eq!(
        (protection.readonly, protection.report),
        (Attached::Maybe, false)
    );
    // A materialized contract alternative is certainly read-only but not a
    // static contradiction, on the receiver and through its children.
    let variant = facts
        .protected_variant(&mut ctx, contract, Tag::Match)
        .unwrap();
    let mut admitted = Address::new(None, variant);
    let protection = admitted.protection(&mut ctx, &facts).unwrap();
    assert_eq!(
        (protection.readonly, protection.report),
        (Attached::Yes, false)
    );
    admitted.index(&mut ctx, &mut facts, &[key]).unwrap();
    let protection = admitted.protection(&mut ctx, &facts).unwrap();
    assert_eq!(
        (protection.readonly, protection.report),
        (Attached::Yes, false)
    );
    // A known protected object keeps its reportable diagnostic.
    let shape = match facts.node(variant) {
        Node::Protected(shape, ..) => *shape,
        _ => unreachable!(),
    };
    let known = facts.protected(&mut ctx, shape, Tag::Match).unwrap();
    let protection = Address::new(None, known)
        .protection(&mut ctx, &facts)
        .unwrap();
    assert_eq!(
        (protection.readonly, protection.report),
        (Attached::Yes, true)
    );
    // A duplicate of the contract alternative re-derives certainty from its
    // node, so `dup`-style copies keep the runtime rejection.
    let copy = Address::new(None, variant);
    assert_eq!(
        copy.protection(&mut ctx, &facts).unwrap().readonly,
        Attached::Yes
    );
    // Mutation dispatch splits the contract into its provenances, and the
    // protected arm dispatches through the protected summaries only.
    let variants = objects::variants(&mut ctx, &mut facts, contract, "clear", false)
        .unwrap()
        .unwrap();
    assert_eq!(variants.data.len(), 3);
    assert!(facts.plain_hash(variants.data[0]));
    assert!(facts.hash_mode(variants.data[1]).object());
    assert_eq!(variants.data[2], variant);
    let site = crate::bytecode::CallSite {
        name: 0,
        method: crate::bytecode::Method::parse("clear"),
        auto: true,
        parenthesized: false,
        scope: false,
    };
    assert!(
        objects::select(&mut ctx, &facts, variant, site, "clear")
            .unwrap()
            .is_none()
    );
    // Reads of declared fields do not split, while the offset fields do
    // because a bare read fails on the match alternative.
    assert!(
        objects::variants(&mut ctx, &mut facts, contract, "captures", false)
            .unwrap()
            .is_none()
    );
    let offsets = annotation(&mut ctx, &mut facts, "{begin:any,...}");
    assert_eq!(
        objects::variants(&mut ctx, &mut facts, offsets, "begin", false)
            .unwrap()
            .unwrap()
            .data
            .len(),
        3
    );
    // The contract arm's member failures are runtime paths, never rejections.
    let mut arguments = super::arguments::Arguments::new();
    let read = super::builtins::protected::member(
        &mut ctx,
        &mut facts,
        variant,
        crate::bytecode::CallSite {
            method: None,
            ..site
        },
        "begin",
        &arguments,
    )
    .unwrap();
    assert!(read.failures.data.is_empty() && read.throws != 0);
    let mutate = super::builtins::protected::member(
        &mut ctx, &mut facts, variant, site, "clear", &arguments,
    )
    .unwrap();
    assert!(mutate.failures.data.is_empty() && mutate.throws != 0);
    arguments
        .positional
        .push(&mut ctx, Atom::Int.fact())
        .unwrap();
    let rejected = super::builtins::protected::member(
        &mut ctx,
        &mut facts,
        known,
        crate::bytecode::CallSite {
            method: crate::bytecode::Method::parse("store"),
            ..site
        },
        "store",
        &arguments,
    )
    .unwrap();
    assert!(!rejected.failures.data.is_empty());
    // The indexed addresses hold metered path hops and must be released too.
    drop((unsplit, admitted, copy));
    drop((variants, arguments, read, mutate, rejected, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

/// Rescue paths reached only through a protected write see the admitted
/// protected alternatives of the receiver, while other error edges keep the
/// full contract domain.
#[test]
fn rescued_protected_writes_narrow_the_receiver_to_admitted_alternatives() {
    // `{x?:int}` admits only an empty named-capture map, so the rescued
    // inspection can never find `x`.
    for source in [
        "def run(h:{named_captures:{x?:int},...})->int;begin;h[:x]=1;0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end",
        "def run(h:{named_captures:{x?:int},...})->int;begin;h[:x]=1;0;rescue RuntimeError;if h.named_captures.empty?;0;else;'bad';end;end;end",
        "def run(h:{begin?:any,end?:any,captures?:any,named_captures:{x?:int},post_match?:string,pre_match?:string,to_s?:string})->int;begin;h.store(:x,1);0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end",
    ] {
        assert_eq!(
            runtime(source, &matched("a", "a")).as_deref(),
            Ok("0"),
            "{source}"
        );
        assert_eq!(
            runtime(source, &entry(b"named_captures", Value::hash(vec![]))).as_deref(),
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
    // An open object can override `store`, so its independent failure must
    // retain the ordinary receiver's named fields in the rescue.
    let source = "def run(h:{named_captures:{x?:int},...})->int;begin;h.store(:x,1);0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end";
    let colliding = Value::object(vec![
        (b"store".to_vec(), Value::int(1)),
        (b"named_captures".to_vec(), entry(b"x", Value::int(1))),
    ]);
    assert!(
        runtime(source, &colliding)
            .unwrap_err()
            .contains(RETURN_VIOLATION)
    );
    general(source, true);
    // A resolved nominal key contract admits only the empty map as well.
    let source = "class Key;end;def run(h:{named_captures:hash<Key,any>,...})->int;begin;h[:x]=1;0;rescue RuntimeError;if h.named_captures.empty?;0;else;'bad';end;end;end";
    assert_eq!(runtime(source, &matched("a", "a")).as_deref(), Ok("0"));
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = super::normalization_tests::analyze(&mut ctx, &mut facts, &program).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    // An ordinary raise reaches the same rescue with the plain receiver, and a
    // rescued inspection that the contract domain genuinely allows to vary
    // keeps both continuations.
    both_paths(
        "def run(h:{named_captures:{x?:int},...})->int;begin;h[:x]=1;raise 'x';rescue RuntimeError;h.named_captures.size;end;end",
        &[
            (matched("a", "a"), "0"),
            (entry(b"named_captures", Value::hash(vec![])), "0"),
            (entry(b"named_captures", entry(b"x", Value::int(1))), "1"),
        ],
    );
}

#[test]
fn nested_protected_write_errors_refine_the_stored_ancestor() {
    for operation in ["h[:named_captures][:x]=1", "h.named_captures[:x]=1"] {
        let source = format!(
            "def run(h:{{named_captures:{{x?:int}},...}})->int;begin;{operation};0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end"
        );
        assert_eq!(runtime(&source, &matched("a", "a")).as_deref(), Ok("0"));
        assert_eq!(
            runtime(
                &source,
                &entry(b"named_captures", entry(b"x", Value::int(2)))
            )
            .as_deref(),
            Ok("0")
        );
        general(&source, false);
    }
    let source = "def run(h:{inner:{named_captures:{x?:int},...}})->int;begin;h[:inner][:x]=1;0;rescue RuntimeError;if h.inner.named_captures.key?(:x);'bad';else;0;end;end;end";
    assert_eq!(
        runtime(source, &entry(b"inner", matched("a", "a"))).as_deref(),
        Ok("0")
    );
    assert_eq!(
        runtime(
            source,
            &entry(
                b"inner",
                entry(b"named_captures", entry(b"x", Value::int(2)))
            )
        )
        .as_deref(),
        Ok("0")
    );
    general(source, false);
}

#[test]
fn detached_protected_errors_preserve_the_reassigned_binding() {
    for operation in [
        "h.store(:x,(begin;h={named_captures:{x:1}};1;end))",
        "h.named_captures.store(:x,(begin;h={named_captures:{x:1}};1;end))",
    ] {
        let source = format!(
            "def run(h:{{named_captures:{{x?:int}},...}})->int;begin;{operation};0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end"
        );
        assert!(
            runtime(&source, &matched("a", "a"))
                .unwrap_err()
                .contains(RETURN_VIOLATION)
        );
        general(&source, true);
    }
}

#[test]
fn protected_mutating_blocks_refine_only_the_readonly_error() {
    let source = "def run(h:{begin?:any,end?:any,captures?:any,named_captures:{x?:int},post_match?:string,pre_match?:string,to_s?:string})->int;begin;h.delete_if {|k,v| true};0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end";
    assert_eq!(runtime(source, &matched("a", "a")).as_deref(), Ok("0"));
    assert_eq!(
        runtime(
            source,
            &entry(b"named_captures", entry(b"x", Value::int(2)))
        )
        .as_deref(),
        Ok("0")
    );
    general(source, false);
}

#[test]
fn enum_contracts_exclude_string_metadata_without_implicit_conversion() {
    let source = "enum Letter;A;end;def plain;{to_s:Letter::A};end;def run(h:{to_s:Letter,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end";
    assert!(
        runtime(source, &matched("A", "A"))
            .unwrap_err()
            .starts_with("argument h expected")
    );
    let script = Engine::new().compile(source).unwrap();
    let input = script
        .call("plain", &[], CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(
        script
            .call("run", &[input], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(0)
    );
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = super::normalization_tests::analyze(&mut ctx, &mut facts, &program).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn forbidden_optional_named_captures_stay_absent_in_open_shapes() {
    for contract in [
        "{named_captures:{x?:int,...},...}",
        "{captures:array<string>,named_captures:{x?:nil,...},...}",
    ] {
        let source = format!(
            "def run(h:{contract})->int;begin;h[:y]=1;0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end"
        );
        assert_eq!(runtime(&source, &matched("a", "(a)")).as_deref(), Ok("0"));
        assert!(
            runtime(&source, &matched("a", "(?<x>a)"))
                .unwrap_err()
                .starts_with("argument h expected")
        );
        general(&source, false);
    }
}

#[test]
fn error_refinement_updates_pending_protected_projections_without_detaching_them() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let contract = annotation(&mut ctx, &mut facts, "{named_captures:{x?:int},...}");
    let root = facts.tuple(&mut ctx, &[contract]).unwrap();
    let zero = facts.integer(&mut ctx, 0).unwrap();
    let named = facts.symbol(&mut ctx, b"named_captures").unwrap();
    let mut selected = Address::new(Some(0), root);
    selected.index(&mut ctx, &mut facts, &[zero]).unwrap();
    let narrowed = selected
        .protected_root(&mut ctx, &mut facts)
        .unwrap()
        .unwrap();
    let mut pending = Address::new(Some(0), root);
    pending.index(&mut ctx, &mut facts, &[zero]).unwrap();
    pending.index(&mut ctx, &mut facts, &[named]).unwrap();
    pending
        .refresh(
            &mut ctx,
            &mut facts,
            narrowed,
            &super::addresses::Change::Refine,
        )
        .unwrap();
    assert_eq!(pending.attached, Attached::Yes);
    let protection = pending.protection(&mut ctx, &facts).unwrap();
    assert_eq!(
        (protection.readonly, protection.report),
        (Attached::Yes, false)
    );
    assert!(
        matches!(facts.node(pending.value), Node::Shape(fields, false, _, _) if fields.data.is_empty())
    );
    drop((selected, pending, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn captured_protection_constraints_reach_an_outer_rescue() {
    let source = "def run(h:{named_captures:{x?:int},...})->int;begin;[1].each {|_| h[:y]=1};0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end";
    assert_eq!(runtime(source, &matched("a", "a")).as_deref(), Ok("0"));
    assert_eq!(
        runtime(
            source,
            &entry(b"named_captures", entry(b"x", Value::int(1)))
        )
        .as_deref(),
        Ok("0")
    );
    general(source, false);
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    for (source, rescued_return) in [
        (
            "def run(h:{captures:array<int>,...})->int;begin;h.captures.push(1);0;rescue RuntimeError;'bad';end;end",
            true,
        ),
        (
            "def run(h:hash<symbol,any>)->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            true,
        ),
        (
            "def run(h:{captures:int,...})->int;begin;h[:x]=1;0;rescue RuntimeError;'bad';end;end",
            false,
        ),
        (
            "def run(h:{inner:{named_captures:{x?:int},...}})->int;begin;h[:inner][:y]=1;0;rescue RuntimeError;if h.inner.named_captures.key?(:x);'bad';else;0;end;end;end",
            false,
        ),
        (
            "def run(h:{captures:array<string>,named_captures:{x:nil},...})->int;begin;[1].each {|_| h[:y]=1};0;rescue RuntimeError;'bad';end;end",
            false,
        ),
        (
            "def run(h:{named_captures:{x?:int},...})->int;begin;[1].each {|_| h[:y]=1};0;rescue RuntimeError;if h.named_captures.key?(:x);'bad';else;0;end;end;end",
            false,
        ),
    ] {
        let mut facts = Facts::new(ctx)?;
        let report = analyze(ctx, &mut facts, source)?;
        assert!(report.incomplete.data.is_empty(), "{report:?}");
        assert_eq!(!report.issues.data.is_empty(), rescued_return, "{report:?}");
    }
    Ok(())
}

#[test]
fn contract_expansion_obeys_exact_quotas_and_reclaims_failures() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
    let stats = ctx.stats();
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
    for memory in (0..stats.peak_memory_bytes).step_by(509) {
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
    for steps in (0..stats.steps).step_by(509) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(steps),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(accounting(&mut ctx).unwrap_err().kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn contract_expansion_preserves_latched_cancellation_and_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        accounting(&mut ctx).unwrap();
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
