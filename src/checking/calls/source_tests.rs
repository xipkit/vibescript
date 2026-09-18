use super::*;
use crate::{CallOptions, ErrorKind, code::Code};

fn world(code: &Code, owner: usize) -> World<'_> {
    World {
        program: &code.program,
        source_owner: owner,
        contracts: &[],
        hosts: &[],
        globals: &[],
        inputs: &[],
    }
}

fn solver<'a>(ctx: &mut CallContext, world: World<'a>, layouts: &'a Layouts) -> Solver<'a> {
    let mut functions = Buffer::with_capacity(ctx, world.program.functions.len()).unwrap();
    functions.data.resize(world.program.functions.len(), false);
    Solver {
        whole: false,
        world,
        values: super::super::inputs::Values::new(),
        layouts,
        jobs: Buffer::empty(),
        buckets: Buffer::empty(),
        queue: Buffer::empty(),
        current: EMPTY,
        dependencies: Buffer::empty(),
        functions,
        search: 0,
        entry_failures: Buffer::empty(),
    }
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
    let mut solver = solver(&mut ctx, world(&first, a), &layouts);
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
        solver.world = world(code, owner);
        solver.layouts = layout;
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
        assert_eq!(solver.jobs.data[entry].returns, expected);
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
    assert_eq!(solver.jobs.data.len(), 3);
    drop(solver);
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
    let mut solver = solver(&mut ctx, world(&code, a), &layouts);
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
    solver.current = first;
    assert!(
        solver
            .ancestor(&mut ctx, Ancestor::Function(source_a, function, &context))
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
                function,
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
    solver.world = world(&code, b);
    for target in [
        Ancestor::Function(source_b, function, &context),
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
    assert!(!solver.jobs.data[first].cyclic);
    assert!(solver.jobs.data[first].widened.is_none());
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
    solver.jobs.data[first]
        .dependencies
        .push(&mut ctx, second)
        .unwrap();
    assert!(
        solver
            .whole_reached(&mut ctx, &[first], source_b, function)
            .unwrap()
    );
    drop(solver);
    drop((layouts, facts, scope, expanded));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn a_queued_job_cannot_run_with_another_sources_program() {
    let first = Code::compile("def run;7;end", &Default::default()).unwrap();
    let second = Code::compile("nil", &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let a = facts.source_owner(&mut ctx, &first, None).unwrap();
    let b = facts.source_owner(&mut ctx, &second, None).unwrap();
    let layouts = Layouts::new(&mut ctx, &first.program, a).unwrap();
    let mut solver = solver(&mut ctx, world(&first, a), &layouts);
    solver
        .request(
            &mut ctx,
            &mut facts,
            first.program.names["run"],
            &[],
            flow::NO_ERROR,
            &Context::plain(),
        )
        .unwrap();
    solver.world = world(&second, b);
    assert_eq!(
        solver.solve(&mut ctx, &mut facts).unwrap_err().kind,
        ErrorKind::Runtime
    );
    drop(solver);
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
    let mut solver = solver(&mut ctx, world(&first, a), &layouts);
    let mut entries = Vec::new();
    for (code, owner, layout) in [(&first, a, &layouts), (&second, b, &other)] {
        solver.world = world(code, owner);
        solver.layouts = layout;
        let mut context = Context::plain();
        context.globals = Globals::initial(&mut ctx, &mut facts, &code.program).unwrap();
        context.globals.files(&mut ctx, &layout.files).unwrap();
        context
            .globals
            .namespaces(&mut ctx, &mut facts, &code.program, owner)
            .unwrap();
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
        solver.solve(&mut ctx, &mut facts).unwrap();
    }
    let analysis = Analysis {
        returns: Atom::Unknown.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: solver.jobs.data.len(),
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
    drop(solver);
    drop((report, check, layouts, other));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
