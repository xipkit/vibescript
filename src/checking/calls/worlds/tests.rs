use super::*;
use crate::{
    CallOptions, Engine, ErrorKind, HostMethod, Limits, Script, Signature, SignatureParam,
};

fn fixtures() -> Vec<(Script, CallOptions)> {
    let mut fixtures = Vec::new();
    for (extra, ty, value) in [
        (false, "int", Value::int(7)),
        (true, "bool", Value::boolean(false)),
    ] {
        let mut engine = Engine::new();
        if extra {
            engine.register("aaa", |_, _| panic!("checker invoked a callback"));
        }
        engine.register_method(
            "choose",
            HostMethod::new("choose", |_, _, _| panic!("checker invoked a callback"))
                .with_signature(Signature {
                    params: vec![SignatureParam {
                        name: "value".into(),
                        ty: ty.into(),
                        optional: false,
                    }],
                    result: ty.into(),
                    accepts_block: false,
                })
                .unwrap(),
        );
        let mut text = "def run(n:int);if n>0;run(n-1);else;choose(root);end;end".to_owned();
        if extra {
            text.push_str(";def later;nil;end;module M;def self.unused;nil;end;end");
        }
        fixtures.push((
            engine.compile(&text).unwrap(),
            CallOptions {
                globals: [("root".into(), value)].into(),
                ..CallOptions::default()
            },
        ));
    }
    fixtures
}

fn file(engine: &Engine, source: &str) -> Script {
    Script {
        inner: Arc::new(crate::ScriptInner {
            code: crate::code::Code::compile_file(source, &engine.hosts).unwrap(),
            loader: engine.loader.clone(),
            strict_effects: engine.strict_effects,
            random_source: engine.random_source.clone(),
            output_writer: engine.output_writer.clone(),
            error_writer: engine.error_writer.clone(),
        }),
    }
}

#[test]
fn interleaved_files_keep_private_writes_namespaces_and_builtin_indexes() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let mut entries = Vec::new();
    for (prefix, constant, root) in [
        ("JSON;Math;", 7, 5),
        ("Math;JSON;module Other;X=1;end;", 9, 11),
    ] {
        let source = format!(
            "{prefix}module M;X={constant};def self.read;X;end;end;x=[1];def change;x.push(2);3;end;x[-1]+=change();M.read*100+x[0]*10+x[1]+root"
        );
        let script = file(&Engine::new(), &source);
        let options = CallOptions {
            globals: [("root".into(), Value::int(root))].into(),
            ..CallOptions::default()
        };
        let expected = script.run(options.clone()).unwrap().value.as_int().unwrap();
        assert_eq!(expected, constant * 100 + 42 + root);
        let source = register(&mut ctx, &mut facts, &mut state, &script, &options).unwrap();
        let (index, handle) = state.worlds.get(&mut ctx, source).unwrap();
        let mut solver = state.adapter(index, &handle);
        let mut context = Context::plain();
        context.kind = Kind::Entry { general: false };
        let entry = solver
            .request(&mut ctx, &mut facts, 0, &[], flow::NO_ERROR, &context)
            .unwrap();
        entries.push((entry, expected));
    }
    state.solve(&mut ctx, &mut facts).unwrap();
    for (entry, expected) in entries {
        let job = &state.jobs.data[entry];
        assert_eq!(job.returns, facts.integer(&mut ctx, expected).unwrap());
    }
    for job in &state.jobs.data {
        let report = job.report.as_ref().unwrap();
        assert!(
            report.incomplete.data.is_empty(),
            "{:?}",
            report.incomplete.data
        );
        assert!(report.issues.data.is_empty(), "{:?}", report.issues.data);
    }
    drop((state, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn host_named_contracts_use_the_calling_sources_mapped_declarations() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let mut engine = Engine::new();
    engine.register_method(
        "accept",
        HostMethod::new("accept", move |_, _, _| {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Value::int(7))
        })
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "value".into(),
                ty: "Choice".into(),
                optional: false,
            }],
            result: "int".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let mut entries = Vec::new();
    for prefix in ["JSON;enum Other;One;end;", "Math;JSON;"] {
        let script = file(
            &engine,
            &format!("{prefix}enum Choice;One;end;accept(Choice::One)"),
        );
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(7)
        );
        let source = register(
            &mut ctx,
            &mut facts,
            &mut state,
            &script,
            &CallOptions::default(),
        )
        .unwrap();
        let (index, handle) = state.worlds.get(&mut ctx, source).unwrap();
        let mut solver = state.adapter(index, &handle);
        entries.push(
            solver
                .request(
                    &mut ctx,
                    &mut facts,
                    0,
                    &[],
                    flow::NO_ERROR,
                    &Context::plain(),
                )
                .unwrap(),
        );
    }
    state.solve(&mut ctx, &mut facts).unwrap();
    for entry in entries {
        let job = &state.jobs.data[entry];
        assert_eq!(job.returns, Atom::Int.fact());
        assert!(job.report.as_ref().unwrap().issues.data.is_empty());
        assert!(job.report.as_ref().unwrap().incomplete.data.is_empty());
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    drop((state, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn register(
    ctx: &mut CallContext,
    facts: &mut Facts,
    state: &mut Scheduler<'_>,
    script: &Script,
    options: &CallOptions,
) -> Result<SourceId> {
    let environment = Environment::new(ctx, facts, script, options)?;
    let handle = Handle::owned(ctx, facts, environment)?;
    let source = handle.view().source;
    state.worlds.insert(ctx, handle)?;
    Ok(source)
}

fn request(
    ctx: &mut CallContext,
    facts: &mut Facts,
    state: &mut Scheduler<'_>,
    source: SourceId,
    count: i64,
) -> Result<usize> {
    request_kind(ctx, facts, state, source, count, Kind::Plain)
}

fn request_kind(
    ctx: &mut CallContext,
    facts: &mut Facts,
    state: &mut Scheduler<'_>,
    source: SourceId,
    count: i64,
    kind: Kind,
) -> Result<usize> {
    let input = facts.integer(ctx, count)?;
    request_inputs(ctx, facts, state, source, &[Input::Supplied(input)], kind)
}

fn request_inputs(
    ctx: &mut CallContext,
    facts: &mut Facts,
    state: &mut Scheduler<'_>,
    source: SourceId,
    inputs: &[Input],
    kind: Kind,
) -> Result<usize> {
    let (world_index, handle) = state.worlds.get(ctx, source)?;
    let view = handle.view();
    let mut solver = state.adapter(world_index, &handle);
    let mut context = Context::plain();
    context.kind = kind;
    solver.request(
        ctx,
        facts,
        view.world.program.names["run"],
        inputs,
        flow::NO_ERROR,
        &context,
    )
}

#[test]
fn mixed_owned_sources_share_one_queue_and_keep_their_call_tables() {
    let fixtures = fixtures();
    let weak_codes: Vec<_> = fixtures
        .iter()
        .map(|(script, _)| Arc::downgrade(&script.inner.code))
        .collect();
    assert_ne!(
        fixtures[0].0.inner.code.program.functions.len(),
        fixtures[1].0.inner.code.program.functions.len()
    );
    assert_ne!(
        fixtures[0].0.inner.code.hosts.len(),
        fixtures[1].0.inner.code.hosts.len()
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let sources: Vec<_> = fixtures
        .iter()
        .map(|(script, options)| {
            register(&mut ctx, &mut facts, &mut state, script, options).unwrap()
        })
        .collect();
    drop(fixtures);
    let first = request(&mut ctx, &mut facts, &mut state, sources[0], 2).unwrap();
    let second = request(&mut ctx, &mut facts, &mut state, sources[1], 3).unwrap();
    let third = request(&mut ctx, &mut facts, &mut state, sources[0], 4).unwrap();
    assert_eq!(state.queue.data.len(), 3);
    state.solve(&mut ctx, &mut facts).unwrap();
    for (index, source, expected) in [
        (first, sources[0], Atom::Int.fact()),
        (second, sources[1], Atom::Bool.fact()),
        (third, sources[0], Atom::Int.fact()),
    ] {
        let job = &state.jobs.data[index];
        assert_eq!(job.source, source);
        assert_eq!(job.returns, expected);
        assert!(job.report.as_ref().unwrap().incomplete.data.is_empty());
        assert!(job.report.as_ref().unwrap().issues.data.is_empty());
    }
    for job in &state.jobs.data {
        assert!(job.report.is_some());
        assert!(!job.queued);
        for dependency in &job.dependencies.data {
            assert_eq!(state.jobs.data[*dependency].source, job.source);
        }
    }
    assert_eq!(state.current, EMPTY);
    assert!(state.dependencies.data.is_empty());
    assert!(weak_codes.iter().all(|code| code.upgrade().is_some()));
    drop((state, facts));
    assert!(weak_codes.iter().all(|code| code.upgrade().is_none()));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn an_active_owned_view_survives_registry_growth() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let fixtures = fixtures();
    let source = register(
        &mut ctx,
        &mut facts,
        &mut state,
        &fixtures[0].0,
        &fixtures[0].1,
    )
    .unwrap();
    let (world_index, handle) = state.worlds.get(&mut ctx, source).unwrap();
    let Metadata::Owned(prepared) = &handle.0 else {
        panic!()
    };
    let weak = Arc::downgrade(prepared);
    drop(fixtures);
    {
        let mut active = state.adapter(world_index, &handle);
        let before = active.resolve(&mut ctx, "run").unwrap();
        let mut weak_codes = Vec::new();
        for _ in 0..24 {
            let script = Engine::new().compile("def extra;nil;end").unwrap();
            weak_codes.push(Arc::downgrade(&script.inner.code));
            register(
                &mut ctx,
                &mut facts,
                active.state,
                &script,
                &CallOptions::default(),
            )
            .unwrap();
        }
        assert!(weak_codes.iter().all(|code| code.upgrade().is_some()));
        assert_eq!(active.source, source);
        assert_eq!(active.resolve(&mut ctx, "run").unwrap(), before);
        assert_eq!(active.layouts.source_owner, active.world.source_owner);
        assert_eq!(active.state.worlds.entries.data.len(), 25);
    }
    let entry = request(&mut ctx, &mut facts, &mut state, source, 2).unwrap();
    state.solve(&mut ctx, &mut facts).unwrap();
    assert_eq!(state.jobs.data[entry].returns, Atom::Int.fact());
    drop(state);
    assert!(weak.upgrade().is_some());
    drop(handle);
    assert!(weak.upgrade().is_none());
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn registry_rejects_replacement_and_mismatched_layouts() {
    let fixtures = fixtures();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let first = register(
        &mut ctx,
        &mut facts,
        &mut state,
        &fixtures[0].0,
        &fixtures[0].1,
    )
    .unwrap();
    let second = register(
        &mut ctx,
        &mut facts,
        &mut state,
        &fixtures[1].0,
        &fixtures[1].1,
    )
    .unwrap();
    let (_, a) = state.worlds.get(&mut ctx, first).unwrap();
    let (_, b) = state.worlds.get(&mut ctx, second).unwrap();
    assert!(
        matches!(Handle::borrowed(&mut ctx, &facts, a.view().world, b.view().layouts), Err(error) if error.kind == ErrorKind::Runtime)
    );
    assert_eq!(
        state.worlds.insert(&mut ctx, a.clone()).unwrap_err().kind,
        ErrorKind::Runtime
    );
    assert_eq!(state.worlds.entries.data.len(), 2);
    let entry = request(&mut ctx, &mut facts, &mut state, first, 0).unwrap();
    state.solve(&mut ctx, &mut facts).unwrap();
    assert_eq!(state.jobs.data[entry].returns, Atom::Int.fact());
    drop((state, a, b, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn work(ctx: &mut CallContext, fixtures: &[(Script, CallOptions)]) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let mut entries = Vec::new();
    for (script, options) in fixtures {
        let source = register(ctx, &mut facts, &mut state, script, options)?;
        entries.push(request(ctx, &mut facts, &mut state, source, 2)?);
    }
    state.solve(ctx, &mut facts)?;
    for (index, expected) in entries
        .into_iter()
        .zip([Atom::Int.fact(), Atom::Bool.fact()])
    {
        assert_eq!(state.jobs.data[index].returns, expected);
    }
    Ok(())
}

#[test]
fn preparation_and_mixed_source_solving_share_exact_and_interrupted_limits() {
    let fixtures = fixtures();
    let owners: Vec<_> = fixtures
        .iter()
        .map(|(script, _)| Arc::strong_count(&script.inner.code))
        .collect();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &fixtures).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for sample in 0..=24 {
        for memory in [false, true] {
            let mut limits = Limits::default();
            let expected = if memory {
                limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 24);
                ErrorKind::Memory
            } else {
                limits.steps = Some(stats.steps * sample as u64 / 24);
                ErrorKind::Steps
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let result = work(&mut ctx, &fixtures);
            if sample == 24 {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().kind, expected);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            }
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            for ((script, _), owners) in fixtures.iter().zip(&owners) {
                assert_eq!(Arc::strong_count(&script.inner.code), *owners);
            }
        }
    }
    for memory in [false, true] {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(stats.peak_memory_bytes - usize::from(memory)),
                steps: Some(stats.steps - u64::from(!memory)),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            work(&mut ctx, &fixtures).unwrap_err().kind,
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
fn prepared_source_fast_paths_preserve_cancellation_and_latched_failures() {
    for reason in [
        ErrorKind::Steps,
        ErrorKind::Memory,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let fixtures = fixtures();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
        let source = register(
            &mut ctx,
            &mut facts,
            &mut state,
            &fixtures[0].0,
            &fixtures[0].1,
        )
        .unwrap();
        request(&mut ctx, &mut facts, &mut state, source, 2).unwrap();
        let (_, handle) = state.worlds.get(&mut ctx, source).unwrap();
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
        for source in [source, SourceId::ROOT] {
            assert!(
                matches!(state.worlds.get(&mut ctx, source), Err(error) if error.kind == reason)
            );
        }
        assert_eq!(
            state
                .worlds
                .insert(&mut ctx, handle.clone())
                .unwrap_err()
                .kind,
            reason
        );
        assert_eq!(state.solve(&mut ctx, &mut facts).unwrap_err().kind, reason);
        assert_eq!(state.queue.data.len(), 1);
        drop((handle, state, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn incomplete_environment_preparation_does_not_invoke_factories() {
    let script = Engine::new().compile("7").unwrap();
    let options = CallOptions {
        capabilities: vec![crate::Capability::new("pending", |_| {
            panic!("checker invoked a factory")
        })],
        ..CallOptions::default()
    };
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let environment = Environment::new(&mut ctx, &mut facts, &script, &options).unwrap();
    assert!(
        matches!(Handle::owned(&mut ctx, &facts, environment), Err(error) if error.kind == ErrorKind::Runtime)
    );
    assert!(ctx.checkpoint().is_ok());
    assert!(!script.check(&options).unwrap().incomplete.is_empty());
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn deferred_entry_failures_do_not_leak_between_sources() {
    let fixtures = fixtures();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let a = register(
        &mut ctx,
        &mut facts,
        &mut state,
        &fixtures[0].0,
        &fixtures[0].1,
    )
    .unwrap();
    let b = register(
        &mut ctx,
        &mut facts,
        &mut state,
        &fixtures[1].0,
        &fixtures[1].1,
    )
    .unwrap();
    let (world_index, handle) = state.worlds.get(&mut ctx, a).unwrap();
    let view = handle.view();
    let bound = Arguments::new()
        .bind_host(
            &mut ctx,
            &mut facts,
            &view.world.program.functions[view.world.program.names["run"]].params,
        )
        .unwrap();
    assert_eq!(bound.failures.data, [Failure::Missing(0)]);
    state.worlds.entries.data[world_index]
        .entry_failures
        .extend(&mut ctx, &bound.failures.data)
        .unwrap();
    let first = request_inputs(
        &mut ctx,
        &mut facts,
        &mut state,
        a,
        &bound.inputs.data,
        Kind::Entry { general: false },
    )
    .unwrap();
    let second = request_kind(
        &mut ctx,
        &mut facts,
        &mut state,
        b,
        0,
        Kind::Entry { general: false },
    )
    .unwrap();
    state.solve(&mut ctx, &mut facts).unwrap();
    assert_eq!(state.jobs.data[first].returns, Atom::Never.fact());
    let first_report = state.jobs.data[first].report.as_ref().unwrap();
    assert_eq!(first_report.issues.data.len(), 1);
    assert!(
        matches!(first_report.issues.data[0].kind, flow::IssueKind::Call {
        target: Target::Function(function), failure: Failure::Missing(0),
    } if function.source == a)
    );
    assert_eq!(state.jobs.data[second].returns, Atom::Bool.fact());
    assert!(
        state.jobs.data[second]
            .report
            .as_ref()
            .unwrap()
            .issues
            .data
            .is_empty()
    );
    drop((state, handle, bound, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
