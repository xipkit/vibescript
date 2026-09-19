use super::{
    arguments::{self, Arguments, Failure, Input},
    calls::{self, Analysis, Host, Target, World},
    facts::{Atom, Fact, Facts, Node},
    flow::IssueKind,
};
use crate::{
    CallContext, CallOptions, ErrorKind, HostMethod, Limits, Result, Signature, SignatureParam,
    Value, budget::Buffer, bytecode,
};

fn contracts(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
) -> Result<Buffer<Fact>> {
    let mut result = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(ctx, ty, |_, _| Ok(None))?;
        result.push(ctx, fact)?;
    }
    Ok(result)
}

fn analyze(ctx: &mut CallContext, facts: &mut Facts, source: &str) -> Result<Analysis> {
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let types = contracts(ctx, facts, &program)?;
    let function = program.names["run"];
    let inputs =
        arguments::general_inputs(ctx, facts, &program.functions[function].params, &types.data)?;
    calls::analyze(
        ctx,
        facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program: &program,
            contracts: &types.data,
            hosts: &[],
            globals: &[],
        },
        function,
        &inputs.data,
    )
}

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
    assert_eq!(
        !result.issues.data.is_empty(),
        rejected,
        "{source}: {result:?}"
    );
    drop((facts, result));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn script_calls_propagate_exact_inputs_and_inferred_returns() {
    for (source, rejected) in [
        ("def id(x); x; end; def run -> int; id(7); end", false),
        ("def id(x); x; end; def run -> int; id(\"bad\"); end", true),
        (
            "def add(x: int, y: int) -> int; x + y; end; def run -> int; add(2, 3); end",
            false,
        ),
        (
            "def add(x: int, y: int) -> int; x + y; end; def run -> int; add(2, \"bad\"); end",
            true,
        ),
        (
            "def bad -> int; \"bad\"; end; def run -> int; bad(); end",
            true,
        ),
        ("def good -> int; 7; end; def run -> int; good; end", false),
        (
            "def pick(flag: bool); if flag; 7; else; \"bad\"; end; end; def run -> int; pick(true); end",
            false,
        ),
        (
            "def pick(flag: bool); if flag; 7; else; \"bad\"; end; end; def run -> int; pick(false); end",
            true,
        ),
        (
            "def id(x: any); x; end; def run -> int; id(\"bad\"); end",
            true,
        ),
        ("def run -> int; def_missing(7); end", true),
        ("def run; f = 7; f(1); end", true),
    ] {
        check(source, rejected);
    }
}

#[test]
fn script_argument_binding_preserves_defaults_keywords_and_rest() {
    for (source, rejected) in [
        ("def f(x = 7); x; end; def run -> int; f(); end", false),
        ("def f(x = \"bad\"); x; end; def run -> int; f(); end", true),
        (
            "def f(x = \"bad\"); x; end; def run -> int; f(7); end",
            false,
        ),
        (
            "def f(x: int = \"bad\"); x; end; def run -> int; f(7); end",
            false,
        ),
        (
            "def f(x: int = \"bad\"); x; end; def run -> int; f(); end",
            true,
        ),
        (
            "def f(x: int, y: int = x + 1); y; end; def run -> int; f(x: 7); end",
            false,
        ),
        (
            "def f(x: int, y: int:); x + y; end; def run -> int; f(1, y: 2); end",
            false,
        ),
        (
            "def f(x: int, y: int:); x + y; end; def run -> int; f(y: 2, x: 1); end",
            false,
        ),
        ("def f(x: int); x; end; def run; f(); end", true),
        ("def f(x: int); x; end; def run; f(1, 2); end", true),
        ("def f(x: int); x; end; def run; f(1, x: 2); end", true),
        ("def f(x: int:); x; end; def run; f(y: 2); end", true),
        (
            "def f(options: {x: int}); options; end; def run -> {x: int}; f(x: 7); end",
            false,
        ),
        (
            "def f(options: {x: int}); options; end; def run; f(x: \"bad\"); end",
            true,
        ),
        (
            "def f(*xs: array<int>); xs; end; def run -> array<int>; f(1, 2, 3); end",
            false,
        ),
        (
            "def f(*xs: array<int>); xs; end; def run; f(1, \"bad\"); end",
            true,
        ),
        (
            "def f(**xs: hash<string, int>); xs; end; def run -> hash<string, int>; f(x: 1, y: 2); end",
            false,
        ),
        (
            "def f(**xs: hash<string, int>); xs; end; def run; f(x: 1, y: \"bad\"); end",
            true,
        ),
        (
            "def f(first: int:, **xs: hash<string, int>); xs; end; def run -> hash<string,int>; f(first: 7, x: 1); end",
            false,
        ),
        (
            "def f(x: int); x; end; def run -> int; f(x: \"bad\", x: 7); end",
            false,
        ),
        (
            "def f(x: int); x; end; def run -> int; f(x: 7, x: \"bad\"); end",
            true,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn literal_splats_keep_elements_fields_and_duplicate_keyword_order() {
    for (source, rejected) in [
        (
            "def f(x: int, y: string); x; end; def run -> int; f(*[7, \"yes\"]); end",
            false,
        ),
        (
            "def f(x: int, y: string); x; end; def run; f(*[7, 8]); end",
            true,
        ),
        ("def f(x: int); x; end; def run; f(*7); end", true),
        ("def f(x: int); x; end; def run; f(**7); end", true),
        (
            "def f(x: int, y: string:); x; end; def run -> int; f(**{x: 7, y: \"yes\"}); end",
            false,
        ),
        (
            "def f(x: int); x; end; def run -> int; f(x: \"bad\", **{x: 7}); end",
            false,
        ),
        (
            "def f(x: int); x; end; def run; f(**{x: 7}, x: \"bad\"); end",
            true,
        ),
        (
            "def f(x: int); x; end; def run -> int; f(**{x: \"bad\", x: 7}); end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn nested_calls_and_argument_branches_preserve_pending_call_frames() {
    for (source, rejected) in [
        (
            "def id(x); x; end; def add(x: int,y: int); x+y; end; def run -> int; add(id(7), id(8)); end",
            false,
        ),
        (
            "def id(x); x; end; def add(x: int,y: int); x+y; end; def run(flag: bool) -> int; add(x: flag ? id(7) : id(8), y: id(9)); end",
            false,
        ),
        (
            "def id(x); x; end; def add(x: int,y: int); x+y; end; def run(flag: bool) -> int; add(flag ? id(7) : id(\"bad\"), id(9)); end",
            true,
        ),
        (
            "def id(x); x; end; def run -> int; id(while true; break id(7); end); end",
            false,
        ),
        (
            "def f(x: int,y: int); x+y; end; def run -> int; f(1, while true; break 7; end); end",
            false,
        ),
        (
            "def f(x: int); x; end; def run -> int; while true; f(while true; break 7; end); break 2; end; end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn recursive_summaries_converge_and_unreachable_calls_have_no_diagnostics() {
    for (source, rejected) in [
        (
            "def f(n: int); if n == 0; 7; else; f(n - 1); end; end; def run -> int; f(3); end",
            false,
        ),
        (
            "def f(n: int); if n == 0; \"bad\"; else; f(n - 1); end; end; def run -> int; f(3); end",
            true,
        ),
        (
            "def a(flag: bool); if flag; 7; else; b(!flag); end; end; def b(flag: bool); a(flag); end; def run -> int; a(false); end",
            false,
        ),
        (
            "def a; b(); end; def b; a(); end; def run -> int; a(); \"unreachable\"; end",
            false,
        ),
        (
            "def bad(x: int); \"bad\"; end; def run -> int; if false; bad(\"bad\"); end; 7; end",
            false,
        ),
        (
            "def bad -> int; \"bad\"; end; def run -> int; 7; end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn long_call_chains_use_the_work_queue_on_the_default_stack() {
    let mut source = String::from("def run -> int; f0(); end\n");
    for i in 0..1000 {
        let body = if i == 999 {
            "7".into()
        } else {
            format!("f{}()", i + 1)
        };
        source.push_str(&format!("def f{i}; {body}; end\n"));
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, &source).unwrap();
    assert!(result.issues.data.is_empty(), "{result:?}");
    assert!(result.incomplete.data.is_empty());
    assert_eq!(facts.atom(result.returns), Some(Atom::Int));
    assert_eq!(result.contexts, 1001);
    drop((facts, result));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn host_method() -> HostMethod {
    HostMethod::new("host", |_, _, _| panic!("static analysis called the host"))
        .with_contract(
            |_, _, _| panic!("static analysis ran the argument validator"),
            |_, _| panic!("static analysis ran the result validator"),
        )
        .with_signature(Signature {
            params: vec![
                SignatureParam {
                    name: "value".into(),
                    ty: "int".into(),
                    optional: false,
                },
                SignatureParam {
                    name: "extra".into(),
                    ty: "string".into(),
                    optional: true,
                },
            ],
            result: "string".into(),
            accepts_block: false,
        })
        .unwrap()
}

#[test]
fn registered_host_contracts_are_read_without_running_callbacks_or_validators() {
    let method = host_method();
    for (body, target, rejected) in [
        ("host(7)", "string", false),
        ("host(7, \"ok\")", "string", false),
        ("host(\"bad\")", "string", true),
        ("host()", "string", true),
        ("host(7, \"a\", \"b\")", "string", true),
        ("host(value: 7)", "string", true),
        ("host(7)", "int", true),
    ] {
        let mut engine = crate::Engine::new();
        engine.register_method("host", method.clone());
        let source = format!("def run -> {target}; {body}; end");
        let script = engine.compile(&source).unwrap();
        let program = &script.inner.code.program;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let types = contracts(&mut ctx, &mut facts, program).unwrap();
        let value = method.value();
        let crate::value::Kind::Host(method) = &value.0 else {
            panic!()
        };
        let host = Host::new(&mut ctx, &mut facts, method.compiled_signature()).unwrap();
        let result = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program,
                contracts: &types.data,
                hosts: &[host],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
        assert_eq!(
            !result.issues.data.is_empty(),
            rejected,
            "{source}: {result:?}"
        );
    }
}

#[test]
fn root_overrides_and_local_shadowing_select_the_actual_callee() {
    let sources = [
        ("def f -> int; 7; end; def run -> string; f(); end", false),
        ("def f -> int; 7; end; def run -> int; f(); end", true),
        ("def f -> int; 7; end; def run(f: int); f(); end", true),
    ];
    for (source, rejected) in sources {
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let types = contracts(&mut ctx, &mut facts, &program).unwrap();
        let mut host = Host::new(&mut ctx, &mut facts, None).unwrap();
        host.result = Atom::String.fact();
        let function = program.names["run"];
        let inputs = arguments::general_inputs(
            &mut ctx,
            &mut facts,
            &program.functions[function].params,
            &types.data,
        )
        .unwrap();
        let result = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program: &program,
                contracts: &types.data,
                hosts: &[host],
                globals: &[(
                    Value::bytes(b"f"),
                    Target::Host(super::sources::SourceId::ROOT.callable(0)),
                )],
            },
            function,
            &inputs.data,
        )
        .unwrap();
        assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
        assert_eq!(
            !result.issues.data.is_empty(),
            rejected,
            "{source}: {result:?}"
        );
    }
}

#[test]
fn unresolved_calls_and_mutable_host_bindings_stay_explicitly_incomplete() {
    for source in [
        "def f(*xs); xs; end; def run(xs: array<int>); f(*xs); end",
        "def f(**xs); xs; end; def run(xs: hash<string,int>); f(**xs); end",
        "def run; require(\"missing\"); end",
        "def run; [1].map! { _1 }; end",
        "def run; begin; 1; rescue; 2; ensure; [1].map! { _1 }; end; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!result.incomplete.data.is_empty(), "{source}: {result:?}");
    }
    let program = bytecode::compile("def run; value = 7; value; end", Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program: &program,
            contracts: &[],
            hosts: &[],
            globals: &[(Value::bytes(b"value"), Target::NonCallable)],
        },
        program.names["run"],
        &[],
    )
    .unwrap();
    assert!(!result.incomplete.data.is_empty());
}

#[test]
fn normalized_facts_keep_literals_and_known_arms_without_losing_enum_conversion() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let yes = facts.boolean(&mut ctx, true).unwrap();
    assert_eq!(
        facts.normalized(&mut ctx, yes, Atom::Bool.fact()).unwrap(),
        yes
    );
    let both = facts
        .union(&mut ctx, &[Atom::Int.fact(), Atom::String.fact()])
        .unwrap();
    assert_eq!(
        facts.normalized(&mut ctx, both, Atom::Any.fact()).unwrap(),
        both
    );
    assert_eq!(
        facts
            .normalized(&mut ctx, Atom::Unknown.fact(), Atom::Int.fact())
            .unwrap(),
        Atom::Int.fact()
    );
    let enumeration = facts
        .nominal(&mut ctx, 0, 0, b"Status", Some(&[b"done"]))
        .unwrap();
    let symbol = facts.symbol(&mut ctx, b"done").unwrap();
    assert_eq!(
        facts.normalized(&mut ctx, symbol, enumeration).unwrap(),
        enumeration
    );
    assert!(!facts.unresolved(enumeration));
    let literal = facts.tuple(&mut ctx, &[symbol]).unwrap();
    let target = facts.array(&mut ctx, enumeration).unwrap();
    let converted = facts.tuple(&mut ctx, &[enumeration]).unwrap();
    assert_eq!(
        facts.normalized(&mut ctx, literal, target).unwrap(),
        converted
    );
}

#[test]
fn abstract_binding_matches_runtime_values_and_rejections() {
    let cases = [
        ("x, y=7", vec![Value::int(1)], vec![]),
        ("x, y=7", vec![], vec![("x", Value::int(1))]),
        ("options", vec![], vec![("x", Value::int(1))]),
        ("x", vec![Value::int(1)], vec![("x", Value::int(2))]),
        ("*xs", vec![Value::int(1), Value::bytes(b"two")], vec![]),
        (
            "**xs",
            vec![],
            vec![("x", Value::int(1)), ("y", Value::bytes(b"two"))],
        ),
        (
            "x:, **xs",
            vec![],
            vec![("x", Value::int(1)), ("y", Value::bytes(b"two"))],
        ),
        ("x, y:", vec![Value::int(1)], vec![("y", Value::int(2))]),
        ("x, y:", vec![], vec![("y", Value::int(2))]),
        ("x", vec![Value::int(1), Value::int(2)], vec![]),
    ];
    for (params, positional, keywords) in cases {
        let program =
            bytecode::compile(&format!("def run({params}); nil; end"), Vec::new(), &()).unwrap();
        let params = &program.functions[program.names["run"]].params;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut abstract_args = Arguments::new();
        let mut runtime_args =
            crate::arguments::Arguments::from_values(&mut ctx, &positional).unwrap();
        let fact_of = |value: &Value| -> Fact {
            match &value.0 {
                crate::value::Kind::Int(_) => Atom::Int.fact(),
                crate::value::Kind::Bytes(_) => Atom::String.fact(),
                _ => unreachable!(),
            }
        };
        for value in &positional {
            let fact = fact_of(value);
            abstract_args.positional.push(&mut ctx, fact).unwrap();
        }
        for (name, value) in &keywords {
            let key = facts.symbol(&mut ctx, name.as_bytes()).unwrap();
            let value_fact = fact_of(value);
            abstract_args.keyword(&mut ctx, key, value_fact).unwrap();
            runtime_args
                .push(
                    &mut ctx,
                    bytecode::ArgumentOp::Keyword(0),
                    name,
                    value.clone(),
                )
                .unwrap();
        }
        let bound = abstract_args.bind(&mut ctx, &mut facts, params).unwrap();
        let runtime = crate::arguments::Binding::new(&mut ctx, params, runtime_args);
        assert_eq!(
            !bound.failures.data.is_empty(),
            runtime.is_err(),
            "{params:?}: {:?}",
            bound.failures
        );
        if let Ok(runtime) = runtime {
            for (index, input) in bound.inputs.data.iter().enumerate() {
                let value = runtime.value(&mut ctx, index).unwrap();
                match (input, value) {
                    (Input::Default, None) => (),
                    (Input::Supplied(fact), Some(value)) => match facts.node(*fact) {
                        Node::Atom(Atom::Int) => assert!(value.as_int().is_some()),
                        Node::Atom(Atom::String) => assert!(value.as_bytes().is_some()),
                        Node::Tuple(items) => {
                            let values = value.as_array().unwrap();
                            assert_eq!(items.data.len(), values.len());
                            for (&fact, value) in items.data.iter().zip(values) {
                                assert_eq!(fact, fact_of(value));
                            }
                        }
                        Node::Shape(fields, false, _, _) => {
                            let values = value.as_hash().unwrap();
                            assert_eq!(fields.data.len(), values.len());
                            for field in &fields.data {
                                let (_, value) = values
                                    .iter()
                                    .find(|(key, _)| key.as_bytes() == field.name.as_bytes())
                                    .unwrap();
                                assert_eq!(field.value, fact_of(value));
                            }
                        }
                        _ => panic!("{params:?}: {input:?}, {value:?}"),
                    },
                    _ => panic!("{params:?}: input presence differs"),
                }
            }
        }
    }
}

fn accounting(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let types = contracts(ctx, &mut facts, program)?;
    let result = calls::analyze(
        ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program,
            contracts: &types.data,
            hosts: &[],
            globals: &[],
        },
        program.names["run"],
        &[],
    )?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.iter().any(|issue| matches!(
        issue.issue.kind,
        IssueKind::Call {
            failure: Failure::Type { .. },
            ..
        }
    )));
    Ok(())
}

#[test]
fn call_analysis_honors_exact_quotas_and_releases_all_failure_paths() {
    let program = bytecode::compile(
        "def id(x); x; end; def f(x: int, y: string:); x; end; def run; f(id(7),y: id(8)); end",
        Vec::new(),
        &(),
    )
    .unwrap();
    let mut baseline = CallContext::new(CallOptions::default());
    accounting(&mut baseline, &program).unwrap();
    let stats = baseline.stats();
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
        let result = accounting(&mut ctx, &program);
        assert_eq!(result.as_ref().err().map(|e| e.kind), error);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by(97) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            accounting(&mut ctx, &program).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn known_bad_defaults_are_checked_only_on_paths_that_evaluate_them() {
    for (call, fails) in [("f(7)", false), ("f()", true)] {
        let source = format!("def f(x: int = \"bad\"); x; end; def run -> int; {call}; end");
        check(&source, fails);
        let result =
            crate::Engine::new()
                .compile(&source)
                .unwrap()
                .call("run", &[], CallOptions::default());
        if fails {
            assert_eq!(result.unwrap_err().kind, ErrorKind::Type);
        } else {
            assert_eq!(result.unwrap().value.as_int(), Some(7));
        }
    }
}

#[test]
fn call_analysis_observes_cancellation_and_deadlines_on_empty_and_cached_paths() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let name = facts.symbol(&mut ctx, b"x").unwrap();
        let mut args = Arguments::new();
        args.keyword(&mut ctx, name, Atom::Int.fact()).unwrap();
        let other = args.snapshot(&mut ctx).unwrap();
        let program = bytecode::compile("def run; 7; end", Vec::new(), &()).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        let expected = if deadline {
            ErrorKind::Deadline
        } else {
            ErrorKind::Cancelled
        };
        assert_eq!(
            args.keyword(&mut ctx, name, Atom::Int.fact())
                .unwrap_err()
                .kind,
            expected
        );
        assert_eq!(args.snapshot(&mut ctx).unwrap_err().kind, expected);
        assert_eq!(
            args.join(&mut ctx, &mut facts, &other).unwrap_err().kind,
            expected
        );
        assert_eq!(
            Arguments::new().snapshot(&mut ctx).unwrap_err().kind,
            expected
        );
        let result = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program: &program,
                contracts: &[],
                hosts: &[],
                globals: &[],
            },
            program.names["run"],
            &[],
        );
        assert_eq!(result.unwrap_err().kind, expected);
        assert_eq!(
            Host::new(&mut ctx, &mut facts, None).unwrap_err().kind,
            expected
        );
        drop((facts, args, other));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn unfinished_callee_analysis_cannot_disappear_behind_a_never_return_summary() {
    let source = "def incomplete; [1].map! { _1 }; end; def run -> int; incomplete(); 7; end";
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(!result.incomplete.data.is_empty());
    assert_eq!(result.returns, Atom::Never.fact());
}

#[test]
fn call_reference_decisions_have_runtime_witnesses_for_each_difference() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-calls.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 52);
    let mut differences = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        check(source, case["rust_rejected"].as_bool().unwrap());
        if case["go_rejected"] != case["rust_rejected"] {
            differences += 1;
            assert!(!case["difference"].as_str().unwrap().is_empty());
            let result = crate::Engine::new().compile(source).unwrap().call(
                "run",
                &[],
                CallOptions::default(),
            );
            if let Some(value) = case["runtime"]["value"].as_i64() {
                assert_eq!(result.unwrap().value.as_int(), Some(value), "{source}");
            } else {
                let kind = match case["runtime"]["error"].as_str().unwrap() {
                    "type" => ErrorKind::Type,
                    "recursion" => ErrorKind::Recursion,
                    _ => panic!("unknown runtime expectation"),
                };
                assert_eq!(result.unwrap_err().kind, kind, "{source}");
            }
        }
    }
    assert_eq!(differences, 8);
}

#[test]
fn missing_callees_fail_before_argument_analysis() {
    for body in [
        "missing(1 + \"bad\")",
        "missing(JSON.parse(\"7\"))",
        "(missing)(1 + \"bad\")",
        "(missing)(JSON.parse(\"7\"))",
    ] {
        let source = format!("def run; {body}; end");
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, &source).unwrap();
        assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
        assert_eq!(result.issues.data.len(), 1, "{source}: {result:?}");
        assert_eq!(
            result.issues.data[0].issue.kind,
            IssueKind::Call {
                target: Target::Undefined,
                failure: Failure::Undefined,
            },
            "{source}"
        );
        let script = crate::Engine::new().compile(&source).unwrap();
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Name,
            "{source}"
        );
    }
}

#[test]
fn bare_function_reads_distinguish_attached_methods_from_opaque_roots() {
    let source = "def f -> int; 7; end; def run; f; end";
    let script = crate::Engine::new().compile(source).unwrap();
    let program = &script.inner.code.program;
    for (value, target) in [
        (Value::int(7), Target::NonCallable),
        (
            host_method().value(),
            Target::Host(super::sources::SourceId::ROOT.callable(0)),
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let types = contracts(&mut ctx, &mut facts, program).unwrap();
        let result = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program,
                contracts: &types.data,
                hosts: &[],
                globals: &[(Value::bytes(b"f"), target)],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        if target == Target::NonCallable {
            assert!(!result.incomplete.data.is_empty());
        } else {
            assert!(result.incomplete.data.is_empty(), "{result:?}");
            assert!(
                result.issues.data.iter().any(|issue| matches!(
                    issue.issue.kind,
                    super::flow::IssueKind::DetachedValue(_)
                )),
                "{result:?}"
            );
        }
        let runtime = script.call(
            "run",
            &[],
            CallOptions {
                globals: [("f".into(), value)].into_iter().collect(),
                ..CallOptions::default()
            },
        );
        if target == Target::NonCallable {
            assert_eq!(runtime.unwrap().value.as_int(), Some(7));
        } else {
            assert_eq!(runtime.unwrap_err().kind, ErrorKind::Type);
        }
    }
}

#[test]
fn missing_host_signature_types_produce_catchable_diagnostics() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for (parameter, result, calls) in [("Missing", "int", 0), ("int", "array<Missing>", 1)] {
        let counter = Arc::new(AtomicUsize::new(0));
        let observed = counter.clone();
        let method = HostMethod::new("host", move |_, _, _| {
            observed.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        })
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "x".into(),
                ty: parameter.into(),
                optional: false,
            }],
            result: result.into(),
            accepts_block: false,
        })
        .unwrap();
        let mut engine = crate::Engine::new();
        engine.register_method("host", method.clone());
        let script = engine
            .compile("def run; begin; host(7); rescue; 99; end; end")
            .unwrap();
        let program = &script.inner.code.program;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let value = method.value();
        let crate::value::Kind::Host(method) = &value.0 else {
            panic!()
        };
        let host = Host::new(&mut ctx, &mut facts, method.compiled_signature()).unwrap();
        let result = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program,
                contracts: &[],
                hosts: &[host],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(result.incomplete.data.is_empty(), "{result:?}");
        assert!(result.issues.data.iter().any(|issue| matches!(
            issue.issue.kind,
            IssueKind::Call {
                failure: Failure::HostTypeBinding { .. },
                ..
            }
        )));
        assert_eq!(result.returns, facts.integer(&mut ctx, 99).unwrap());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        assert_eq!(
            script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
                .to_string(),
            "99"
        );
        assert_eq!(counter.load(Ordering::Relaxed), calls);
    }
}
