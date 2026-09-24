use super::*;
use crate::{
    CallOptions, Engine, ErrorKind, Limits, Script,
    checking::{facts::HashKind, normalization_tests::observed},
    code::Code,
};

fn analyze_pair(
    ctx: &mut CallContext,
    facts: &mut Facts,
    caller: &Arc<Code>,
    callee: &Arc<Code>,
    exported: Callable,
) -> Result<Analysis> {
    let caller_owner = facts.source_owner(ctx, caller, None)?;
    let callee_owner = facts.source_owner(ctx, callee, None)?;
    let caller_layouts = Layouts::new(ctx, &caller.program, caller_owner)?;
    let callee_layouts = Layouts::new(ctx, &callee.program, callee_owner)?;
    let callable = facts.callable(ctx, callee_owner, exported)?;
    let fields = facts.shape(ctx, &[(b"remote", callable, false)], false)?;
    let object = facts.hash_as(ctx, fields, HashKind::OBJECT)?;
    let roots = [
        (ctx.bytes(b"lib")?, Target::Value(object)),
        (ctx.bytes(b"seed")?, Target::Deferred(0)),
    ];
    let inputs = [Value::int(7)];
    let foreign_roots = [
        (ctx.bytes(b"seed")?, Target::Deferred(0)),
        (ctx.bytes(b"hidden")?, Target::Deferred(1)),
    ];
    let foreign_inputs = [Value::int(999), Value::int(321)];
    let mut caller_contracts = Buffer::empty();
    let mut callee_contracts = Buffer::empty();
    let mut caller_hosts = Buffer::empty();
    let mut callee_hosts = Buffer::empty();
    for (code, contracts, hosts) in [
        (caller, &mut caller_contracts, &mut caller_hosts),
        (callee, &mut callee_contracts, &mut callee_hosts),
    ] {
        for ty in &code.program.types {
            let contract = facts.annotation(ctx, ty, |_, _| Ok(None))?;
            contracts.push(ctx, contract)?;
        }
        for registered in &code.hosts {
            let host = Host::registered(ctx, facts, registered)?;
            hosts.push(ctx, host)?;
        }
    }
    let caller_world = World {
        loader: None,
        program: &caller.program,
        source_owner: caller_owner,
        contracts: &caller_contracts.data,
        hosts: &caller_hosts.data,
        globals: &roots,
        inputs: &inputs,
    };
    let callee_world = World {
        loader: None,
        program: &callee.program,
        source_owner: callee_owner,
        contracts: &callee_contracts.data,
        hosts: &callee_hosts.data,
        globals: &foreign_roots,
        inputs: &foreign_inputs,
    };
    let mut scheduler = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let caller_handle = Handle::borrowed(ctx, facts, caller_world, &caller_layouts)?;
    let callee_handle = Handle::borrowed(ctx, facts, callee_world, &callee_layouts)?;
    let index = scheduler.worlds.insert(ctx, caller_handle.clone())?;
    scheduler.worlds.insert(ctx, callee_handle)?;
    let mut solver = scheduler.adapter(index, &caller_handle);
    let entry = solver.request(
        ctx,
        facts,
        caller.program.names["run"],
        &[],
        flow::NO_ERROR,
        &Context::plain(),
    )?;
    solver.solve(ctx, facts)?;
    let result = Analysis {
        returns: solver.state.jobs.data[entry].returns,
        throws: solver.state.jobs.data[entry].throws,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: solver.state.jobs.data.len(),
    };
    solver.collect(ctx, &[entry], result)
}

fn runtime_function(code: &Arc<Code>) -> Value {
    let mut producer = CallContext::new(CallOptions::default());
    let environment = crate::objects::environment(&mut producer).unwrap();
    let function = crate::exports::Function::new(
        &mut producer,
        code.clone(),
        environment,
        code.program.names["remote"],
    )
    .unwrap();
    Value::object(vec![(
        b"remote".to_vec(),
        Value(crate::value::Kind::Function(function)),
    )])
}

fn check(caller: &Script, callee: &Arc<Code>, expected: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze_pair(
        &mut ctx,
        &mut facts,
        &caller.inner.code,
        callee,
        Callable::Function(callee.program.names["remote"]),
    )
    .unwrap();
    assert!(
        report.incomplete.data.is_empty(),
        "{report:?}; caller: {:?}; callee: {:?}",
        caller.inner.code.program.source,
        callee.program.source
    );
    assert_eq!(!report.issues.data.is_empty(), rejected, "{report:?}");
    let output = caller
        .call(
            "run",
            &[],
            CallOptions {
                globals: [
                    ("lib".into(), runtime_function(callee)),
                    ("seed".into(), Value::int(7)),
                ]
                .into(),
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(output.value.to_string(), expected);
    let actual = observed(
        &mut ctx,
        &mut facts,
        &caller.inner.code.program,
        &output.value,
    );
    assert_ne!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Rejected,
        "{report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn prepared_foreign_functions_use_their_parameters_defaults_and_return_contracts() {
    for (body, expected, rejected) in [
        ("lib.remote(4, right:5)", "9", false),
        ("lib[:remote](4)", "7", false),
        ("lib::remote(4, 6)", "10", false),
        ("lib.remote()", "5", false),
        ("begin;lib.remote(false);rescue;19;end", "19", true),
        ("begin;fn=lib[:remote];fn(4);rescue;21;end", "21", true),
    ] {
        let caller = Engine::new()
            .compile(&format!("def other(x,y,z);false;end;def run;{body};end"))
            .unwrap();
        let callee = Code::compile_file(
            "def remote(left:int=2,right:int=3) -> int;left+right;end",
            &Default::default(),
        )
        .unwrap();
        check(&caller, &callee, expected, rejected);
    }
}

#[test]
fn foreign_auto_calls_use_the_defining_arity_and_keep_methods_attached() {
    for (body, expected, rejected) in [
        ("lib.remote", "7", false),
        ("lib[:remote]()", "7", false),
        ("begin;fn=lib[:remote];7;rescue;9;end", "9", true),
    ] {
        let caller = Engine::new()
            .compile(&format!("def run;{body};end;def misleading(a,b,c);nil;end"))
            .unwrap();
        let callee = Code::compile_file("def remote;7;end", &Default::default()).unwrap();
        check(&caller, &callee, expected, rejected);
    }
}

#[test]
fn foreign_yields_preserve_captures_pending_mutations_and_control_transfers() {
    for (body, expected) in [
        ("x=1;r=lib.remote(4) {|n| x+=n};[x,r]", "[5, 5]"),
        ("x=1;r=lib.remote(4) {|n| x+=n;break 7};[x,r]", "[5, 7]"),
        ("lib.remote(4) {|n| return n+5};99", "9"),
        ("a=[1];a[-1]+=lib.remote(4) {|n| a.push(9);n};a", "[5, 9]"),
        ("a=[1];lib.remote(4) {|n| a.map {|x| x+n}}", "[5]"),
        ("r=lib.remote(4) {|n| lib.remote(n+1) {|x| x*2}};r", "10"),
    ] {
        let caller = Engine::new()
            .compile(&format!("def padding;nil;end;def run;{body};end"))
            .unwrap();
        let callee = Code::compile_file(
            "def remote(n);one=1;two=2;three=3;four=4;yield n;end",
            &Default::default(),
        )
        .unwrap();
        check(&caller, &callee, expected, false);
    }
}

#[test]
fn foreign_recursion_and_errors_keep_source_identity() {
    for (caller_source, callee_source, expected, rejected) in [
        (
            "def run;lib.remote(3);end",
            "def remote(n);if n>0;remote(n-1)+1;else;4;end;end",
            "7",
            false,
        ),
        (
            "def run;begin;lib.remote();rescue;11;end;end",
            "def remote;1+nil;end",
            "11",
            true,
        ),
        (
            "def run;begin;lib.remote();rescue;13;end;end",
            "def remote -> int;false;end",
            "13",
            true,
        ),
    ] {
        let caller = Engine::new().compile(caller_source).unwrap();
        let callee = Code::compile_file(callee_source, &Default::default()).unwrap();
        check(&caller, &callee, expected, rejected);
        if rejected {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let report = analyze_pair(
                &mut ctx,
                &mut facts,
                &caller.inner.code,
                &callee,
                Callable::Function(callee.program.names["remote"]),
            )
            .unwrap();
            let owner = facts.source_owner(&mut ctx, &callee, None).unwrap();
            let source = facts.source_id(&mut ctx, owner).unwrap();
            assert!(
                report
                    .issues
                    .data
                    .iter()
                    .any(|issue| issue.source == source)
            );
        }
    }
}

#[test]
fn imported_functions_use_receiving_roots_and_preserve_private_rebindings() {
    for (caller_source, callee_source, expected, rejected) in [
        (
            "def run;lib.remote();end",
            "def remote;seed+1;end",
            "8",
            false,
        ),
        (
            "def run;r=lib.remote();[r,seed];end",
            "def remote;seed+=2;seed;end",
            "[9, 7]",
            false,
        ),
        (
            "def run;begin;lib.remote();rescue;19;end;end",
            "def remote;hidden;end",
            "19",
            true,
        ),
        (
            "def helper(n);n+3;end;def run;lib.remote();end",
            "def remote;helper(4);end",
            "7",
            false,
        ),
        (
            "def helper;7;end;def run;lib.remote();end",
            "def remote;helper;end",
            "7",
            false,
        ),
        (
            "def helper;7;end;def run;lib.remote();end",
            "def helper;8;end;def remote;helper;end",
            "8",
            false,
        ),
        (
            "def JSON;7;end;def run;lib.remote();end",
            "def remote;JSON;end",
            "7",
            false,
        ),
        (
            "def int(n);n+1;end;def run;lib.remote();end",
            "def remote;int(4);end",
            "5",
            false,
        ),
        (
            "def JSON;7;end;def run;lib.remote();end",
            "def remote;JSON.to_s;end",
            "7",
            false,
        ),
        (
            "def helper;yield 4;end;def run;lib.remote {|n| n+5};end",
            "def remote;helper {|n| yield n};end",
            "9",
            false,
        ),
        (
            "def helper(n);lib.remote(n-1);end;def run;lib.remote(6);end",
            "def remote(n);if n>0;helper(n-1);else;4;end;end",
            "4",
            false,
        ),
    ] {
        let caller = Engine::new().compile(caller_source).unwrap();
        let callee = Code::compile_file(callee_source, &Default::default()).unwrap();
        check(&caller, &callee, expected, rejected);
    }
}

#[test]
fn receiving_shadows_never_fall_through_to_builtins() {
    for (body, caller_body, expected, rejected) in [
        ("JSON", "lib.remote()==JSON", "true", false),
        ("JSON::One", "lib.remote()==JSON::One", "true", false),
        (
            "JSON.stringify(1)",
            "begin;lib.remote();rescue;9;end",
            "9",
            true,
        ),
    ] {
        let caller = Engine::new()
            .compile(&format!("enum JSON;One;end;def run;{caller_body};end"))
            .unwrap();
        let callee =
            Code::compile_file(&format!("def remote;{body};end"), &Default::default()).unwrap();
        check(&caller, &callee, expected, rejected);
    }
    let caller = Engine::new()
        .compile("def JSON;7;end;def run;begin;lib.remote();rescue;9;end;end")
        .unwrap();
    let callee =
        Code::compile_file("def remote;JSON.stringify(1);end", &Default::default()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze_pair(
        &mut ctx,
        &mut facts,
        &caller.inner.code,
        &callee,
        Callable::Function(callee.program.names["remote"]),
    )
    .unwrap();
    // The receiving function returns an integer, which has no stringify member.
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(report.issues.data.len(), 1, "{report:?}");
    let output = caller
        .call(
            "run",
            &[],
            CallOptions {
                globals: [("lib".into(), runtime_function(&callee))].into(),
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(output.value.as_int(), Some(9));
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn check_host(
    caller: &Script,
    callee: &Script,
    method: &crate::HostMethod,
    expected: &str,
    rejected: bool,
) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let host = callee
        .inner
        .code
        .program
        .hosts
        .iter()
        .position(|name| name == "remote")
        .unwrap();
    let report = analyze_pair(
        &mut ctx,
        &mut facts,
        &caller.inner.code,
        &callee.inner.code,
        Callable::Host(host),
    )
    .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(!report.issues.data.is_empty(), rejected, "{report:?}");
    let output = caller
        .call(
            "run",
            &[],
            CallOptions {
                globals: [(
                    "lib".into(),
                    Value::object(vec![(b"remote".to_vec(), method.value())]),
                )]
                .into(),
                ..CallOptions::default()
            },
        )
        .unwrap();
    assert_eq!(output.value.to_string(), expected);
    let actual = observed(
        &mut ctx,
        &mut facts,
        &caller.inner.code.program,
        &output.value,
    );
    assert_ne!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Rejected,
        "{report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn foreign_host_metadata_uses_the_callers_named_contracts() {
    use crate::{HostMethod, Signature, SignatureParam};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let method = HostMethod::new("remote", move |_, _, _| {
        counter.fetch_add(1, Ordering::Relaxed);
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
    .unwrap();
    let mut provider = Engine::new();
    provider.register_method("remote", method.clone());
    let callee = provider.compile("enum Choice;Other;end;nil").unwrap();
    let mut receiver = Engine::new();
    receiver.register("wrong", |_, _| panic!("checker invoked the wrong callback"));
    for (body, expected, rejected) in [
        ("lib.remote(Choice::One)", "7", false),
        ("begin;lib.remote(false);rescue;17;end", "17", true),
    ] {
        let before = calls.load(Ordering::Relaxed);
        let caller = receiver
            .compile(&format!("enum Choice;One;end;def run;{body};end"))
            .unwrap();
        check_host(&caller, &callee, &method, expected, rejected);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            before + usize::from(!rejected)
        );
    }
}

#[test]
fn foreign_host_blocks_keep_sticky_break_and_return() {
    use crate::{HostMethod, Signature};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let method = HostMethod::new_with_block("remote", move |call, _, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        let _ = call.call_block(&[]);
        let _ = call.call_block(&[]);
        Ok(Value::int(99))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: true,
    })
    .unwrap();
    let mut provider = Engine::new();
    provider.register_method("remote", method.clone());
    let callee = provider.compile("nil").unwrap();
    for (body, expected, rejected) in [
        ("lib.remote {break 7}", "7", false),
        ("lib.remote {return 9};17", "9", false),
        ("begin;lib.remote {break false};rescue;17;end", "17", true),
    ] {
        let before = calls.load(Ordering::Relaxed);
        let caller = Engine::new()
            .compile(&format!("def run;{body};end"))
            .unwrap();
        check_host(&caller, &callee, &method, expected, rejected);
        assert_eq!(calls.load(Ordering::Relaxed), before + 1);
    }
}

fn register(
    ctx: &mut CallContext,
    facts: &mut Facts,
    state: &mut Scheduler<'static>,
    script: &Script,
) -> (usize, Handle<'static>) {
    let environment =
        crate::checking::environment::Environment::new(ctx, facts, script, &CallOptions::default())
            .unwrap();
    let handle = Handle::owned(ctx, facts, environment).unwrap();
    let index = state.worlds.insert(ctx, handle.clone()).unwrap();
    (index, handle)
}

fn closure(function: CallableId) -> blocks::Closure {
    blocks::Closure {
        scope: blocks::Scope::Invocation,
        function,
        receiver: None,
        ambient: None,
        given: false,
        locals: 0,
        inherited: Buffer::empty(),
        captures: Buffer::empty(),
        pending: crate::checking::pending::Pending::new(),
        destinations: Buffer::empty(),
    }
}

#[test]
fn foreign_initializers_and_methods_use_the_defining_namespace_state() {
    let caller = Engine::new()
        .compile("module M;X=99;end;def run;nil;end")
        .unwrap();
    let callee = Engine::new()
        .compile("module M;X=7;def self.read;X;end;end")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let (index, handle) = register(&mut ctx, &mut facts, &mut state, &caller);
    let (_, foreign) = register(&mut ctx, &mut facts, &mut state, &callee);
    let view = foreign.view();
    let body = view.world.program.namespaces[0].body.unwrap();
    let body = closure(view.source.callable(body));
    let receiver = crate::checking::namespaces::value(
        &mut ctx,
        &mut facts,
        view.world.program,
        view.world.source_owner,
        0,
    )
    .unwrap();
    let method = view.world.program.namespaces[0]
        .methods
        .iter()
        .find(|method| method.name == "read")
        .unwrap()
        .function;
    let target = Target::Method {
        function: view.source.callable(method),
        receiver,
        constructor: false,
    };
    let mut solver = state.adapter(index, &handle);
    let layout = solver.prepare(&mut ctx, &mut facts).unwrap();
    let globals = Globals::initial(&mut ctx, &layout).unwrap();
    let parent = solver
        .request(
            &mut ctx,
            &mut facts,
            caller.inner.code.program.names["run"],
            &[],
            flow::NO_ERROR,
            &Context::plain(),
        )
        .unwrap();
    solver.solve(&mut ctx, &mut facts).unwrap();
    solver.state.current = parent;
    solver
        .initialize(&mut ctx, &mut facts, &body, flow::NO_ERROR, &globals)
        .unwrap();
    solver.solve(&mut ctx, &mut facts).unwrap();
    solver.state.current = parent;
    let initialized = solver
        .initialize(&mut ctx, &mut facts, &body, flow::NO_ERROR, &globals)
        .unwrap();
    assert!(!initialized.incomplete);
    let returned = initialized
        .exits
        .data
        .iter()
        .find(|exit| exit.completion == blocks::Completion::Value)
        .unwrap();
    let fields = returned
        .globals
        .layout
        .source(&mut ctx, view.source)
        .unwrap()
        .namespace(0);
    let seven = facts.integer(&mut ctx, 7).unwrap();
    let fields = returned.globals.value(&mut ctx, fields).unwrap();
    assert_eq!(
        facts.selected_field(&mut ctx, fields, b"X").unwrap(),
        Some((seven, false))
    );
    solver
        .invoke(
            &mut ctx,
            &mut facts,
            target,
            Arguments::new(),
            flow::NO_ERROR,
            &returned.globals,
        )
        .unwrap();
    solver.solve(&mut ctx, &mut facts).unwrap();
    solver.state.current = parent;
    let outcome = solver
        .invoke(
            &mut ctx,
            &mut facts,
            target,
            Arguments::new(),
            flow::NO_ERROR,
            &returned.globals,
        )
        .unwrap();
    assert!(!outcome.incomplete);
    assert_eq!(outcome.value, seven);
    assert!(solver.state.jobs.data.iter().all(|job| {
        job.report.as_ref().is_some_and(|report| {
            report.issues.data.is_empty() && report.incomplete.data.is_empty()
        })
    }));
    drop((
        outcome,
        initialized,
        body,
        globals,
        layout,
        state,
        foreign,
        handle,
        facts,
    ));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn prepared_dispatch_never_reassigns_an_independent_receiving_environment() {
    let a = Engine::new().compile("def run;1;end").unwrap();
    let b = Engine::new().compile("def run;2;end").unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
    let (ai, ah) = register(&mut ctx, &mut facts, &mut state, &a);
    let (bi, bh) = register(&mut ctx, &mut facts, &mut state, &b);
    state
        .adapter(ai, &ah)
        .prepare(&mut ctx, &mut facts)
        .unwrap();
    state
        .adapter(bi, &bh)
        .prepare(&mut ctx, &mut facts)
        .unwrap();
    let before = state.storage.layout.clone();
    let target = Target::Function(bh.view().source.callable(b.inner.code.program.names["run"]));
    let globals = Globals::initial(&mut ctx, &before).unwrap();
    let result = state
        .adapter(ai, &ah)
        .invoke(
            &mut ctx,
            &mut facts,
            target,
            Arguments::new(),
            flow::NO_ERROR,
            &globals,
        )
        .unwrap();
    assert!(result.incomplete);
    assert!(state.jobs.data.is_empty());
    assert!(state.storage.layout.same(&before));
    drop((result, globals, before, state, ah, bh, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn prepared_foreign_dispatch_fast_paths_keep_cancellation_deadlines_and_failures() {
    for reason in [
        ErrorKind::Steps,
        ErrorKind::Memory,
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
    ] {
        let script = Engine::new().compile("def run;1;end").unwrap();
        let other = Engine::new().compile("def run;2;end").unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut state = Scheduler::new(crate::checking::inputs::Values::new(), false);
        let (index, handle) = register(&mut ctx, &mut facts, &mut state, &script);
        let (_, foreign) = register(&mut ctx, &mut facts, &mut state, &other);
        let mut solver = state.adapter(index, &handle);
        let source = foreign.view().source;
        solver
            .callee(&mut ctx, &mut facts, source)
            .unwrap()
            .unwrap();
        let id = source.callable(other.inner.code.program.names["run"]);
        let body = closure(id);
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
            solver.function_arity(&mut ctx, id).unwrap_err().kind,
            reason
        );
        assert_eq!(
            solver
                .attached(
                    &mut ctx,
                    foreign.view().world.source_owner,
                    Callable::Function(id.index)
                )
                .unwrap_err()
                .kind,
            reason
        );
        assert_eq!(
            solver.host_uses_block(&mut ctx, id).unwrap_err().kind,
            reason
        );
        for result in [
            solver.invoke(
                &mut ctx,
                &mut facts,
                Target::Function(id),
                Arguments::new(),
                flow::NO_ERROR,
                &Globals::empty(),
            ),
            solver.initialize(
                &mut ctx,
                &mut facts,
                &body,
                flow::NO_ERROR,
                &Globals::empty(),
            ),
            solver.host_boundary(
                &mut ctx,
                &mut facts,
                id,
                HostBoundary::Result(None),
                &Globals::empty(),
            ),
        ] {
            assert_eq!(result.err().unwrap().kind, reason);
        }
        assert_eq!(ctx.checkpoint().unwrap_err().kind, reason);
        drop((body, state, handle, foreign, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn foreign_dispatch_keeps_one_budget_and_releases_interrupted_summaries() {
    let caller = Code::compile(
        "def run;a=[1];a[-1]+=lib.remote(4) {|n| a.push(9);n};a;end",
        &Default::default(),
    )
    .unwrap();
    let callee = Code::compile_file("def remote(n);yield n;end", &Default::default()).unwrap();
    let run = |options| {
        let mut ctx = CallContext::new(options);
        let result = (|| {
            let mut facts = Facts::new(&mut ctx)?;
            analyze_pair(
                &mut ctx,
                &mut facts,
                &caller,
                &callee,
                Callable::Function(callee.program.names["remote"]),
            )
        })();
        let error = result.as_ref().err().map(|error| error.kind);
        if let Ok(report) = &result {
            assert!(report.incomplete.data.is_empty(), "{report:?}");
            assert!(report.issues.data.is_empty(), "{report:?}");
        }
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        if let Some(error) = error {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, error);
        }
        (error, ctx.stats())
    };
    let (_, baseline) = run(CallOptions::default());
    let (_, repeated) = run(CallOptions::default());
    assert_eq!(baseline.steps, repeated.steps);
    assert_eq!(baseline.peak_memory_bytes, repeated.peak_memory_bytes);
    for (steps, memory, expected) in [
        (baseline.steps, baseline.peak_memory_bytes, None),
        (baseline.steps - 1, 0, Some(ErrorKind::Steps)),
        (0, baseline.peak_memory_bytes - 1, Some(ErrorKind::Memory)),
    ] {
        let options = CallOptions {
            limits: Limits {
                steps: (steps > 0).then_some(steps),
                memory_bytes: (memory > 0).then_some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(run(options).0, expected);
    }
    for fraction in [1, 2, 3, 5, 7] {
        for memory in [false, true] {
            let options = CallOptions {
                limits: Limits {
                    steps: (!memory).then_some(baseline.steps * fraction / 8),
                    memory_bytes: memory
                        .then_some(baseline.peak_memory_bytes * fraction as usize / 8),
                    ..Limits::default()
                },
                ..CallOptions::default()
            };
            assert_eq!(
                run(options).0,
                Some(if memory {
                    ErrorKind::Memory
                } else {
                    ErrorKind::Steps
                })
            );
        }
    }
}
