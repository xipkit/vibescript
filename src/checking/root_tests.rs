use super::{
    arguments,
    calls::{self, Analysis, Target, World},
    facts::Facts,
    normalization_tests::observed,
    relation::Relation,
    type_bindings::Bindings,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Value, budget::Buffer, bytecode,
};

fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    globals: &[(String, Value)],
) -> Result<Analysis> {
    let mut roots = Buffer::empty();
    for (name, value) in globals {
        let fact = observed(ctx, facts, program, value);
        let name = ctx.bytes(name.as_bytes())?;
        roots.push(ctx, (name, Target::Value(fact)))?;
    }
    analyze_roots(ctx, facts, program, &roots.data)
}

fn analyze_roots(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    roots: &[(Value, Target)],
) -> Result<Analysis> {
    let mut bindings = Bindings::new();
    let scope = bindings.source(ctx, facts, program, 42)?;
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(ctx, ty, |ctx, name| {
            Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
        })?;
        contracts.push(ctx, fact)?;
    }
    let function = program.names["run"];
    let inputs = arguments::general_inputs(
        ctx,
        facts,
        &program.functions[function].params,
        &contracts.data,
    )?;
    calls::analyze(
        ctx,
        facts,
        World {
            inputs: &[],
            program,
            source_owner: 42,
            contracts: &contracts.data,
            hosts: &[],
            globals: roots,
        },
        function,
        &inputs.data,
    )
}

fn witness(
    source: &str,
    globals: &[(&str, Value)],
    args: &[Value],
    expected: &str,
    rejected: bool,
) {
    let script = Engine::new().compile(source).unwrap();
    let program = &script.inner.code.program;
    let globals: Vec<_> = globals
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    for _ in 0..2 {
        let actual = script
            .call(
                "run",
                args,
                CallOptions {
                    globals: globals.iter().cloned().collect(),
                    ..CallOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(actual.value.to_string(), expected, "{source}");
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, program, &globals).unwrap();
        let ops: Vec<_> = report
            .incomplete
            .data
            .iter()
            .map(|&(function, pc)| (function, pc, program.functions[function].code[pc]))
            .collect();
        assert!(ops.is_empty(), "{source}: {report:?}; unsupported: {ops:?}");
        assert_eq!(
            !report.issues.data.is_empty(),
            rejected,
            "{source}: {report:?}"
        );
        let concrete = observed(&mut ctx, &mut facts, program, &actual.value);
        assert_ne!(
            facts.relation(&mut ctx, concrete, report.returns).unwrap(),
            Relation::Rejected,
            "{source}: {report:?}; actual: {:?}; inferred: {:?}",
            facts.node(concrete),
            facts.node(report.returns)
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn array() -> Value {
    Value::array(vec![Value::int(1)])
}

fn descriptor(source: &str) -> Value {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}

#[test]
fn roots_share_live_values_across_functions_and_keep_copies_isolated() {
    for (source, expected) in [
        (
            "def change; items.push(2); end; def run; before=items; change(); [before,items]; end",
            "[[1], [1, 2]]",
        ),
        (
            "def change; items=[7]; end; def run; change(); items; end",
            "[7]",
        ),
        (
            "def change; items.clear; end; def run; items.push(2); change(); items; end",
            "[]",
        ),
        (
            "def change; items[0]=7; end; def run; change(); items[0]; end",
            "7",
        ),
    ] {
        let original = array();
        witness(source, &[("items", original.clone())], &[], expected, false);
        assert_eq!(original.to_string(), "[1]");
    }
}

#[test]
fn root_assignments_and_nested_block_mutations_use_the_nearest_binding() {
    for (source, expected) in [
        ("def run; 2.times { count += 1 }; count; end", "3"),
        (
            "def run; [1].each { [1].each { count += 1 } }; count; end",
            "2",
        ),
        ("def run; [7].each {|count| count += 1}; count; end", "1"),
        (
            "def run; [7].each {|count| [1].each { count=9 }}; count; end",
            "1",
        ),
        (
            "def helper(count:int); [1].each { count += 1 }; count; end; def run; [helper(7),count]; end",
            "[8, 1]",
        ),
        ("def run; count=7; [1].each { count=9 }; count; end", "9"),
    ] {
        witness(source, &[("count", Value::int(1))], &[], expected, false);
    }
}

#[test]
fn root_values_override_functions_declarations_and_builtins_even_when_nil() {
    for (source, globals, expected) in [
        (
            "def helper; 9; end; def run; helper; end",
            vec![("helper", Value::int(7))],
            "7",
        ),
        (
            "class Box; end; def run; Box; end",
            vec![("Box", Value::int(7))],
            "7",
        ),
        (
            "enum Status; Draft; end; def change; Status=7; end; def run; change(); Status; end",
            vec![("Status", Value::nil())],
            "7",
        ),
        (
            "class Box; end; def change; Box=7; end; def run; change(); Box; end",
            vec![("Box", Value::nil())],
            "7",
        ),
        (
            "enum Status; Draft; end; def run; Status; end",
            vec![("Status", Value::nil())],
            "nil",
        ),
        ("def run; Math; end", vec![("Math", Value::int(7))], "7"),
        (
            "def run; count ||= 7; count; end",
            vec![("count", Value::nil())],
            "7",
        ),
        (
            "def run; to_int=7; to_int; end",
            vec![("to_int", Value::nil())],
            "7",
        ),
    ] {
        witness(source, &globals, &[], expected, false);
    }
}

#[test]
fn root_call_targets_are_selected_before_arguments_mutate_the_binding() {
    let parse = descriptor("JSON::parse");
    for name in ["helper", "to_int"] {
        let source = format!(
            "def replace; {name}=7; \"3\"; end; def run; result={name}(replace()); [result,{name}]; end"
        );
        witness(&source, &[(name, parse.clone())], &[], "[3, 7]", false);
    }
    witness(
        "def run; result=helper((begin; helper=7; \"3\"; end)); [result,helper]; end",
        &[("helper", parse)],
        &[],
        "[3, 7]",
        false,
    );
}

#[test]
fn root_calls_and_descriptor_reads_keep_runtime_errors_catchable() {
    for (source, value) in [
        (
            "def run; begin; helper(); rescue; 7; end; end",
            Value::nil(),
        ),
        (
            "def run; begin; helper; rescue; 7; end; end",
            descriptor("JSON::parse"),
        ),
        (
            "def run; helper=7; begin; helper(); rescue; helper; end; end",
            descriptor("JSON::parse"),
        ),
    ] {
        witness(source, &[("helper", value)], &[], "7", true);
    }
}

#[test]
fn root_addresses_survive_calls_and_negative_index_growth() {
    for (source, expected) in [
        (
            "def grow; items.push(2); 7; end; def run; items[-1]=grow(); items; end",
            "[1, 7]",
        ),
        (
            "def grow; items.push(2); 7; end; def run; items[-1] += grow(); items; end",
            "[8, 2]",
        ),
        (
            "def run; items[-1] += [7].map {|x| items.push(2); x}.first; items; end",
            "[8, 2]",
        ),
    ] {
        witness(source, &[("items", array())], &[], expected, false);
    }
}

#[test]
fn root_writes_survive_rescue_ensure_and_nonlocal_block_returns() {
    for (source, expected) in [
        (
            "def change; items.push(2); raise \"bad\"; end; def run; begin; change(); rescue; items; end; end",
            "[1, 2]",
        ),
        (
            "def change; begin; return 7; ensure; items.push(2); end; end; def run; [change(),items]; end",
            "[7, [1, 2]]",
        ),
        (
            "def change; [7].each {|x| items.push(2); return x}; end; def run; [change(),items]; end",
            "[7, [1, 2]]",
        ),
        (
            "def run; [7].each {|x| items.push(2); break x}; items; end",
            "[1, 2]",
        ),
    ] {
        witness(source, &[("items", array())], &[], expected, false);
    }
}

#[test]
fn roots_keep_conditional_values_and_callable_choices() {
    for flag in [false, true] {
        witness(
            "def change(flag:bool); items.push(2) if flag; end; def run(flag:bool); change(flag); items; end",
            &[("items", array())],
            &[Value::boolean(flag)],
            if flag { "[1, 2]" } else { "[1]" },
            false,
        );
        witness(
            "def run(flag:bool); helper=flag ? JSON::parse : 7; begin; helper(\"3\"); rescue; 9; end; end",
            &[("helper", Value::nil())],
            &[Value::boolean(flag)],
            if flag { "3" } else { "9" },
            true,
        );
    }
}

#[test]
fn root_objects_keep_namespace_dispatch_and_nested_value_semantics() {
    let object = descriptor("Math.clear; Math[:items]=[1]; Math");
    witness(
        "def change; settings.items.push(2); end; def run; before=settings; change(); [before.items,settings.items]; end",
        &[("settings", object)],
        &[],
        "[[1], [1, 2]]",
        false,
    );
}

#[test]
fn root_rescue_bindings_shadow_temporarily_and_reveal_the_live_root_afterwards() {
    for (source, expected) in [
        (
            "def run; begin; raise \"bad\"; rescue => count; count.message; end; count; end",
            "1",
        ),
        (
            "def change; count=7; end; def run; begin; raise \"bad\"; rescue => count; change(); end; count; end",
            "7",
        ),
        ("def run; begin; 7; rescue; count=9; end; count; end", "1"),
        (
            "def run; begin; count=7; ensure; count+=2; end; count; end",
            "9",
        ),
    ] {
        witness(source, &[("count", Value::int(1))], &[], expected, false);
    }
}

#[test]
fn root_type_bindings_follow_mutations_across_calls() {
    let enumeration = descriptor("enum HostState; Draft; end; HostState");
    witness(
        "def echo(value:Alias); value.symbol; end; def run; echo(:draft); end",
        &[("Alias", enumeration.clone())],
        &[],
        "draft",
        false,
    );
    witness(
        "enum Status; Sent; end; def change; Alias=Status; end; def echo(value:Alias); value.symbol; end; def run; before=echo(:draft); change(); [before,echo(:sent)]; end",
        &[("Alias", enumeration)],
        &[],
        "[draft, sent]",
        false,
    );
    witness(
        "enum Status; Draft; end; def echo(value:Status); value; end; def run; begin; echo(:draft); rescue; 7; end; end",
        &[("Status", Value::int(7))],
        &[],
        "7",
        true,
    );
}

#[test]
fn forwarded_blocks_share_roots_without_overwriting_captured_parameters() {
    for (source, expected) in [
        (
            "def invoke; yield 7; end; def relay; invoke { yield _1 }; end; def run; relay {|x| count+=x}; count; end",
            "8",
        ),
        (
            "def invoke; yield 7; end; def helper(count:int); invoke {|x| count+=x}; count; end; def run; [helper(3),count]; end",
            "[10, 1]",
        ),
        (
            "def invoke; count=7; yield; count; end; def run; invoke { count+=2 }; count; end",
            "9",
        ),
    ] {
        witness(source, &[("count", Value::int(1))], &[], expected, false);
    }
}

#[test]
fn root_analysis_never_executes_shadowed_host_callbacks() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let mut engine = Engine::new();
    engine.register("host", move |_, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(9))
    });
    let script = engine
        .compile("def run; begin; host(); rescue; 7; end; end")
        .unwrap();
    let globals = [("host".to_owned(), Value::int(1))];
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, &script.inner.code.program, &globals).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(!report.issues.data.is_empty(), "{report:?}");
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        script
            .call(
                "run",
                &[],
                CallOptions {
                    globals: globals.into_iter().collect(),
                    ..CallOptions::default()
                }
            )
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting_program() -> bytecode::Program {
    bytecode::compile("def grow; items.push(2); count+=1; 7; end; def run(flag:bool); items[-1]+=grow(); [1].each { count+=1 }; begin; count=9 if flag; ensure; items.push(count); end; [items,count]; end", Vec::new(), &()).unwrap()
}

fn work(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let one = facts.integer(ctx, 1)?;
    let items = facts.tuple(ctx, &[one])?;
    let roots = [
        (ctx.bytes(b"items")?, Target::Value(items)),
        (ctx.bytes(b"count")?, Target::Value(one)),
    ];
    let report = analyze_roots(ctx, &mut facts, program, &roots)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn root_analysis_accounts_for_all_state_and_releases_interrupted_work() {
    let program = accounting_program();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &program).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, kind) in [
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
        assert_eq!(work(&mut ctx, &program).err().map(|e| e.kind), kind);
        if let Some(kind) = kind {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..24 {
        for memory in [false, true] {
            let (limits, kind) = if memory {
                (
                    Limits {
                        memory_bytes: Some(stats.peak_memory_bytes * sample / 24),
                        ..Limits::default()
                    },
                    ErrorKind::Memory,
                )
            } else {
                (
                    Limits {
                        steps: Some(stats.steps * sample as u64 / 24),
                        ..Limits::default()
                    },
                    ErrorKind::Steps,
                )
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(work(&mut ctx, &program).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn root_analysis_keeps_cancellation_and_deadlines_latched() {
    let program = accounting_program();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        work(&mut ctx, &program).unwrap();
        let kind = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        assert_eq!(work(&mut ctx, &program).unwrap_err().kind, kind);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn root_nested_addresses_keep_the_selected_element_during_parent_growth() {
    let items = Value::array(vec![array()]);
    witness(
        "def grow; items.push([2]); 3; end; def run; items[-1].push(grow()); items; end",
        &[("items", items.clone())],
        &[],
        "[[1, 3], [2]]",
        false,
    );
    witness(
        "def run; items[-1].push([3].map {|x| items.push([2]); x}.first); items; end",
        &[("items", items)],
        &[],
        "[[1, 3], [2]]",
        false,
    );
}

#[test]
fn copying_root_values_into_arguments_and_callback_bindings_detaches_mutations() {
    witness(
        "def change(value:array<int>); value.push(2); value; end; def run; [change(items),items]; end",
        &[("items", array())],
        &[],
        "[[1, 2], [1]]",
        false,
    );
    witness(
        "def run; values=[items].map {|value| value.push(2)}; [values,items]; end",
        &[("items", array())],
        &[],
        "[[[1, 2]], [1]]",
        false,
    );
}

#[test]
fn declared_host_reads_observe_supplied_values_and_keep_methods_attached() {
    let mut engine = Engine::new();
    engine.register("host", |_, _| panic!("analysis executed a host callback"));
    for (body, roots, rejected, expected) in [
        ("host", vec![("host".to_owned(), Value::int(7))], false, "7"),
        (
            "host=9; host",
            vec![("host".to_owned(), Value::int(7))],
            false,
            "9",
        ),
        ("begin; host; rescue; 7; end", vec![], true, "7"),
    ] {
        let script = engine.compile(&format!("def run; {body}; end")).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &script.inner.code.program, &roots).unwrap();
        assert!(report.incomplete.data.is_empty(), "{body}: {report:?}");
        assert_eq!(
            !report.issues.data.is_empty(),
            rejected,
            "{body}: {report:?}"
        );
        let actual = script
            .call(
                "run",
                &[],
                CallOptions {
                    globals: roots.into_iter().collect(),
                    ..CallOptions::default()
                },
            )
            .unwrap()
            .value;
        assert_eq!(actual.to_string(), expected);
        let concrete = observed(&mut ctx, &mut facts, &script.inner.code.program, &actual);
        assert_ne!(
            facts.relation(&mut ctx, concrete, report.returns).unwrap(),
            Relation::Rejected
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
