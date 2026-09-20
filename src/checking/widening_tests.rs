use super::{
    collection_tests::analyze,
    facts::{Atom, Facts, HashKind, Node},
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, ErrorKind, Limits, Result, budget::Buffer, bytecode::Method,
};

#[test]
fn growing_collections_converge_without_losing_known_scalar_alternatives() {
    for recursive in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut value = facts.tuple(&mut ctx, &[]).unwrap();
        let int = facts.integer(&mut ctx, 7).unwrap();
        let bad = facts.string(&mut ctx, b"bad").unwrap();
        let unknown = facts.union(&mut ctx, &[Atom::Unknown.fact(), bad]).unwrap();
        let mut depth = None;
        let mut stable = false;
        for _ in 0..12 {
            let appended = if recursive { value } else { int };
            let incoming = facts
                .collection_mutate(&mut ctx, value, Method::Push, &[appended, unknown])
                .unwrap()
                .receiver;
            let bound = *depth.get_or_insert_with(|| facts.max_depth());
            let next = facts.widen(&mut ctx, value, incoming, bound).unwrap();
            assert_ne!(
                facts.relation(&mut ctx, incoming, next).unwrap(),
                Relation::Rejected
            );
            if next == value {
                stable = true;
                break;
            }
            value = next;
        }
        assert!(stable, "recursive={recursive}: {:?}", facts.node(value));
        let ints = facts.array(&mut ctx, Atom::Int.fact()).unwrap();
        assert_eq!(
            facts.relation(&mut ctx, value, ints).unwrap(),
            Relation::Rejected
        );
        let mixed = facts
            .union(&mut ctx, &[Atom::Unknown.fact(), Atom::String.fact()])
            .unwrap();
        let widened = facts
            .widen(&mut ctx, Atom::Unknown.fact(), mixed, 0)
            .unwrap();
        assert_eq!(
            facts.relation(&mut ctx, widened, Atom::Int.fact()).unwrap(),
            Relation::Rejected
        );
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn stable_tuple_positions_and_literal_keys_survive_loop_joins() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let text = facts.string(&mut ctx, b"text").unwrap();
    let a = facts.tuple(&mut ctx, &[one, text]).unwrap();
    let b = facts.tuple(&mut ctx, &[two, text]).unwrap();
    let joined = facts.widen(&mut ctx, a, b, 1).unwrap();
    let first = facts
        .integer_range(
            &mut ctx,
            super::integers::Bounds {
                min: Some(1),
                max: Some(2),
            },
        )
        .unwrap();
    assert_eq!(joined, facts.tuple(&mut ctx, &[first, text]).unwrap());
    let key = facts.string(&mut ctx, b"key").unwrap();
    let other = facts.string(&mut ctx, b"other").unwrap();
    assert_eq!(
        facts.widen(&mut ctx, key, other, 0).unwrap(),
        facts.union(&mut ctx, &[key, other]).unwrap()
    );
    assert_eq!(facts.widen(&mut ctx, a, a, 0).unwrap(), a);
}

#[test]
fn hash_joins_preserve_optional_fields_and_plain_provenance() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = facts
        .shape(&mut ctx, &[(b"x", Atom::Int.fact(), false)], false)
        .unwrap();
    let b = facts
        .shape(
            &mut ctx,
            &[
                (b"x", Atom::String.fact(), false),
                (b"y", Atom::Bool.fact(), false),
            ],
            false,
        )
        .unwrap();
    let joined = facts.widen(&mut ctx, a, b, 1).unwrap();
    let x = facts
        .union(&mut ctx, &[Atom::Int.fact(), Atom::String.fact()])
        .unwrap();
    assert_eq!(
        joined,
        facts
            .shape(
                &mut ctx,
                &[(b"x", x, false), (b"y", Atom::Bool.fact(), true)],
                false
            )
            .unwrap()
    );
    assert!(facts.plain_hash(joined));
    let open = facts.shape(&mut ctx, &[], true).unwrap();
    let joined = facts.widen(&mut ctx, a, open, 1).unwrap();
    let x = facts
        .union(&mut ctx, &[Atom::Int.fact(), Atom::Unknown.fact()])
        .unwrap();
    assert_eq!(
        joined,
        facts.shape(&mut ctx, &[(b"x", x, true)], true).unwrap()
    );
    let general = facts
        .hash(&mut ctx, Atom::String.fact(), Atom::Bool.fact())
        .unwrap();
    let joined = facts.widen(&mut ctx, a, general, 1).unwrap();
    assert!(!facts.plain_hash(joined));
    assert!(matches!(
        facts.node(joined),
        Node::Hash(_, _, HashKind::ANY)
    ));
    // Joins unite provenance sets: plain with object stays untagged, and only
    // a structural contract (ANY) contributes protected possibilities.
    let untagged = HashKind::PLAIN.join(HashKind::OBJECT);
    assert!(!untagged.tagged() && !untagged.single());
    for (left, right, expected) in [
        (HashKind::PLAIN, HashKind::PLAIN, HashKind::PLAIN),
        (HashKind::OBJECT, HashKind::OBJECT, HashKind::OBJECT),
        (HashKind::PLAIN, HashKind::OBJECT, untagged),
        (HashKind::OBJECT, HashKind::PLAIN, untagged),
        (untagged, HashKind::PLAIN, untagged),
        (HashKind::OBJECT, untagged, untagged),
        (HashKind::PLAIN, HashKind::ANY, HashKind::ANY),
        (HashKind::ANY, HashKind::OBJECT, HashKind::ANY),
        (untagged, HashKind::ANY, HashKind::ANY),
        (HashKind::ANY, HashKind::ANY, HashKind::ANY),
    ] {
        let a = facts
            .hash_kind(&mut ctx, Atom::String.fact(), Atom::Int.fact(), left)
            .unwrap();
        let b = facts
            .hash_kind(&mut ctx, Atom::String.fact(), Atom::Bool.fact(), right)
            .unwrap();
        let joined = facts.widen(&mut ctx, a, b, 1).unwrap();
        assert_eq!(facts.hash_mode(joined), expected);
        let a = facts
            .shape_fields(&mut ctx, Buffer::empty(), false, Atom::String.fact(), left)
            .unwrap();
        let b = facts
            .shape_fields(&mut ctx, Buffer::empty(), false, Atom::String.fact(), right)
            .unwrap();
        let joined = facts.widen(&mut ctx, a, b, 1).unwrap();
        assert_eq!(facts.hash_mode(joined), expected);
    }
}

#[test]
fn plain_and_object_joins_never_admit_protected_dispatch() {
    use super::objects::{self, Selection};
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    // The fields a protected match object also carries, so a structural
    // contract with them stays conservatively guarded.
    let plain = facts
        .shape(
            &mut ctx,
            &[
                (b"to_s", Atom::String.fact(), false),
                (b"pre_match", Atom::String.fact(), false),
                (b"post_match", Atom::String.fact(), false),
            ],
            true,
        )
        .unwrap();
    let object = facts.hash_as(&mut ctx, plain, HashKind::OBJECT).unwrap();
    let contract = facts.hash_as(&mut ctx, plain, HashKind::ANY).unwrap();
    let joined = facts.widen(&mut ctx, plain, object, 1).unwrap();
    assert!(!facts.plain_hash(joined));
    assert!(!facts.hash_mode(joined).tagged());
    assert!(facts.hash_mode(contract).tagged());
    assert!(objects::may_be_protected(&mut ctx, &facts, contract).unwrap());
    assert!(!objects::may_be_protected(&mut ctx, &facts, joined).unwrap());
    let site = crate::bytecode::CallSite {
        name: 0,
        method: Method::parse("clear"),
        auto: true,
        parenthesized: false,
        scope: false,
    };
    assert!(matches!(
        objects::select(&mut ctx, &facts, contract, site, "clear").unwrap(),
        Some(Selection::UnmodeledProtection)
    ));
    // A provenance-sensitive member splits the join into its exact plain and
    // object copies instead of abandoning dispatch.
    let variants = objects::variants(&mut ctx, &mut facts, joined, "clear", false)
        .unwrap()
        .unwrap();
    assert_eq!(variants.data, [plain, object]);
    assert!(
        objects::variants(&mut ctx, &mut facts, contract, "clear", false)
            .unwrap()
            .is_none()
    );
    // Indexed writes stay modeled on the join and preserve its provenance set,
    // while the contract keeps its guard.
    let key = facts.symbol(&mut ctx, b"x").unwrap();
    let write = facts
        .collection_write(&mut ctx, joined, key, Atom::Int.fact())
        .unwrap();
    assert!(!write.unsupported && !write.rejected, "{write:?}");
    assert_eq!(facts.hash_mode(write.receiver), facts.hash_mode(joined));
    assert!(
        facts
            .collection_write(&mut ctx, contract, key, Atom::Int.fact())
            .unwrap()
            .unsupported
    );
    // Exact protected data alongside the join keeps its tag.
    let protected = facts
        .protected(&mut ctx, object, crate::hash::Tag::Match)
        .unwrap();
    let mixed = facts.union(&mut ctx, &[joined, protected]).unwrap();
    assert_eq!(facts.arm_count(mixed), 2);
    assert!((0..2).any(|i| facts.arm(mixed, i) == protected));
    drop(variants);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn collection_assignment_loops_finish_with_reachable_boundary_diagnostics() {
    for (source, rejected) in [
        (
            "def run(n: int) -> array; a=[]; while n>0; a=[a]; n-=1; end; a; end",
            false,
        ),
        (
            "def run(n: int) -> array<int>; a=[]; while n>0; a=[a,\"bad\"]; n-=1; end; a; end",
            true,
        ),
        (
            "def run(n: int) -> hash; h={}; while n>0; h={child:h}; n-=1; end; h; end",
            false,
        ),
        (
            "def run(n: int) -> hash<string,int>; h={}; while n>0; h={child:h,value:\"bad\"}; n-=1; end; h; end",
            true,
        ),
        (
            "def run(n: int) -> array<array<int>>; a=[[7]]; while n>0; a=[a]; n-=1; end; a; end",
            true,
        ),
        (
            "def run(n: int) -> int; a=[7,\"other\"]; while n>0; a=[7,a]; n-=1; end; a[0]; end",
            false,
        ),
        (
            "def run(n: int) -> string; a=[7,\"other\"]; while n>0; a=[7,a]; n-=1; end; a[0]; end",
            true,
        ),
        (
            "def run(n: int) -> int; a={value:7}; while n>0; a={value:7,child:a}; n-=1; end; a.value; end",
            false,
        ),
        (
            "def run -> array<int>; a=[]; while false; a=[a,\"bad\"]; end; a; end",
            false,
        ),
        (
            "def run(n: int); a=[]; while n>0; a=[a]; n-=1; next; a=7; end; a; end",
            false,
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, source)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
        assert_eq!(
            !result.issues.data.is_empty(),
            rejected,
            "{source}: {result:?}"
        );
        drop((result, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn shared_deep_widening_uses_bounded_work_and_the_default_rust_stack() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut a = Atom::Int.fact();
    let mut b = Atom::String.fact();
    for _ in 0..2000 {
        a = facts.tuple(&mut ctx, &[a, a]).unwrap();
        b = facts.tuple(&mut ctx, &[b, b]).unwrap();
    }
    let joined = facts.widen(&mut ctx, a, b, 2000).unwrap();
    assert_eq!(facts.depth(joined), 2000);
    assert_eq!(
        facts.relation(&mut ctx, a, joined).unwrap(),
        Relation::Accepted
    );
    assert_eq!(
        facts.relation(&mut ctx, b, joined).unwrap(),
        Relation::Accepted
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let a = facts.tuple(ctx, &[Atom::Int.fact()])?;
    let b = facts.tuple(ctx, &[Atom::String.fact(), a])?;
    let a = facts.shape(ctx, &[(b"x", a, false)], false)?;
    let b = facts.shape(ctx, &[(b"y", b, false)], false)?;
    let _ = facts.widen(ctx, a, b, 4)?;
    let _ = facts.widen(ctx, a, b, 4)?;
    Ok(())
}

#[test]
fn widening_has_exact_quotas_failure_cleanup_and_cancellation() {
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
        assert_eq!(accounting(&mut ctx).as_ref().err().map(|e| e.kind), error);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by(131) {
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
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let a = facts.array(&mut ctx, Atom::Int.fact()).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        assert_eq!(
            facts.widen(&mut ctx, a, a, 1).unwrap_err().kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
