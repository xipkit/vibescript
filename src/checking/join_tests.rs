use super::facts::{Atom, Fact, Facts};
use crate::{CallContext, CallOptions, ErrorKind, Limits};

fn pool(ctx: &mut CallContext, facts: &mut Facts) -> Vec<Fact> {
    let mut pool = vec![
        Atom::Nil.fact(),
        Atom::Int.fact(),
        Atom::String.fact(),
        Atom::Bool.fact(),
        Atom::Symbol.fact(),
        Atom::Unknown.fact(),
        Atom::Any.fact(),
        Atom::Never.fact(),
    ];
    for value in -3..12 {
        pool.push(facts.integer(ctx, value).unwrap());
    }
    for value in [false, true] {
        pool.push(facts.boolean(ctx, value).unwrap());
    }
    for text in ["a", "b", "c"] {
        pool.push(facts.string(ctx, text.as_bytes()).unwrap());
        pool.push(facts.symbol(ctx, text.as_bytes()).unwrap());
    }
    let tuple = facts.tuple(ctx, &[pool[8], pool[9]]).unwrap();
    let array = facts.array(ctx, Atom::Int.fact()).unwrap();
    pool.extend([tuple, array]);
    pool
}

#[test]
fn pairwise_and_collected_unions_build_the_same_normalized_facts() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let pool = pool(&mut ctx, &mut facts);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for round in 0..400 {
        let count = [2, 3, 5, 17, 40][round % 5];
        let mut picked = Vec::new();
        for _ in 0..count {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            picked.push(pool[seed as usize % pool.len()]);
        }
        // Nested unions and repeated alternatives exercise the merge, cover and dedup paths.
        let half = facts.union(&mut ctx, &picked[..count / 2]).unwrap();
        let rest = facts.union(&mut ctx, &picked[count / 2..]).unwrap();
        let mut folded = Atom::Never.fact();
        for &fact in picked.iter().rev() {
            folded = facts.union(&mut ctx, &[fact, folded]).unwrap();
        }
        let collected = facts.union(&mut ctx, &picked).unwrap();
        let mut repeated = picked.clone();
        repeated.extend_from_slice(&picked);
        repeated.push(half);
        assert_eq!(folded, collected, "{picked:?}");
        assert_eq!(facts.union(&mut ctx, &[half, rest]).unwrap(), collected);
        assert_eq!(facts.union(&mut ctx, &[rest, half]).unwrap(), collected);
        assert_eq!(facts.union(&mut ctx, &repeated).unwrap(), collected);
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn remembered_operations_replay_results_for_less_work() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut left = Vec::new();
    for value in 0..34 {
        left.push(facts.integer(&mut ctx, value).unwrap());
    }
    left.push(Atom::Nil.fact());
    let left = facts.union(&mut ctx, &left).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let right = facts
        .scalar_binary(&mut ctx, "+", left, one)
        .unwrap()
        .0
        .value;
    for op in ["-", "==", "<", "!="] {
        let before = ctx.stats().steps;
        let (first, limit) = facts.scalar_binary(&mut ctx, op, left, right).unwrap();
        let computed = ctx.stats().steps - before;
        let before = ctx.stats().steps;
        let (second, repeated) = facts.scalar_binary(&mut ctx, op, left, right).unwrap();
        let replayed = ctx.stats().steps - before;
        assert_eq!(
            (
                first.value,
                first.rejected,
                first.unsupported,
                first.throws,
                limit
            ),
            (
                second.value,
                second.rejected,
                second.unsupported,
                second.throws,
                repeated
            ),
            "{op}"
        );
        // Arithmetic and ordering on the nil alternative remain known contradictions.
        assert_eq!(first.rejected, matches!(op, "-" | "<"), "{op}");
        assert!(replayed * 100 < computed, "{op}: {replayed} of {computed}");
    }
    // Decided integer comparisons stop early without dropping either outcome.
    let mut high = Vec::new();
    for value in 1..34 {
        high.push(facts.integer(&mut ctx, value).unwrap());
    }
    let high = facts.union(&mut ctx, &high).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let low = facts.union(&mut ctx, &[one, two]).unwrap();
    let (less, _) = facts.scalar_binary(&mut ctx, "<", low, high).unwrap();
    assert_eq!(less.value, Atom::Bool.fact());
    let zero = facts.integer(&mut ctx, 0).unwrap();
    let (less, _) = facts.scalar_binary(&mut ctx, "<", zero, high).unwrap();
    assert_eq!(less.value, facts.boolean(&mut ctx, true).unwrap());
    let (equal, _) = facts.scalar_binary(&mut ctx, "==", zero, high).unwrap();
    assert_eq!(equal.value, facts.boolean(&mut ctx, false).unwrap());
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn join_caches_obey_exact_limits_and_release_their_tables() {
    let run = |ctx: &mut CallContext| -> crate::Result<()> {
        let mut facts = Facts::new(ctx)?;
        let mut values = Vec::new();
        for value in 0..300 {
            values.push(facts.integer(ctx, value)?);
        }
        let mut union = Atom::Never.fact();
        for &value in &values {
            union = facts.union(ctx, &[union, value])?;
            facts.widen(ctx, union, value, 2)?;
        }
        facts.union(ctx, &values)?;
        Ok(())
    };
    let mut ctx = CallContext::new(CallOptions::default());
    run(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (steps, memory, expected) in [
        (stats.steps, stats.peak_memory_bytes, None),
        (
            stats.steps - 1,
            stats.peak_memory_bytes,
            Some(ErrorKind::Steps),
        ),
        (
            stats.steps,
            stats.peak_memory_bytes - 1,
            Some(ErrorKind::Memory),
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(steps),
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(run(&mut ctx).err().map(|error| error.kind), expected);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
