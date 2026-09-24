use super::*;
use crate::{CallOptions, ErrorKind, code::Code};

fn world(code: &Code, owner: usize) -> World<'_> {
    World {
        loader: None,
        program: &code.program,
        source_owner: owner,
        contracts: &[],
        hosts: &[],
        globals: &[],
        inputs: &[],
    }
}

fn registered<'a>(
    ctx: &mut CallContext,
    facts: &Facts,
    state: &mut Scheduler<'a>,
    world: World<'a>,
    layouts: &'a Layouts,
) -> (usize, Handle<'a>) {
    let handle = Handle::borrowed(ctx, facts, world, layouts).unwrap();
    let index = state.worlds.insert(ctx, handle.clone()).unwrap();
    (index, handle)
}

#[test]
fn same_function_indexes_keep_separate_summaries_across_sources_and_scopes() {
    let first = Code::compile("def run;7;end", &Default::default()).unwrap();
    let second = Code::compile("def run;false;end", &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut producer = CallContext::new(CallOptions::default());
    let scope = crate::objects::environment(&mut producer).unwrap();
    let a = facts.source_owner(&mut ctx, &first, None).unwrap();
    let b = facts.source_owner(&mut ctx, &second, None).unwrap();
    let c = facts.source_owner(&mut ctx, &first, Some(&scope)).unwrap();
    let layouts = Layouts::new(&mut ctx, &first.program, a).unwrap();
    let other = Layouts::new(&mut ctx, &second.program, b).unwrap();
    let captured = Layouts::new(&mut ctx, &first.program, c).unwrap();
    let mut state = Scheduler::new(super::super::inputs::Values::new(), false);
    let function = first.program.names["run"];
    assert_eq!(function, second.program.names["run"]);
    let context = Context::plain();
    let seven = facts.integer(&mut ctx, 7).unwrap();
    let falsity = facts.boolean(&mut ctx, false).unwrap();
    let mut entries = Vec::new();
    for (code, owner, layout, expected) in [
        (&first, a, &layouts, seven),
        (&second, b, &other, falsity),
        (&first, c, &captured, seven),
    ] {
        let (world_index, handle) =
            registered(&mut ctx, &facts, &mut state, world(code, owner), layout);
        let mut solver = state.adapter(world_index, &handle);
        let entry = solver
            .request(
                &mut ctx,
                &mut facts,
                function,
                &[],
                flow::NO_ERROR,
                &context,
            )
            .unwrap();
        assert!(!entries.contains(&entry));
        entries.push(entry);
        solver.solve(&mut ctx, &mut facts).unwrap();
        assert_eq!(solver.state.jobs.data[entry].returns, expected);
        assert_eq!(
            solver
                .request(
                    &mut ctx,
                    &mut facts,
                    function,
                    &[],
                    flow::NO_ERROR,
                    &context
                )
                .unwrap(),
            entry
        );
    }
    assert_eq!(state.jobs.data.len(), 3);
    drop(state);
    drop((layouts, other, captured, facts, scope));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn recursion_and_whole_file_reachability_do_not_confuse_sources() {
    let code = Code::compile("def run;7;end", &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut producer = CallContext::new(CallOptions::default());
    let scope = crate::objects::environment(&mut producer).unwrap();
    let a = facts.source_owner(&mut ctx, &code, None).unwrap();
    let b = facts.source_owner(&mut ctx, &code, Some(&scope)).unwrap();
    let source_a = facts.source_id(&mut ctx, a).unwrap();
    let source_b = facts.source_id(&mut ctx, b).unwrap();
    let layouts = Layouts::new(&mut ctx, &code.program, a).unwrap();
    let other = Layouts::new(&mut ctx, &code.program, b).unwrap();
    let mut state = Scheduler::new(super::super::inputs::Values::new(), false);
    let (world_index, handle) = registered(&mut ctx, &facts, &mut state, world(&code, a), &layouts);
    let mut solver = state.adapter(world_index, &handle);
    let function = code.program.names["run"];
    let context = Context::plain();
    let first = solver
        .request(
            &mut ctx,
            &mut facts,
            function,
            &[],
            flow::NO_ERROR,
            &context,
        )
        .unwrap();
    solver.solve(&mut ctx, &mut facts).unwrap();
    solver.state.current = first;
    assert!(
        solver
            .ancestor(
                &mut ctx,
                Ancestor::Function(source_a, function, &context, &facts, &[])
            )
            .unwrap()
            .is_some()
    );
    let mut expanded = Context::plain();
    expanded
        .inherited
        .push(
            &mut ctx,
            blocks::Layer {
                scope: blocks::Scope::Invocation,
                function: source_a.callable(function),
                receiver: None,
                ambient: None,
                given: false,
                locals: 0,
            },
        )
        .unwrap();
    assert!(
        solver
            .ancestor(&mut ctx, Ancestor::Expanding(source_a, function, &expanded))
            .unwrap()
            .is_some()
    );
    let (world_index, other_handle) =
        registered(&mut ctx, &facts, &mut state, world(&code, b), &other);
    let mut solver = state.adapter(world_index, &other_handle);
    for target in [
        Ancestor::Function(source_b, function, &context, &facts, &[]),
        Ancestor::Expanding(source_b, function, &expanded),
    ] {
        assert!(solver.ancestor(&mut ctx, target).unwrap().is_none());
    }
    let second = solver
        .request(
            &mut ctx,
            &mut facts,
            function,
            &[],
            flow::NO_ERROR,
            &context,
        )
        .unwrap();
    assert_ne!(first, second);
    assert!(!solver.state.jobs.data[first].cyclic);
    assert!(solver.state.jobs.data[first].widened.is_none());
    assert!(
        solver
            .whole_reached(&mut ctx, &[first], source_a, function)
            .unwrap()
    );
    assert!(
        !solver
            .whole_reached(&mut ctx, &[first], source_b, function)
            .unwrap()
    );
    solver.state.jobs.data[first]
        .dependencies
        .push(&mut ctx, second)
        .unwrap();
    assert!(
        solver
            .whole_reached(&mut ctx, &[first], source_b, function)
            .unwrap()
    );
    drop(state);
    drop((layouts, other, facts, scope, expanded));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn an_unregistered_queued_source_cannot_use_another_sources_program() {
    let first = Code::compile("def run;7;end", &Default::default()).unwrap();
    let second = Code::compile("nil", &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = facts.source_owner(&mut ctx, &first, None).unwrap();
    let b = facts.source_owner(&mut ctx, &second, None).unwrap();
    let layouts = Layouts::new(&mut ctx, &first.program, a).unwrap();
    let mut state = Scheduler::new(super::super::inputs::Values::new(), false);
    let (world_index, handle) =
        registered(&mut ctx, &facts, &mut state, world(&first, a), &layouts);
    let mut solver = state.adapter(world_index, &handle);
    let entry = solver
        .request(
            &mut ctx,
            &mut facts,
            first.program.names["run"],
            &[],
            flow::NO_ERROR,
            &Context::plain(),
        )
        .unwrap();
    solver.state.jobs.data[entry].source = facts.source_id(&mut ctx, b).unwrap();
    assert_eq!(
        solver.solve(&mut ctx, &mut facts).unwrap_err().kind,
        ErrorKind::Runtime
    );
    drop(state);
    drop((layouts, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn identical_diagnostics_and_incomplete_locations_survive_source_collection() {
    let text = "def run;begin;1+nil;rescue;nil;end;require 'missing';end";
    let first = Code::compile(text, &Default::default()).unwrap();
    let second = Code::compile(text, &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = facts.source_owner(&mut ctx, &first, None).unwrap();
    let b = facts.source_owner(&mut ctx, &second, None).unwrap();
    let layouts = Layouts::new(&mut ctx, &first.program, a).unwrap();
    let other = Layouts::new(&mut ctx, &second.program, b).unwrap();
    let mut state = Scheduler::new(super::super::inputs::Values::new(), false);
    let mut entries = Vec::new();
    for (code, owner, layout) in [(&first, a, &layouts), (&second, b, &other)] {
        let (world_index, handle) =
            registered(&mut ctx, &facts, &mut state, world(code, owner), layout);
        let mut solver = state.adapter(world_index, &handle);
        let context = Context::plain();
        let entry = solver
            .request(
                &mut ctx,
                &mut facts,
                code.program.names["run"],
                &[],
                flow::NO_ERROR,
                &context,
            )
            .unwrap();
        entries.extend([entry, entry]);
    }
    state.solve(&mut ctx, &mut facts).unwrap();
    let source = facts.source_id(&mut ctx, a).unwrap();
    let (world_index, handle) = state.worlds.get(&mut ctx, source).unwrap();
    let mut solver = state.adapter(world_index, &handle);
    let analysis = Analysis {
        returns: Atom::Unknown.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: solver.state.jobs.data.len(),
    };
    let analysis = solver.collect(&mut ctx, &entries, analysis).unwrap();
    assert_eq!(analysis.issues.data.len(), 2, "{analysis:?}");
    assert_eq!(analysis.incomplete.data.len(), 2, "{analysis:?}");
    assert_ne!(
        analysis.issues.data[0].source,
        analysis.issues.data[1].source
    );
    assert_ne!(
        analysis.incomplete.data[0].source,
        analysis.incomplete.data[1].source
    );
    let check = super::super::entry::Check {
        facts,
        analysis,
        entry: false,
        pending: None,
    };
    let report = super::super::report::build(&mut ctx, &first.program, &check).unwrap();
    assert_eq!(report.diagnostics.len(), 2);
    assert_eq!(report.incomplete.len(), 2);
    drop(state);
    drop((report, check, layouts, other));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn qualified_call_targets_never_dispatch_through_an_unrelated_source() {
    let first = Code::compile("def run;7;end", &Default::default()).unwrap();
    let second = Code::compile("def run;9;end", &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = facts.source_owner(&mut ctx, &first, None).unwrap();
    let b = facts.source_owner(&mut ctx, &second, None).unwrap();
    let source_a = facts.source_id(&mut ctx, a).unwrap();
    let source_b = facts.source_id(&mut ctx, b).unwrap();
    let layouts = Layouts::new(&mut ctx, &first.program, a).unwrap();
    let mut state = Scheduler::new(super::super::inputs::Values::new(), false);
    let (world_index, handle) =
        registered(&mut ctx, &facts, &mut state, world(&first, a), &layouts);
    let mut solver = state.adapter(world_index, &handle);
    let function = first.program.names["run"];
    assert_eq!(
        solver.resolve(&mut ctx, "run").unwrap(),
        Target::Function(source_a.callable(function))
    );
    assert_eq!(
        solver
            .attached(&mut ctx, a, Callable::Function(function))
            .unwrap(),
        Target::Function(source_a.callable(function))
    );
    let foreign = source_b.callable(function);
    for target in [
        Target::Function(foreign),
        Target::Block(foreign),
        Target::Method {
            function: foreign,
            receiver: Atom::Int.fact(),
            constructor: true,
        },
        Target::Host(source_b.callable(usize::MAX)),
    ] {
        let outcome = solver
            .invoke(
                &mut ctx,
                &mut facts,
                target,
                Arguments::new(),
                flow::NO_ERROR,
                &Globals::empty(),
            )
            .unwrap();
        assert!(outcome.incomplete);
        assert_eq!(outcome.value, Atom::Never.fact());
        assert!(solver.state.jobs.data.is_empty());
    }
    for boundary in [
        HostBoundary::Arguments(&Arguments::new()),
        HostBoundary::Result(None),
    ] {
        let outcome = solver
            .host_boundary(&mut ctx, &mut facts, foreign, boundary, &Globals::empty())
            .unwrap();
        assert!(outcome.incomplete);
    }
    assert!(!solver.host_uses_block(&mut ctx, foreign).unwrap());
    drop(state);
    drop((layouts, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn foreign_source_fast_paths_preserve_latched_failures_and_cancellation() {
    for reason in [ErrorKind::Steps, ErrorKind::Cancelled, ErrorKind::Deadline] {
        let code = Code::compile("def run;7;end", &Default::default()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let owner = facts.source_owner(&mut ctx, &code, None).unwrap();
        let layouts = Layouts::new(&mut ctx, &code.program, owner).unwrap();
        let mut state = Scheduler::new(super::super::inputs::Values::new(), false);
        let (world_index, handle) =
            registered(&mut ctx, &facts, &mut state, world(&code, owner), &layouts);
        let mut solver = state.adapter(world_index, &handle);
        let foreign = SourceId::ROOT.callable(1);
        let closure = blocks::Closure {
            scope: blocks::Scope::Invocation,
            function: foreign,
            receiver: None,
            ambient: None,
            given: false,
            locals: 0,
            inherited: Buffer::empty(),
            captures: Buffer::empty(),
            pending: super::super::pending::Pending::new(),
            destinations: Buffer::empty(),
        };
        match reason {
            ErrorKind::Steps => {
                ctx.charge(u64::MAX).unwrap_err();
            }
            ErrorKind::Cancelled => ctx.options.cancellation.cancel(),
            ErrorKind::Deadline => ctx.options.deadline = Some(std::time::Instant::now()),
            _ => unreachable!(),
        }
        for result in [
            solver.host_boundary(
                &mut ctx,
                &mut facts,
                foreign,
                HostBoundary::Result(None),
                &Globals::empty(),
            ),
            solver.initialize_body(
                &mut ctx,
                &mut facts,
                &closure,
                flow::NO_ERROR,
                &Globals::empty(),
            ),
            solver.invoke(
                &mut ctx,
                &mut facts,
                Target::Function(foreign),
                Arguments::new(),
                flow::NO_ERROR,
                &Globals::empty(),
            ),
        ] {
            assert_eq!(result.err().unwrap().kind, reason);
        }
        assert_eq!(ctx.checkpoint().unwrap_err().kind, reason);
        drop(state);
        drop((layouts, facts, closure));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
