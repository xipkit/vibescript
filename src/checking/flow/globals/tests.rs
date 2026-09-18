use super::*;
use crate::checking::globals::{layout, tests::Fixture};
use crate::{CallOptions, ErrorKind, Limits};

fn state(ctx: &mut CallContext, source: SourceId, globals: &Globals) -> Result<State> {
    let mut state = State::new(ctx, 3, source.callable(0), &globals.layout)?;
    state.global_pending = globals.pending.snapshot(ctx)?;
    for (index, &value) in globals.values.data.iter().enumerate() {
        state.locals.set(
            ctx,
            state.global_base + index,
            Binding {
                value,
                missing: globals.missing.data[index],
                owner: blocks::Owner::Unknown,
            },
        )?;
    }
    Ok(state)
}

#[test]
fn a_call_can_grow_storage_without_moving_selected_writes_or_caller_locals() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = Fixture::new(&mut ctx, &mut facts, "x=1;module M;X=1;end").unwrap();
    let b = Fixture::new(&mut ctx, &mut facts, "JSON;Math;y=2;module N;Y=2;end").unwrap();
    let mut storage = layout::Storage::new();
    let before = storage
        .prepare(&mut ctx, &mut facts, a.definition(&[], None))
        .unwrap();
    let slots = before.source(&mut ctx, a.source).unwrap();
    let initial = Globals::initial(&mut ctx, &before).unwrap();
    let mut caller = state(&mut ctx, a.source, &initial).unwrap();
    let root = caller.global_base + slots.files.start;
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let old = facts.tuple(&mut ctx, &[one]).unwrap();
    caller.store(&mut ctx, &mut facts, root, old).unwrap();
    caller.store(&mut ctx, &mut facts, 0, one).unwrap();
    let mut address = Address::new(Some(root), old);
    let last = facts.integer(&mut ctx, -1).unwrap();
    assert_eq!(
        address.index(&mut ctx, &mut facts, &[last]).unwrap().value,
        one
    );
    caller.addresses.push(&mut ctx, address).unwrap();
    let mut snapshot = caller.snapshot(&mut ctx).unwrap();
    let inputs = caller.global_call(&mut ctx).unwrap();
    let mut callee = state(&mut ctx, a.source, &inputs).unwrap();
    let next = storage
        .prepare(&mut ctx, &mut facts, b.definition(&[], Some(a.source)))
        .unwrap();
    callee.expand(&mut ctx, &next).unwrap();
    let other = next.source(&mut ctx, b.source).unwrap();
    callee
        .store(
            &mut ctx,
            &mut facts,
            callee.global_base + other.files.start,
            two,
        )
        .unwrap();
    let appended = facts.tuple(&mut ctx, &[one, two]).unwrap();
    callee.store(&mut ctx, &mut facts, root, appended).unwrap();
    let mutation = Address::new(Some(root), old);
    callee
        .refresh_globals(
            &mut ctx,
            &mut facts,
            root,
            appended,
            &Change::Mutation {
                address: &mutation,
                method: Some(Method::Push),
                args: &[two],
                fresh: false,
            },
        )
        .unwrap();
    let returned = callee.globals(&mut ctx).unwrap();
    caller
        .apply_globals(&mut ctx, &mut facts, &returned)
        .unwrap();
    assert_eq!(caller.locals.get(&mut ctx, 0).unwrap().value, one);
    assert_eq!(caller.locals.get(&mut ctx, root).unwrap().value, appended);
    assert_eq!(
        caller
            .locals
            .get(&mut ctx, caller.global_base + other.files.start)
            .unwrap()
            .value,
        two
    );
    assert_eq!(caller.addresses.data[0].root, Some(root));
    assert_eq!(caller.addresses.data[0].value, one);
    let rebuilt = caller.addresses.data[0]
        .rebuild(&mut ctx, &mut facts, two)
        .unwrap();
    assert!(!rebuilt.unsupported);
    assert_eq!(rebuilt.value, facts.tuple(&mut ctx, &[two, two]).unwrap());
    assert_eq!(snapshot.global_count, before.len());
    assert_eq!(snapshot.locals.get(&mut ctx, root).unwrap().value, old);
    assert!(
        snapshot
            .join(&mut ctx, &mut facts, &caller, false, &a.code.program)
            .unwrap()
    );
    assert_eq!(snapshot.global_count, next.len());
    let binding = snapshot
        .locals
        .get(&mut ctx, snapshot.global_base + other.files.start)
        .unwrap();
    assert_eq!(binding.value, two);
    assert!(binding.missing);
    drop((
        a, b, storage, before, slots, initial, caller, snapshot, inputs, callee, next, other,
        mutation, returned, facts,
    ));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn older_exits_do_not_remove_sources_discovered_by_the_caller() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = Fixture::new(&mut ctx, &mut facts, "x=1").unwrap();
    let b = Fixture::new(&mut ctx, &mut facts, "y=2").unwrap();
    let mut storage = layout::Storage::new();
    let first = storage
        .prepare(&mut ctx, &mut facts, a.definition(&[], None))
        .unwrap();
    let mut initial = Globals::initial(&mut ctx, &first).unwrap();
    let a_slot = first.source(&mut ctx, a.source).unwrap().files.start;
    let seven = facts.integer(&mut ctx, 7).unwrap();
    initial.values.data[a_slot] = seven;
    initial.missing.data[a_slot] = false;
    initial.written.data[a_slot] = true;
    let mut caller = state(&mut ctx, a.source, &initial).unwrap();
    let latest = storage
        .prepare(&mut ctx, &mut facts, b.definition(&[], Some(a.source)))
        .unwrap();
    let b_slot = latest.source(&mut ctx, b.source).unwrap().files.start;
    caller.expand(&mut ctx, &latest).unwrap();
    caller
        .store(
            &mut ctx,
            &mut facts,
            caller.global_base + b_slot,
            Atom::Bool.fact(),
        )
        .unwrap();
    caller
        .apply_globals(&mut ctx, &mut facts, &initial)
        .unwrap();
    assert!(caller.global_layout.same(&latest));
    assert_eq!(
        caller
            .locals
            .get(&mut ctx, caller.global_base + b_slot)
            .unwrap()
            .value,
        Atom::Bool.fact()
    );
    assert_eq!(
        caller
            .locals
            .get(&mut ctx, caller.global_base + a_slot)
            .unwrap()
            .value,
        seven
    );
    drop((a, b, storage, first, initial, caller, latest, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn growing_call(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let a = Fixture::new(ctx, &mut facts, "x=1;module M;X=1;end")?;
    let b = Fixture::new(ctx, &mut facts, "JSON;Math;Time;y=2;module N;Y=2;end")?;
    let mut storage = layout::Storage::new();
    let first = storage.prepare(ctx, &mut facts, a.definition(&[], None))?;
    let initial = Globals::initial(ctx, &first)?;
    let mut caller = state(ctx, a.source, &initial)?;
    let previous = caller.snapshot(ctx)?;
    let inputs = caller.global_call(ctx)?;
    let mut callee = state(ctx, a.source, &inputs)?;
    let next = storage.prepare(ctx, &mut facts, b.definition(&[], Some(a.source)))?;
    let slot = next.source(ctx, b.source)?.files.start;
    callee.expand(ctx, &next)?;
    callee.store(ctx, &mut facts, callee.global_base + slot, Atom::Int.fact())?;
    let result = callee.globals(ctx)?;
    caller.apply_globals(ctx, &mut facts, &result)?;
    caller.join(ctx, &mut facts, &previous, true, &a.code.program)?;
    let binding = caller.locals.get(ctx, caller.global_base + slot)?;
    assert_eq!(binding.value, Atom::Int.fact());
    assert!(binding.missing);
    Ok(())
}

#[test]
fn flow_growth_call_returns_and_widening_share_exact_and_interrupted_quotas() {
    let mut ctx = CallContext::new(CallOptions::default());
    growing_call(&mut ctx).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for memory in [false, true] {
        for sample in 1..=25 {
            let steps = if memory {
                stats.steps
            } else if sample == 25 {
                stats.steps - 1
            } else {
                stats.steps * sample as u64 / 24
            };
            let bytes = if !memory {
                stats.peak_memory_bytes
            } else if sample == 25 {
                stats.peak_memory_bytes - 1
            } else {
                stats.peak_memory_bytes * sample / 24
            };
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: Some(steps),
                    memory_bytes: Some(bytes),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let result = growing_call(&mut ctx);
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
    }
}
