use super::super::{calls::Root, sources::SourceId};
use super::*;
use crate::{CallOptions, ErrorKind, Limits, code::Code};
use std::sync::Arc;

pub(in crate::checking) struct Fixture {
    pub code: Arc<Code>,
    pub source: SourceId,
    owner: usize,
    files: super::super::file_bindings::Layout,
}

impl Fixture {
    pub fn new(ctx: &mut CallContext, facts: &mut Facts, source: &str) -> Result<Self> {
        let code = Code::compile_file(source, &Default::default()).unwrap();
        let owner = facts.source_owner(ctx, &code, None)?;
        let source = facts.source_id(ctx, owner)?;
        let files = super::super::file_bindings::Layout::new(ctx, &code.program)?;
        Ok(Self {
            code,
            source,
            owner,
            files,
        })
    }

    pub fn definition<'a>(
        &'a self,
        roots: &'a [Root],
        receiving: Option<SourceId>,
    ) -> layout::Definition<'a> {
        layout::Definition {
            source: self.source,
            owner: self.owner,
            program: &self.code.program,
            files: &self.files,
            roots,
            receiving,
        }
    }
}

#[test]
fn growing_sources_preserve_addresses_and_share_only_receiving_roots() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let first = Fixture::new(&mut ctx, &mut facts, "JSON;Math;x=7;module M;X=1;end").unwrap();
    let second = Fixture::new(
        &mut ctx,
        &mut facts,
        "Math;JSON;Time;x=false;module M;X=2;end",
    )
    .unwrap();
    let separate = Fixture::new(&mut ctx, &mut facts, "Math;JSON;x=9;module M;X=3;end").unwrap();
    let seven = facts.integer(&mut ctx, 7).unwrap();
    let roots = [Root {
        name: ctx.bytes(b"root").unwrap(),
        value: seven,
        missing: true,
    }];
    let mut storage = layout::Storage::new();
    let before = storage
        .prepare(&mut ctx, &mut facts, first.definition(&roots, None))
        .unwrap();
    let a = before.source(&mut ctx, first.source).unwrap();
    let mut state = Globals::initial(&mut ctx, &before).unwrap();
    state.values.data[a.files.start] = seven;
    state.missing.data[a.files.start] = false;
    state.written.data[a.files.start] = true;
    state
        .pending
        .addresses
        .push(
            &mut ctx,
            super::super::addresses::Address::new(Some(a.files.start), seven),
        )
        .unwrap();
    let snapshot = state.snapshot(&mut ctx).unwrap();
    let prepared = storage
        .prepare(
            &mut ctx,
            &mut facts,
            second.definition(&roots, Some(first.source)),
        )
        .unwrap();
    let b = prepared.source(&mut ctx, second.source).unwrap();
    assert_eq!(a.globals.data[0], b.globals.data[1]);
    assert_eq!(a.globals.data[1], b.globals.data[0]);
    assert_eq!(a.roots.data, b.roots.data);
    assert!(b.files.start >= before.len());
    assert!(b.globals.data[2] >= before.len());
    assert_ne!(a.namespace(0), b.namespace(0));
    assert_ne!(a.declarations.start, b.declarations.start);
    assert!(state.expand(&mut ctx, &prepared).unwrap());
    assert_eq!(&state.values.data[..before.len()], &snapshot.values.data);
    assert_eq!(state.pending.addresses.data[0].root, Some(a.files.start));
    assert!(snapshot.layout.same(&before));
    assert!(snapshot.same_initialization(&mut ctx, &state).unwrap());
    let initialized = facts.boolean(&mut ctx, true).unwrap();
    state.values.data[b.namespace(0) + 1] = initialized;
    assert!(!snapshot.same_initialization(&mut ctx, &state).unwrap());
    let latest = storage
        .prepare(&mut ctx, &mut facts, separate.definition(&roots, None))
        .unwrap();
    let c = latest.source(&mut ctx, separate.source).unwrap();
    assert_ne!(c.globals.data[0], b.globals.data[0]);
    assert_ne!(c.roots.data[0], a.roots.data[0]);
    assert!(Arc::ptr_eq(
        &a,
        &latest.source(&mut ctx, first.source).unwrap()
    ));
    let cached = storage
        .prepare(&mut ctx, &mut facts, first.definition(&roots, None))
        .unwrap();
    assert!(latest.same(&cached));
    drop((
        cached, latest, c, b, a, prepared, snapshot, state, before, storage, roots, first, second,
        separate, facts,
    ));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn joins_expand_old_snapshots_with_missing_state_and_keep_initialization_separate() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let first = Fixture::new(&mut ctx, &mut facts, "x=1;module M;X=1;end").unwrap();
    let second = Fixture::new(&mut ctx, &mut facts, "x=2;module M;X=2;end").unwrap();
    let mut storage = layout::Storage::new();
    let before = storage
        .prepare(&mut ctx, &mut facts, first.definition(&[], None))
        .unwrap();
    let a = before.source(&mut ctx, first.source).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let mut old = Globals::initial(&mut ctx, &before).unwrap();
    old.values.data[a.files.start] = one;
    old.missing.data[a.files.start] = false;
    let latest = storage
        .prepare(
            &mut ctx,
            &mut facts,
            second.definition(&[], Some(first.source)),
        )
        .unwrap();
    let b = latest.source(&mut ctx, second.source).unwrap();
    let mut new = old.snapshot(&mut ctx).unwrap();
    new.expand(&mut ctx, &latest).unwrap();
    new.values.data[b.files.start] = two;
    new.missing.data[b.files.start] = false;
    new.written.data[b.files.start] = true;
    new.values.data[b.namespace(0) + 1] = facts.boolean(&mut ctx, true).unwrap();
    assert!(old.compatible(&mut ctx, &new).unwrap());
    assert!(!old.equal(&mut ctx, &new).unwrap());
    let mut forward = old.snapshot(&mut ctx).unwrap();
    let mut reverse = new.snapshot(&mut ctx).unwrap();
    assert!(forward.join(&mut ctx, &mut facts, &new, None).unwrap());
    assert!(reverse.join(&mut ctx, &mut facts, &old, None).unwrap());
    assert!(forward.equal(&mut ctx, &reverse).unwrap());
    assert_eq!(forward.values.data[a.files.start], one);
    assert_eq!(forward.values.data[b.files.start], two);
    assert!(forward.missing.data[b.files.start]);
    assert!(forward.written.data[b.files.start]);
    assert_eq!(forward.values.data[b.namespace(0) + 1], Atom::Bool.fact());
    let mut left = std::collections::hash_map::DefaultHasher::new();
    let mut right = std::collections::hash_map::DefaultHasher::new();
    forward.hash(&mut ctx, &mut left).unwrap();
    reverse.hash(&mut ctx, &mut right).unwrap();
    assert_eq!(left.finish(), right.finish());
    drop((
        old, new, forward, reverse, a, b, before, latest, storage, first, second, facts,
    ));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn unrelated_address_spaces_cannot_be_combined_or_reassigned() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let first = Fixture::new(&mut ctx, &mut facts, "module M;X=1;end").unwrap();
    let second = Fixture::new(&mut ctx, &mut facts, "module M;X=2;end").unwrap();
    let mut a = layout::Storage::new();
    let mut b = layout::Storage::new();
    let left = a
        .prepare(&mut ctx, &mut facts, first.definition(&[], None))
        .unwrap();
    let right = b
        .prepare(&mut ctx, &mut facts, first.definition(&[], None))
        .unwrap();
    assert!(!left.compatible(&right));
    assert_eq!(
        left.latest(&mut ctx, &right).unwrap_err().kind,
        ErrorKind::Runtime
    );
    assert!(
        a.prepare(
            &mut ctx,
            &mut facts,
            second.definition(&[], Some(second.source))
        )
        .is_err()
    );
    assert!(a.layout.same(&left));
    a.prepare(&mut ctx, &mut facts, second.definition(&[], None))
        .unwrap();
    assert!(
        a.prepare(
            &mut ctx,
            &mut facts,
            first.definition(&[], Some(second.source))
        )
        .is_err()
    );
    drop((left, right, a, b, first, second, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn bounded_growth(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let mut storage = layout::Storage::new();
    let a = Fixture::new(ctx, &mut facts, "JSON;Math;x=1;module M;X=1;end")?;
    let b = Fixture::new(ctx, &mut facts, "Math;JSON;Time;x=2;module N;X=2;end")?;
    let first = storage.prepare(ctx, &mut facts, a.definition(&[], None))?;
    let mut old = Globals::initial(ctx, &first)?;
    let next = storage.prepare(ctx, &mut facts, b.definition(&[], Some(a.source)));
    if next.is_err() {
        assert!(storage.layout.same(&first));
    }
    let next = next?;
    let mut new = old.snapshot(ctx)?;
    new.expand(ctx, &next)?;
    let depth = facts.max_depth();
    old.join(ctx, &mut facts, &new, Some(depth))?;
    Ok(())
}

#[test]
fn address_preparation_growth_and_joins_use_one_budget_and_release_failed_work() {
    let mut baseline = CallContext::new(CallOptions::default());
    bounded_growth(&mut baseline).unwrap();
    let stats = baseline.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for memory in [false, true] {
        for sample in 1..=24 {
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    memory_bytes: Some(if memory {
                        stats.peak_memory_bytes * sample / 24
                    } else {
                        stats.peak_memory_bytes
                    }),
                    steps: Some(if memory {
                        stats.steps
                    } else {
                        stats.steps * sample as u64 / 24
                    }),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let result = bounded_growth(&mut ctx);
            if sample == 24 {
                result.unwrap();
            } else {
                let expected = if memory {
                    ErrorKind::Memory
                } else {
                    ErrorKind::Steps
                };
                assert_eq!(result.unwrap_err().kind, expected);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(stats.peak_memory_bytes - usize::from(memory)),
                steps: Some(stats.steps - u64::from(!memory)),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            bounded_growth(&mut ctx).unwrap_err().kind,
            if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            }
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn layout_fast_paths_preserve_latched_limits_cancellation_and_deadlines() {
    for reason in [
        ErrorKind::Steps,
        ErrorKind::Memory,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let fixture = Fixture::new(&mut ctx, &mut facts, "JSON;x=1").unwrap();
        let mut storage = layout::Storage::new();
        let layout = storage
            .prepare(&mut ctx, &mut facts, fixture.definition(&[], None))
            .unwrap();
        let mut globals = Globals::initial(&mut ctx, &layout).unwrap();
        match reason {
            ErrorKind::Steps => {
                ctx.charge(u64::MAX).unwrap_err();
            }
            ErrorKind::Memory => {
                ctx.reserve(usize::MAX).unwrap_err();
            }
            ErrorKind::Cancelled => ctx.cancellation().cancel(),
            ErrorKind::Deadline => ctx.options.deadline = Some(std::time::Instant::now()),
            _ => unreachable!(),
        }
        assert_eq!(
            storage
                .prepare(&mut ctx, &mut facts, fixture.definition(&[], None))
                .unwrap_err()
                .kind,
            reason
        );
        assert_eq!(layout.latest(&mut ctx, &layout).unwrap_err().kind, reason);
        assert_eq!(
            layout.source(&mut ctx, fixture.source).unwrap_err().kind,
            reason
        );
        assert_eq!(globals.expand(&mut ctx, &layout).unwrap_err().kind, reason);
        assert!(storage.layout.same(&layout));
        drop((storage, layout, globals, fixture, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
