use super::{
    arguments,
    calls::{self, Host, Target, World},
    facts::{Atom, Callable, Fact, Facts, HashKind},
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Signature,
    SignatureParam, Value, budget::Buffer, bytecode, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn method(counter: &Arc<AtomicUsize>) -> HostMethod {
    let counter = counter.clone();
    HostMethod::new("sms.deliver", move |_, args, _| {
        counter.fetch_add(1, Ordering::Relaxed);
        Ok(args[0].clone())
    })
    .with_signature(Signature {
        params: vec![SignatureParam {
            name: "message".into(),
            ty: "int".into(),
            optional: false,
        }],
        result: "int".into(),
        accepts_block: false,
    })
    .unwrap()
}

pub(super) fn admitted(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    hosts: &mut Buffer<Host>,
    value: &Value,
) -> Result<Fact> {
    let admitted = super::admission::value(ctx, facts, value, |ctx, facts, value| {
        Ok(Some(match &value.0 {
            Kind::Host(method) => {
                let index = hosts.data.len();
                let host = Host::new(ctx, facts, method.compiled_signature())?;
                hosts.push(ctx, host)?;
                facts.callable(ctx, 0, Callable::Host(index))?
            }
            Kind::Function(function) => {
                let owner = if std::ptr::eq(&function.code.program, program) {
                    0
                } else {
                    99
                };
                facts.callable(ctx, owner, Callable::Function(function.index))?
            }
            _ => return Ok(None),
        }))
    })?;
    assert!(!admitted.incomplete, "unsupported admitted test value");
    Ok(admitted.value)
}

fn witness(body: &str, expected: &str, rejected: bool) {
    let counter = Arc::new(AtomicUsize::new(0));
    let method = method(&counter);
    let object = Value::object(vec![
        (b"deliver".to_vec(), method.value()),
        (b"number".to_vec(), Value::int(7)),
    ]);
    let source = format!("def run; {body}; end");
    let script = Engine::new().compile(&source).unwrap();
    let program = &script.inner.code.program;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut hosts = Buffer::empty();
    let object_fact = admitted(&mut ctx, &mut facts, program, &mut hosts, &object).unwrap();
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            program,
            source_owner: 0,
            contracts: &[],
            hosts: &hosts.data,
            globals: &[(Value::bytes(b"sms"), Target::Value(object_fact))],
        },
        program.names["run"],
        &[],
    )
    .unwrap();
    assert_eq!(
        counter.load(Ordering::Relaxed),
        0,
        "analysis executed a callback"
    );
    let ops: Vec<_> = report
        .incomplete
        .data
        .iter()
        .map(
            |&super::calls::Location {
                 function: f, pc, ..
             }| (f, pc, program.functions[f].code[pc]),
        )
        .collect();
    assert!(ops.is_empty(), "{source}: {report:?}; unsupported: {ops:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    let value = script
        .call(
            "run",
            &[],
            CallOptions {
                globals: [("sms".into(), object.clone())].into_iter().collect(),
                ..CallOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .value;
    assert_eq!(value.to_string(), expected, "{source}");
    let concrete = observed(&mut ctx, &mut facts, program, &value);
    assert_ne!(
        facts.relation(&mut ctx, concrete, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}; observed: {:?}; inferred: {:?}",
        facts.node(concrete),
        facts.node(report.returns)
    );
    drop((report, hosts, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn attached_host_methods_support_direct_scoped_indexed_and_forwarded_calls() {
    for expression in [
        "sms.deliver(7)",
        "sms::deliver(7)",
        "sms[:deliver](7)",
        "sms.send(:deliver,7)",
        "sms.public_send(:deliver,7)",
        "sms.deliver(*[7])",
    ] {
        witness(expression, "7", false);
    }
}

#[test]
fn attached_host_methods_cannot_be_read_stored_or_passed_as_values() {
    for expression in [
        "sms.deliver",
        "sms::deliver",
        "sms[:deliver]",
        "saved=sms[:deliver]",
        "[sms::deliver]",
        "sms[:deliver].to_s",
    ] {
        witness(&format!("begin; {expression}; rescue; 9; end"), "9", true);
    }
}

#[test]
fn host_method_signatures_validate_arguments_without_executing_callbacks() {
    for expression in [
        "sms.deliver()",
        "sms.deliver(1,2)",
        "sms.deliver(\"bad\")",
        "sms.deliver(7,bad:1)",
    ] {
        witness(&format!("begin; {expression}; rescue; 9; end"), "9", true);
    }
}

#[test]
fn attached_calls_capture_the_target_before_arguments_replace_the_field() {
    witness(
        "result=sms.deliver((begin; sms[:deliver]=9; 7; end)); [result,sms[:deliver]]",
        "[7, 9]",
        false,
    );
    witness(
        "result=sms[:deliver]((begin; sms.clear; 7; end)); [result,sms.size]",
        "[7, 0]",
        false,
    );
}

#[test]
fn collection_results_cannot_detach_host_methods_from_objects() {
    for expression in [
        "sms.values",
        "sms.to_a",
        "sms.merge({})",
        "sms.select {|k,v| true}",
        "sms.map {|k,v| v}",
        "sms.each { 1 }",
        "sms.each_value { 1 }",
    ] {
        witness(
            &format!("seen=[]; begin; {expression}; rescue; seen.push(9); end; seen"),
            "[9]",
            true,
        );
    }
    witness(
        "seen=[]; begin; sms.each { seen.push(1) }; rescue; seen.push(9); end; seen",
        "[9]",
        true,
    );
    witness("sms.each_key {|k| k}; sms.keys.size", "2", false);
}

#[test]
fn namespace_copies_keep_attached_methods_and_json_rejects_them() {
    witness("copy=sms.dup; copy.deliver(7)", "7", false);
    witness("values=[sms]; values[0].deliver(7)", "7", false);
    witness("begin; JSON.stringify(sms); rescue; 9; end", "9", true);
}

#[test]
fn exported_fact_projection_preserves_objects_and_filters_nested_data_iteratively() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let callable = facts.callable(&mut ctx, 0, Callable::Host(0)).unwrap();
    let maybe = facts
        .union(&mut ctx, &[callable, Atom::Int.fact()])
        .unwrap();
    assert_eq!(facts.exported(&mut ctx, maybe).unwrap(), Atom::Int.fact());
    let array = facts.array(&mut ctx, callable).unwrap();
    let empty = facts.tuple(&mut ctx, &[]).unwrap();
    assert_eq!(facts.exported(&mut ctx, array).unwrap(), empty);
    let plain = facts
        .shape(&mut ctx, &[(b"method", callable, false)], false)
        .unwrap();
    let object = facts.hash_as(&mut ctx, plain, HashKind::Object).unwrap();
    assert_eq!(facts.exported(&mut ctx, plain).unwrap(), Atom::Never.fact());
    assert_eq!(facts.exported(&mut ctx, object).unwrap(), object);
    let mut nested = callable;
    for _ in 0..128 {
        nested = facts.tuple(&mut ctx, &[nested, nested]).unwrap();
    }
    assert_eq!(
        facts.exported(&mut ctx, nested).unwrap(),
        Atom::Never.fact()
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn verify_script(script: &crate::Script, roots: &[(&str, Value)], expected: &str, rejected: bool) {
    verify_script_args(script, roots, &[], expected, rejected);
}

fn verify_script_args(
    script: &crate::Script,
    roots: &[(&str, Value)],
    args: &[Value],
    expected: &str,
    rejected: bool,
) {
    let program = &script.inner.code.program;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut hosts = Buffer::empty();
    let mut globals = Buffer::empty();
    for (name, value) in roots {
        let value = admitted(&mut ctx, &mut facts, program, &mut hosts, value).unwrap();
        globals
            .push(&mut ctx, (Value::bytes(*name), Target::Value(value)))
            .unwrap();
    }
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let contract = facts.annotation(&mut ctx, ty, |_, _| Ok(None)).unwrap();
        contracts.push(&mut ctx, contract).unwrap();
    }
    let function = program.names["run"];
    let inputs = arguments::general_inputs(
        &mut ctx,
        &mut facts,
        &program.functions[function].params,
        &contracts.data,
    )
    .unwrap();
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            program,
            source_owner: 0,
            contracts: &contracts.data,
            hosts: &hosts.data,
            globals: &globals.data,
        },
        function,
        &inputs.data,
    )
    .unwrap();
    let ops: Vec<_> = report
        .incomplete
        .data
        .iter()
        .map(
            |&super::calls::Location {
                 function: f, pc, ..
             }| (f, pc, program.functions[f].code[pc]),
        )
        .collect();
    assert!(ops.is_empty(), "{report:?}; unsupported: {ops:?}");
    assert_eq!(!report.issues.data.is_empty(), rejected, "{report:?}");
    let actual = script
        .call(
            "run",
            args,
            CallOptions {
                globals: roots
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), value.clone()))
                    .collect(),
                ..CallOptions::default()
            },
        )
        .unwrap()
        .value;
    assert_eq!(actual.to_string(), expected);
    let concrete = observed(&mut ctx, &mut facts, program, &actual);
    assert_ne!(
        facts.relation(&mut ctx, concrete, report.returns).unwrap(),
        Relation::Rejected,
        "{report:?}; inferred: {:?}; observed: {:?}",
        facts.node(report.returns),
        facts.node(concrete)
    );
    drop((report, globals, hosts, contracts, inputs, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn invalid_global_data_fails_before_mutators_and_can_be_overwritten_without_reading() {
    for (body, expected, rejected) in [
        ("begin; bad.clear; rescue; 9; end", "9", true),
        (
            "seen=[]; begin; bad.push(seen.push(1)); rescue; seen.push(9); end; seen",
            "[9]",
            true,
        ),
        (
            "seen=[]; begin; bad.each { seen.push(1) }; rescue; seen.push(9); end; seen",
            "[9]",
            true,
        ),
        ("bad=7; bad", "7", false),
        ("7", "7", false),
    ] {
        let counter = Arc::new(AtomicUsize::new(0));
        let bad = Value::array(vec![method(&counter).value()]);
        let script = Engine::new()
            .compile(&format!("def run; {body}; end"))
            .unwrap();
        verify_script(&script, &[("bad", bad)], expected, rejected);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn root_host_methods_allow_calls_but_keep_ordinary_reads_attached() {
    for (body, expected, rejected, count) in [
        ("deliver(7)", "7", false, 1),
        ("begin; deliver; rescue; 9; end", "9", true, 0),
        (
            "result=deliver((begin; deliver=9; 7; end)); [result,deliver]",
            "[7, 9]",
            false,
            1,
        ),
    ] {
        let counter = Arc::new(AtomicUsize::new(0));
        let script = Engine::new()
            .compile(&format!("def run; {body}; end"))
            .unwrap();
        verify_script(
            &script,
            &[("deliver", method(&counter).value())],
            expected,
            rejected,
        );
        assert_eq!(counter.load(Ordering::Relaxed), count);
    }
}

#[test]
fn attached_source_methods_support_calls_and_only_nullary_automatic_reads() {
    for (body, expected, rejected) in [
        ("m.echo(7)", "7", false),
        ("m::echo(7)", "7", false),
        ("m[:echo](7)", "7", false),
        ("m.send(:echo,7)", "7", false),
        ("m.answer", "7", false),
        ("m.answer()", "7", false),
        ("m.items.push(7)", "[1, 7]", false),
        ("begin; m.echo; rescue; 9; end", "9", true),
        ("begin; m::answer; rescue; 9; end", "9", true),
        ("begin; m[:answer]; rescue; 9; end", "9", true),
        ("begin; m.fallback; rescue; 9; end", "9", true),
        ("m.fallback()", "7", false),
        ("begin; m.values; rescue; 9; end", "9", true),
    ] {
        let source = format!(
            "def echo(value); value; end; def answer; 7; end; def items; [1]; end; def fallback(value=7); value; end; def run; {body}; end"
        );
        let script = Engine::new().compile(&source).unwrap();
        let mut admission = CallContext::new(CallOptions::default());
        let environment = crate::objects::environment(&mut admission).unwrap();
        let methods = ["echo", "answer", "items", "fallback"]
            .into_iter()
            .map(|name| {
                let function = crate::exports::Function::new(
                    &mut admission,
                    script.inner.code.clone(),
                    environment.clone(),
                    script.inner.code.program.names[name],
                )
                .unwrap();
                (name.as_bytes().to_vec(), Value(Kind::Function(function)))
            })
            .collect();
        verify_script(
            &script,
            &[("m", Value::object(methods))],
            expected,
            rejected,
        );
    }
}

#[test]
fn attached_method_receivers_reject_mutation_after_evaluating_arguments() {
    witness(
        "seen=[]; begin; sms.deliver.push(seen.push(1)); rescue; seen; end",
        "[1]",
        true,
    );
    witness(
        "seen=[]; begin; sms[:deliver].push(seen.push(1)); rescue; seen; end",
        "[1]",
        true,
    );
    witness(
        "seen=[]; begin; sms.dup().deliver.push(seen.push(1)); rescue; seen; end",
        "[]",
        true,
    );
}

#[test]
fn conditional_method_fields_keep_valid_calls_and_detachment_errors() {
    for flag in [false, true] {
        let counter = Arc::new(AtomicUsize::new(0));
        let object = Value::object(vec![(b"deliver".to_vec(), method(&counter).value())]);
        let script = Engine::new().compile("def run(flag:bool); sms[:deliver]=9 if flag; begin; sms[:deliver](7); rescue; 9; end; end").unwrap();
        verify_script_args(
            &script,
            &[("sms", object.clone())],
            &[Value::boolean(flag)],
            if flag { "9" } else { "7" },
            true,
        );
        let script = Engine::new().compile("def run(flag:bool); sms[:deliver]=9 if flag; begin; sms[:deliver]; rescue; 7; end; end").unwrap();
        verify_script_args(
            &script,
            &[("sms", object)],
            &[Value::boolean(flag)],
            if flag { "9" } else { "7" },
            true,
        );
    }
}

#[test]
fn source_method_reads_preserve_root_writes_and_block_control_transfers() {
    for (body, expected) in [
        ("[m.answer,count]", "[2, 2]"),
        (
            "result=m.apply {|x| count+=x; break 9}; [result,count]",
            "[9, 8]",
        ),
        (
            "begin; m.apply {|x| count+=x; return count}; ensure; count+=1; end",
            "8",
        ),
    ] {
        let script = Engine::new()
            .compile(&format!(
                "def answer; count+=1; count; end; def apply; yield 7; end; def run; {body}; end"
            ))
            .unwrap();
        let mut admission = CallContext::new(CallOptions::default());
        let environment = crate::objects::environment(&mut admission).unwrap();
        let methods = ["answer", "apply"]
            .into_iter()
            .map(|name| {
                let function = crate::exports::Function::new(
                    &mut admission,
                    script.inner.code.clone(),
                    environment.clone(),
                    script.inner.code.program.names[name],
                )
                .unwrap();
                (name.as_bytes().to_vec(), Value(Kind::Function(function)))
            })
            .collect();
        verify_script(
            &script,
            &[("m", Value::object(methods)), ("count", Value::int(1))],
            expected,
            false,
        );
    }
}

#[test]
fn method_owners_cannot_resolve_to_an_unrelated_world_with_the_same_index() {
    let program = bytecode::compile(
        "def helper; 7; end; def run; item[:method](7); end",
        Vec::new(),
        &(),
    )
    .unwrap();
    for callable in [
        Callable::Host(0),
        Callable::Function(program.names["helper"]),
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let method = facts.callable(&mut ctx, 99, callable).unwrap();
        let plain = facts
            .shape(&mut ctx, &[(b"method", method, false)], false)
            .unwrap();
        let object = facts.hash_as(&mut ctx, plain, HashKind::Object).unwrap();
        let host = Host::new(&mut ctx, &mut facts, None).unwrap();
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                program: &program,
                source_owner: 0,
                contracts: &[],
                hosts: &[host],
                globals: &[(Value::bytes(b"item"), Target::Value(object))],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(!report.incomplete.data.is_empty(), "{report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn accounting_program() -> bytecode::Program {
    bytecode::compile("def run; begin; sms.values; rescue; sms[:deliver]=7; end; begin; sms.each { 1 }; rescue; sms.clear; end; sms.values; end", Vec::new(), &()).unwrap()
}

fn work(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let method = facts.callable(ctx, 0, Callable::Host(0))?;
    let shape = facts.shape(ctx, &[(b"deliver", method, false)], false)?;
    let object = facts.hash_as(ctx, shape, HashKind::Object)?;
    let host = Host::new(ctx, &mut facts, None)?;
    let name = ctx.bytes(b"sms")?;
    let report = calls::analyze(
        ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            program,
            source_owner: 0,
            contracts: &[],
            hosts: &[host],
            globals: &[(name, Target::Value(object))],
        },
        program.names["run"],
        &[],
    )?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(!report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn attached_method_analysis_accounts_for_projection_and_releases_interrupted_work() {
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
fn attached_method_analysis_keeps_cancellation_and_deadlines_latched() {
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
fn supplied_arguments_reject_nested_methods_before_an_unused_parameter_body_runs() {
    let script = Engine::new().compile("def run(value); 7; end").unwrap();
    let program = &script.inner.code.program;
    let counter = Arc::new(AtomicUsize::new(0));
    for value in [
        method(&counter).value(),
        Value::array(vec![method(&counter).value()]),
        Value::hash(vec![(b"method".to_vec(), method(&counter).value())]),
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut hosts = Buffer::empty();
        let value_fact = admitted(&mut ctx, &mut facts, program, &mut hosts, &value).unwrap();
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                program,
                source_owner: 0,
                contracts: &[],
                hosts: &hosts.data,
                globals: &[],
            },
            program.names["run"],
            &[arguments::Input::Supplied(value_fact)],
        )
        .unwrap();
        assert_eq!(report.returns, Atom::Never.fact());
        assert_eq!(report.contexts, 0);
        assert!(report.incomplete.data.is_empty());
        assert!(!report.issues.data.is_empty());
        assert_eq!(
            script
                .call("run", std::slice::from_ref(&value), CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type
        );
        assert_eq!(counter.load(Ordering::Relaxed), 0);
        drop((report, hosts, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn optional_and_uncertain_hash_projections_keep_the_object_alternatives() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let method = facts.callable(&mut ctx, 0, Callable::Host(0)).unwrap();
    let optional = facts
        .shape(&mut ctx, &[(b"method", method, true)], false)
        .unwrap();
    let empty = facts.shape(&mut ctx, &[], false).unwrap();
    assert_eq!(facts.exported(&mut ctx, optional).unwrap(), empty);
    let required = facts
        .shape(&mut ctx, &[(b"method", method, false)], false)
        .unwrap();
    let uncertain = facts.hash_as(&mut ctx, required, HashKind::Any).unwrap();
    let object = facts.hash_as(&mut ctx, required, HashKind::Object).unwrap();
    assert_eq!(facts.exported(&mut ctx, uncertain).unwrap(), object);
    let values = facts.union(&mut ctx, &[method, Atom::Int.fact()]).unwrap();
    let tuple = facts.tuple(&mut ctx, &[values, values]).unwrap();
    let expected = facts
        .tuple(&mut ctx, &[Atom::Int.fact(), Atom::Int.fact()])
        .unwrap();
    assert_eq!(facts.exported(&mut ctx, tuple).unwrap(), expected);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}
