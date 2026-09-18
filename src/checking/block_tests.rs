use super::{
    blocks::{self, Completion},
    calls::Unavailable,
    collection_tests::literal_fact,
    facts::{Atom, Facts},
    flow::{self, Body},
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine, ErrorClass, Value, budget::Buffer, bytecode};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Observed {
    normal: Option<Value>,
    after: bool,
    captures: Vec<Value>,
}

fn witness(
    params: &str,
    args: &[(&str, Value)],
    body: &str,
    captures: &[(&str, &str, Value)],
    expected: Completion,
    given: bool,
) {
    let mut source = format!(
        "def once; result=yield({}); record_normal(result); result; end; def run; ",
        args.iter().map(|a| a.0).collect::<Vec<_>>().join(",")
    );
    for (name, expression, _) in captures {
        source.push_str(&format!("{name}={expression}; "));
    }
    source.push_str(&format!(
        "begin; result=once {{{params} {body}}}; record_after(); result; ensure; record_captures({}); end; end; def entry; {}; end",
        captures.iter().map(|c| c.0).collect::<Vec<_>>().join(","),
        if given { "run {7}" } else { "run()" }
    ));
    let observed = Arc::new(Mutex::new(Observed::default()));
    let mut engine = Engine::new();
    let recorded = observed.clone();
    engine.register("record_normal", move |_, args| {
        recorded.lock().unwrap().normal = Some(args[0].clone());
        Ok(Value::nil())
    });
    let recorded = observed.clone();
    engine.register("record_after", move |_, _| {
        recorded.lock().unwrap().after = true;
        Ok(Value::nil())
    });
    let recorded = observed.clone();
    engine.register("record_captures", move |_, args| {
        recorded.lock().unwrap().captures = args.to_vec();
        Ok(Value::nil())
    });
    let runtime = engine
        .compile(&source)
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .call("entry", &[], CallOptions::default());
    let observed = observed.lock().unwrap();
    let actual = match expected {
        Completion::Error(class) => {
            let error = runtime.unwrap_err();
            assert_eq!(error.class(), Some(class), "{source}: {error}");
            assert!(observed.normal.is_none(), "{source}");
            assert!(!observed.after, "{source}");
            Value::nil()
        }
        _ => {
            let result = runtime.unwrap_or_else(|e| panic!("{source}: {e}"));
            assert_eq!(
                observed.normal.is_some(),
                expected == Completion::Value,
                "{source}"
            );
            assert_eq!(
                observed.after,
                expected != Completion::Return(0),
                "{source}"
            );
            result.value
        }
    };

    let program = bytecode::compile(&source, Vec::new(), &()).unwrap();
    let function = program.functions[program.names["run"]]
        .code
        .iter()
        .find_map(|op| {
            if let bytecode::Op::Attach(i) = op {
                Some(*i)
            } else {
                None
            }
        })
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    ctx.output_writer = Some(Arc::new(|_, _| panic!("analysis executed output")));
    ctx.random_source = Some(Arc::new(|_, _| panic!("analysis executed entropy")));
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut inputs = Buffer::empty();
    let mut variables = Buffer::empty();
    for (_, arg) in args {
        let fact = literal_fact(&mut ctx, &mut facts, arg);
        inputs.push(&mut ctx, fact).unwrap();
    }
    for (name, _, value) in captures {
        if let Some(slot) = program.functions[function]
            .local_names
            .iter()
            .position(|n| n == name)
        {
            let value = literal_fact(&mut ctx, &mut facts, value);
            variables
                .push(
                    &mut ctx,
                    blocks::Capture {
                        slot,
                        value,
                        missing: false,
                        owner: blocks::Owner::Unknown,
                    },
                )
                .unwrap();
        }
    }
    let pending = super::pending::Pending::new();
    let input = blocks::Inputs {
        pending: &pending,
        arguments: &inputs.data,
        captures: &variables.data,
        given,
        inherited: &[],
    };
    let report = flow::analyze_body(
        &mut ctx,
        &mut facts,
        Body {
            program: &program,
            function,
            contracts: &[],
            inputs: &[],
            current_error: flow::NO_ERROR,
            block: Some(&input),
            incoming: None,
            layouts: None,
        },
        &mut Unavailable,
    )
    .unwrap_or_else(|e| panic!("{source}: {e}"));
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    let actual = literal_fact(&mut ctx, &mut facts, &actual);
    let mut covered = false;
    for exit in &report.block_exits.data {
        if exit.completion != expected {
            continue;
        }
        let mut compatible = matches!(expected, Completion::Error(_))
            || facts.relation(&mut ctx, actual, exit.value).unwrap() != Relation::Rejected;
        for (index, (name, _, _)) in captures.iter().enumerate() {
            if let Some(slot) = program.functions[function]
                .local_names
                .iter()
                .position(|n| n == name)
            {
                let actual = literal_fact(&mut ctx, &mut facts, &observed.captures[index]);
                let inferred = exit.captures.get(&mut ctx, slot).unwrap();
                compatible &=
                    facts.relation(&mut ctx, actual, inferred).unwrap() != Relation::Rejected;
            }
        }
        covered |= compatible;
    }
    assert!(covered, "{source}: {report:?}");
    drop((report, variables, inputs, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn block_arguments_bind_explicit_implicit_and_destructured_parameters() {
    for (params, body) in [
        ("|x|", "x"),
        ("|x,y|", "[x,y]"),
        ("|(x,y)|", "[x,y]"),
        ("|(*rest)|", "rest"),
        ("", "[_1,_2,_9]"),
        ("", "it"),
    ] {
        for args in [
            vec![],
            vec![("7", Value::int(7))],
            vec![("7", Value::int(7)), ("9", Value::int(9))],
            vec![("[7,9]", Value::array(vec![Value::int(7), Value::int(9)]))],
            vec![("nil", Value::nil())],
        ] {
            witness(params, &args, body, &[], Completion::Value, false);
        }
    }
}

#[test]
fn block_parameter_shadowing_preserves_the_original_capture() {
    for (params, body) in [("|x|", "x=9; x"), ("|(x,rest)|", "x=9; x"), ("", "it")] {
        let name = if body == "it" { "it" } else { "x" };
        witness(
            params,
            &[("7", Value::int(7))],
            body,
            &[(name, "\"outer\"", Value::bytes(b"outer"))],
            Completion::Value,
            false,
        );
    }
}

#[test]
fn captured_collection_writes_and_copies_survive_block_completion() {
    for body in [
        "x.push(2); x",
        "copy=x; copy.push(2); [copy,x]",
        "x[0]+=2; x",
        "x[-1]+= (while true; x.push(9); break 2; end); x",
        "x[0] ||= 9; x",
        "begin; x[99]=7; rescue; x.push(2); end; x",
    ] {
        witness(
            "",
            &[],
            body,
            &[("x", "[1]", Value::array(vec![Value::int(1)]))],
            Completion::Value,
            false,
        );
    }
}

#[test]
fn block_nonlocal_controls_keep_distinct_exit_kinds() {
    for (body, expected) in [
        ("x=2; 7", Completion::Value),
        ("x=2; next 7; missing", Completion::Value),
        ("x=2; next; missing", Completion::Value),
        ("x=2; break 7; missing", Completion::Break(true)),
        ("x=2; break; missing", Completion::Break(false)),
        ("x=2; return 7; missing", Completion::Return(0)),
        ("x=2; raise \"bad\"", Completion::Error(ErrorClass::Runtime)),
    ] {
        witness("", &[], body, &[("x", "1", Value::int(1))], expected, false);
    }
}

#[test]
fn ensure_writes_follow_the_surviving_block_control() {
    for (body, expected) in [
        ("begin; x=2; 7; ensure; x=3; end", Completion::Value),
        ("begin; x=2; next 7; ensure; x=3; end", Completion::Value),
        (
            "begin; x=2; break 7; ensure; x=3; end",
            Completion::Break(true),
        ),
        (
            "begin; x=2; return 7; ensure; x=3; end",
            Completion::Return(0),
        ),
        (
            "begin; x=2; raise \"bad\"; ensure; x=3; end",
            Completion::Error(ErrorClass::Runtime),
        ),
        (
            "begin; return 7; ensure; x=3; next 9; end",
            Completion::Value,
        ),
        (
            "begin; break 7; ensure; x=3; return 9; end",
            Completion::Return(0),
        ),
        (
            "begin; next 7; ensure; x=3; break 9; end",
            Completion::Break(true),
        ),
        (
            "begin; return 7; ensure; x=3; raise \"bad\"; end",
            Completion::Error(ErrorClass::Runtime),
        ),
        (
            "begin; raise \"bad\"; ensure; x=3; return 9; end",
            Completion::Return(0),
        ),
    ] {
        witness("", &[], body, &[("x", "1", Value::int(1))], expected, false);
    }
}

#[test]
fn block_local_loops_keep_their_own_control_targets() {
    for body in [
        "while x<4; x+=1; if x==2; next 9; end; if x==3; break 7; end; end; x",
        "for n in [2,3]; x+=n; next; end; x",
        "while true; begin; x=2; break 7; ensure; x=3; end; end; x",
        "begin; raise \"bad\"; rescue; x+=1; if x<3; retry; end; x; end",
        "begin; raise \"bad\"; rescue => error; x=2; error.message; end",
    ] {
        witness(
            "",
            &[],
            body,
            &[("x", "1", Value::int(1))],
            Completion::Value,
            false,
        );
    }
}

#[test]
fn block_presence_uses_the_lexical_owners_incoming_block() {
    for given in [false, true] {
        witness("", &[], "block_given?", &[], Completion::Value, given);
        witness("", &[], "block_given?()", &[], Completion::Value, given);
    }
    for body in ["block_given?(missing)", "block_given? {missing}"] {
        // The attached block case needs the outer block's bytecode, not its nested body.
        witness(
            "",
            &[],
            body,
            &[],
            Completion::Error(ErrorClass::Runtime),
            false,
        );
    }
    witness(
        "",
        &[],
        "yield missing",
        &[],
        Completion::Error(ErrorClass::LocalJump),
        false,
    );
}

#[test]
fn ordinary_functions_distinguish_missing_blocks_and_skip_yield_arguments() {
    super::native_tests::witness("def run -> bool; block_given?; end", Some("false"), false);
    super::native_tests::witness(
        "def run; begin; yield missing; rescue LocalJumpError; 7; end; end",
        Some("7"),
        true,
    );
    super::native_tests::witness(
        "def run; x=0; begin; block_given?((while true; x=7; break 9; end)); rescue; x; end; end",
        Some("0"),
        true,
    );
}

#[test]
fn block_cleanup_control_matrix_matches_execution() {
    let bodies = [
        ("7", Completion::Value),
        ("next 7", Completion::Value),
        ("break 7", Completion::Break(true)),
        ("break", Completion::Break(false)),
        ("return 7", Completion::Return(0)),
        ("raise \"body\"", Completion::Error(ErrorClass::Runtime)),
    ];
    let cleanups = [
        ("9", None),
        ("next 9", Some(Completion::Value)),
        ("break 9", Some(Completion::Break(true))),
        ("break", Some(Completion::Break(false))),
        ("return 9", Some(Completion::Return(0))),
        (
            "raise \"cleanup\"",
            Some(Completion::Error(ErrorClass::Runtime)),
        ),
    ];
    for (body, completion) in bodies {
        for (cleanup, replacement) in cleanups {
            witness(
                "",
                &[],
                &format!("begin; x=2; {body}; ensure; x=3; {cleanup}; end"),
                &[("x", "1", Value::int(1))],
                replacement.unwrap_or(completion),
                false,
            );
        }
    }
}

#[test]
fn captured_writes_survive_each_ordinary_exception_class() {
    for class in [
        ErrorClass::Runtime,
        ErrorClass::Standard,
        ErrorClass::Assertion,
        ErrorClass::Limit,
        ErrorClass::Type,
        ErrorClass::ZeroDivision,
        ErrorClass::LocalJump,
        ErrorClass::Argument,
    ] {
        witness(
            "",
            &[],
            &format!(
                "begin; x=2; raise {}, \"body\"; ensure; x=3; end",
                class.name()
            ),
            &[("x", "1", Value::int(1))],
            Completion::Error(class),
            false,
        );
    }
    witness(
        "",
        &[],
        "x=2; /a/.match((\"x\"*1024)*1025)",
        &[("x", "1", Value::int(1))],
        Completion::Error(ErrorClass::Limit),
        false,
    );
    witness(
        "",
        &[],
        "x=2; begin; \"a\".match(\"[\"); rescue => error; x=3; error.backtrace.clear; end",
        &[("x", "1", Value::int(1))],
        Completion::Error(ErrorClass::Runtime),
        false,
    );
}

#[test]
fn rescue_bindings_and_failed_writes_preserve_live_captures() {
    witness(
        "",
        &[],
        "begin; raise \"bad\"; rescue => error; x=2; error.message; end; error",
        &[("x", "1", Value::int(1)), ("error", "99", Value::int(99))],
        Completion::Value,
        false,
    );
    witness(
        "",
        &[],
        "begin; x[\"bad\"]=(while true; x.push(2); break 9; end); rescue; x; end",
        &[("x", "[1]", Value::array(vec![Value::int(1)]))],
        Completion::Value,
        false,
    );
    witness(
        "",
        &[],
        "begin; x[0].push((while true; x[0]=[8]; break 9; end)); rescue; x; end",
        &[(
            "x",
            "[[1]]",
            Value::array(vec![Value::array(vec![Value::int(1)])]),
        )],
        Completion::Value,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run; x=[1]; once {|flag| begin; x.push(2); if flag; return x; else; raise \"bad\"; end; ensure; x.push(3); end}; end";
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let function = program
        .functions
        .iter()
        .position(|f| f.name == "<block>")
        .unwrap();
    let slot = program.functions[function]
        .local_names
        .iter()
        .position(|n| n == "x")
        .unwrap();
    let mut facts = Facts::new(ctx)?;
    let one = facts.integer(ctx, 1)?;
    let value = facts.tuple(ctx, &[one])?;
    let captures = [blocks::Capture {
        slot,
        value,
        missing: false,
        owner: blocks::Owner::Unknown,
    }];
    let pending = super::pending::Pending::new();
    let input = blocks::Inputs {
        pending: &pending,
        arguments: &[Atom::Bool.fact()],
        captures: &captures,
        given: false,
        inherited: &[],
    };
    let report = flow::analyze_body(
        ctx,
        &mut facts,
        Body {
            program: &program,
            function,
            contracts: &[],
            inputs: &[],
            current_error: flow::NO_ERROR,
            block: Some(&input),
            incoming: None,
            layouts: None,
        },
        &mut Unavailable,
    )?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(
        report
            .block_exits
            .data
            .iter()
            .any(|e| e.completion == Completion::Return(0))
    );
    assert!(
        report
            .block_exits
            .data
            .iter()
            .any(|e| e.completion == Completion::Error(ErrorClass::Runtime))
    );
    Ok(())
}

#[test]
fn block_capture_and_exit_storage_obey_exact_quotas_and_reclaim_failures() {
    use crate::{ErrorKind, Limits};
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
    for memory in (0..stats.peak_memory_bytes).step_by(127) {
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
    for steps in (0..stats.steps).step_by(127) {
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
fn block_analysis_preserves_latched_cancellation_and_deadlines() {
    use crate::ErrorKind;
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

#[test]
fn native_calls_and_nested_yields_remain_incomplete_until_solver_integration() {
    {
        let source = "def run; [1].map! {|x| x}; end";
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = super::collection_tests::analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty());
    }
    let program = bytecode::compile("def run; once {yield 7}; end", Vec::new(), &()).unwrap();
    let function = program
        .functions
        .iter()
        .position(|f| f.name == "<block>")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let pending = super::pending::Pending::new();
    let input = blocks::Inputs {
        pending: &pending,
        arguments: &[],
        captures: &[],
        given: true,
        inherited: &[],
    };
    let report = flow::analyze_body(
        &mut ctx,
        &mut facts,
        Body {
            program: &program,
            function,
            contracts: &[],
            inputs: &[],
            current_error: flow::NO_ERROR,
            block: Some(&input),
            incoming: None,
            layouts: None,
        },
        &mut Unavailable,
    )
    .unwrap();
    assert!(!report.incomplete.data.is_empty());
}

#[test]
fn captured_writes_remain_separate_for_different_nonlocal_exits() {
    let program = bytecode::compile(
        "def run; x=1; once {if _1; x=2; return 7; else; x=3; break 9; end}; end",
        Vec::new(),
        &(),
    )
    .unwrap();
    let function = program
        .functions
        .iter()
        .position(|f| f.name == "<block>")
        .unwrap();
    let slot = program.functions[function]
        .local_names
        .iter()
        .position(|n| n == "x")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let value = facts.integer(&mut ctx, 1).unwrap();
    let captures = [blocks::Capture {
        slot,
        value,
        missing: false,
        owner: blocks::Owner::Unknown,
    }];
    let pending = super::pending::Pending::new();
    let input = blocks::Inputs {
        pending: &pending,
        arguments: &[Atom::Bool.fact()],
        captures: &captures,
        given: false,
        inherited: &[],
    };
    let report = flow::analyze_body(
        &mut ctx,
        &mut facts,
        Body {
            program: &program,
            function,
            contracts: &[],
            inputs: &[],
            current_error: flow::NO_ERROR,
            block: Some(&input),
            incoming: None,
            layouts: None,
        },
        &mut Unavailable,
    )
    .unwrap();
    assert!(report.incomplete.data.is_empty());
    assert!(report.issues.data.is_empty());
    assert_eq!(report.returns, Atom::Never.fact());
    assert_eq!(report.throws, 0);
    assert_eq!(report.block_exits.data.len(), 2);
    for (completion, value, written) in [
        (Completion::Return(0), 7, 2),
        (Completion::Break(true), 9, 3),
    ] {
        let exit = report
            .block_exits
            .data
            .iter()
            .find(|e| e.completion == completion)
            .unwrap();
        assert_eq!(exit.value, facts.integer(&mut ctx, value).unwrap());
        assert_eq!(
            exit.captures.get(&mut ctx, slot).unwrap(),
            facts.integer(&mut ctx, written).unwrap()
        );
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn block_presence_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-blocks.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 12);
    let mut differences = 0;
    for case in cases {
        super::native_tests::witness(
            case["source"].as_str().unwrap(),
            case["runtime"]["display"].as_str(),
            case["rust_rejected"].as_bool().unwrap(),
        );
        if case["go_rejected"] != case["rust_rejected"] {
            assert!(!case["difference"].as_str().unwrap().is_empty());
            differences += 1;
        }
    }
    assert_eq!(differences, 7);
}

#[test]
fn block_controls_preserve_completed_argument_effects_without_publishing_pending_writes() {
    for (control, completion) in [
        ("next 7", Completion::Value),
        ("break 7", Completion::Break(true)),
        ("return 7", Completion::Return(0)),
        ("raise \"bad\"", Completion::Error(ErrorClass::Runtime)),
    ] {
        for expression in [
            format!("x.push((begin; x[0]=2; {control}; end))"),
            format!("x[0]=(begin; x.push(2); {control}; end)"),
            format!("format(\"%s\",(begin; x.push(2); {control}; end))"),
        ] {
            witness(
                "",
                &[],
                &expression,
                &[("x", "[1]", Value::array(vec![Value::int(1)]))],
                completion,
                false,
            );
        }
    }
}
