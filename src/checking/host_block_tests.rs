use super::{
    arguments,
    attached_tests::admitted,
    calls::{self, Analysis, Host, Target, World},
    facts::Facts,
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, Error, ErrorClass, ErrorKind, HostMethod, Limits, Result,
    Script, Signature, SignatureParam, Value, budget::Buffer, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone)]
struct Schedule {
    rounds: usize,
    arguments: Vec<Value>,
    swallow: bool,
    result: Value,
    last: bool,
    argument_error: bool,
}

impl Schedule {
    fn new(rounds: usize) -> Self {
        Self {
            rounds,
            arguments: vec![Value::int(2), Value::int(3)],
            swallow: false,
            result: Value::int(7),
            last: false,
            argument_error: false,
        }
    }
}

struct Fixture {
    script: Script,
    method: Value,
    globals: Vec<(String, Value)>,
    counters: Arc<[AtomicUsize; 3]>,
}

impl Fixture {
    fn new(source: &str, signature: Option<Signature>, schedule: Schedule) -> Self {
        let counters = Arc::new(std::array::from_fn::<_, 3, _>(|_| AtomicUsize::new(0)));
        let calls = counters.clone();
        let validators = counters.clone();
        let result_validators = counters.clone();
        let argument_error = schedule.argument_error;
        let method = HostMethod::new_with_block("visit", move |call, _, _| {
            calls[0].fetch_add(1, Ordering::Relaxed);
            let mut result = schedule.result.clone();
            for _ in 0..schedule.rounds {
                calls[1].fetch_add(1, Ordering::Relaxed);
                match call.call_block(&schedule.arguments) {
                    Ok(value) if schedule.last => result = value,
                    Ok(_) => (),
                    Err(_) if schedule.swallow => (),
                    Err(error) => return Err(error),
                }
            }
            Ok(result)
        })
        .with_contract(
            move |_, _, _| {
                validators[2].fetch_add(1, Ordering::Relaxed);
                if argument_error {
                    Err(Error::new(ErrorKind::Runtime, "validator stopped")
                        .with_class(ErrorClass::Assertion))
                } else {
                    Ok(())
                }
            },
            move |_, _| {
                result_validators[2].fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        );
        let method = signature.map_or_else(
            || method.clone(),
            |signature| method.clone().with_signature(signature).unwrap(),
        );
        let mut engine = Engine::new();
        engine.register_method("visit", method.clone());
        let script = engine
            .compile(&format!(
                "enum Status; Draft; Sent; end; enum Review; Draft; end; {source}"
            ))
            .unwrap();
        let program = &script.inner.code.program;
        let status = program.declarations[program.declaration_names["Status"]].clone();
        Self {
            script,
            method: method.value(),
            globals: vec![
                ("alias".into(), status),
                (
                    "host".into(),
                    Value::object(vec![(b"run".to_vec(), method.value())]),
                ),
                ("counter".into(), Value::int(0)),
            ],
            counters,
        }
    }

    fn signed(source: &str, result: &str, schedule: Schedule) -> Self {
        Self::new(
            source,
            Some(Signature {
                params: vec![],
                result: result.into(),
                accepts_block: true,
            }),
            schedule,
        )
    }

    fn analyze(&self, ctx: &mut CallContext, facts: &mut Facts) -> Result<Analysis> {
        let program = &self.script.inner.code.program;
        let Kind::Host(method) = &self.method.0 else {
            panic!()
        };
        let mut hosts = Buffer::empty();
        let host = Host::new(ctx, facts, method.signature())?;
        hosts.push(ctx, host)?;
        let mut globals = Buffer::empty();
        for (name, value) in &self.globals {
            let value = admitted(ctx, facts, program, &mut hosts, value)?;
            let name = ctx.bytes(name.as_bytes())?;
            globals.push(ctx, (name, Target::Value(value)))?;
        }
        let mut contracts = Buffer::empty();
        for ty in &program.types {
            let contract = facts.annotation(ctx, ty, |_, _| Ok(None))?;
            contracts.push(ctx, contract)?;
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
                hosts: &hosts.data,
                globals: &globals.data,
            },
            function,
            &inputs.data,
        )
    }

    fn execute(&self) -> Result<crate::Outcome> {
        self.script.call(
            "run",
            &[],
            CallOptions {
                globals: self.globals.iter().cloned().collect(),
                ..CallOptions::default()
            },
        )
    }
}

fn witness(fixture: &Fixture, warnings: Option<bool>) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    if let Some(warnings) = warnings {
        assert_eq!(!report.issues.data.is_empty(), warnings, "{report:?}");
    }
    for counter in fixture.counters.iter() {
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "analysis executed a callback or validator"
        );
    }
    match fixture.execute() {
        Ok(output) => {
            let actual = observed(
                &mut ctx,
                &mut facts,
                &fixture.script.inner.code.program,
                &output.value,
            );
            assert_ne!(
                facts.relation(&mut ctx, actual, report.returns).unwrap(),
                Relation::Rejected,
                "{}: {report:?}",
                output.value
            );
        }
        Err(error) => assert_ne!(
            report.throws & (1 << error.class().unwrap() as u8),
            0,
            "{error}: {report:?}"
        ),
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn host_block_schedules_allow_zero_one_and_repeated_invocations() {
    for rounds in [0, 1, 3] {
        for source in [
            "def run; n=0; result=visit { n+=1 }; [result,n]; end",
            "def run; a=[]; visit { a.push(7) }; a; end",
            "def run; h={a:[]}; visit { h.a.push(7) }; h; end",
            "def run; a=[1]; visit { b=a; b.push(7) }; a; end",
            "def run; visit { counter+=1 }; counter; end",
            "def run; visit { |counter| counter=7 }; counter; end",
        ] {
            witness(
                &Fixture::signed(source, "int", Schedule::new(rounds)),
                Some(false),
            );
        }
    }
}

#[test]
fn host_block_arguments_cover_padding_autosplat_numbered_and_typed_parameters() {
    for arguments in [
        vec![],
        vec![Value::int(2)],
        vec![Value::int(2), Value::int(3), Value::int(4)],
        vec![Value::array(vec![Value::int(2), Value::int(3)])],
    ] {
        for source in [
            "def run; value=nil; visit { |a,b,c| value=[a,b,c] }; value; end",
            "def run; value=nil; visit { value=[_1,_2,_3,_9] }; value; end",
            "def run; value=nil; visit { value=it }; value; end",
            "def run; value=nil; visit { |n:int| value=n+1 }; value; end",
            "def run; value=nil; visit { |(a,b)| value=[a,b] }; value; end",
        ] {
            let mut schedule = Schedule::new(1);
            schedule.arguments = arguments.clone();
            witness(&Fixture::signed(source, "int", schedule), None);
        }
    }
}

#[test]
fn host_block_calls_support_attached_and_forwarded_dispatch() {
    for target in [
        "visit",
        "host.run",
        "host::run",
        "host[:run]",
        "host.send(:run)",
        "host.public_send(:run)",
    ] {
        let source = format!("def run; n=0; {target} {{ n+=1 }}; n; end");
        witness(
            &Fixture::signed(&source, "int", Schedule::new(3)),
            Some(false),
        );
    }
    for source in [
        "def relay; visit { yield }; end; def run; n=0; relay { n+=1 }; n; end",
        "def relay; visit { yield }; end; def run; relay { return 9 }; 7; end",
        "def relay; visit { yield }; end; def run; relay { break 9 }; end",
        "def run; visit { visit { return 9 } }; 7; end",
        "def run; [1,2].map { |n| visit { break n }; n }; end",
        "def run; visit { [1,2].each { return 9 } }; 7; end",
    ] {
        witness(
            &Fixture::signed(source, "int", Schedule::new(1)),
            Some(false),
        );
    }
}

#[test]
fn ordinary_host_block_errors_preserve_writes_when_swallowed_or_propagated() {
    for swallow in [false, true] {
        for source in [
            "def run; a=[]; begin; visit { a.push(7); raise \"bad\" }; rescue; a.push(9); end; a; end",
            "def run; a=[]; begin; visit { begin; raise \"bad\"; ensure; a.push(7); end }; rescue; a.push(9); end; a; end",
            "def run; n=0; visit { n+=1; raise \"bad\" if n==1 }; n; end",
            "def run; counter=0; begin; visit { counter+=1; raise ArgumentError, \"bad\" }; rescue ArgumentError; counter; end; end",
        ] {
            let mut schedule = Schedule::new(3);
            schedule.swallow = swallow;
            witness(&Fixture::signed(source, "int", schedule), Some(false));
        }
    }
}

#[test]
fn sticky_host_transfers_cannot_repeat_script_writes() {
    for transfer in ["break 7", "break", "return a"] {
        let source = format!("def run; a=[]; visit {{ a.push(1); {transfer} }}; a; end");
        let mut schedule = Schedule::new(3);
        schedule.swallow = true;
        let fixture = Fixture::signed(&source, "", schedule);
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = fixture.analyze(&mut ctx, &mut facts).unwrap();
        assert!(report.incomplete.data.is_empty(), "{report:?}");
        assert!(report.issues.data.is_empty(), "{report:?}");
        let one = facts.integer(&mut ctx, 1).unwrap();
        let impossible = facts.tuple(&mut ctx, &[one, one]).unwrap();
        assert_eq!(
            facts
                .relation(&mut ctx, impossible, report.returns)
                .unwrap(),
            Relation::Rejected,
            "{report:?}"
        );
        assert_eq!(fixture.execute().unwrap().value.to_string(), "[1]");
        assert_eq!(fixture.counters[1].load(Ordering::Relaxed), 3);
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn sticky_nonlocal_returns_skip_host_result_checks_and_later_script_effects() {
    for source in [
        "def run; visit { return 9 }; 7; end",
        "def run; n=0; begin; visit { n=9; return 1 }; n=7; ensure; return n; end; end",
        "def run; visit { begin; return 9; ensure; counter=7; end }; 7; end",
        "def relay; visit { yield }; end; def run; relay { return 9 }; 7; end",
    ] {
        let mut schedule = Schedule::new(3);
        schedule.swallow = true;
        schedule.result = Value::bytes(b"host result");
        witness(&Fixture::signed(source, "string", schedule), Some(false));
    }
}

#[test]
fn host_break_results_are_normalized_at_the_updated_return_boundary() {
    for (source, result, warnings) in [
        ("def run; visit { break 9 }; end", "int", false),
        (
            "def run; begin; visit { break }; rescue; 99; end; end",
            "int",
            true,
        ),
        (
            "def run; begin; visit { break \"bad\" }; rescue; 99; end; end",
            "int",
            true,
        ),
        (
            "def run; visit { alias=Review; break :draft }.enum.name; end",
            "alias",
            false,
        ),
        (
            "def run; begin; visit { alias=nil; break :draft }; rescue; 99; end; end",
            "alias",
            true,
        ),
        (
            "def run; Math[:State]=Status; visit { Math[:State]=Review; break :draft }.enum.name; end",
            "Math.State",
            false,
        ),
    ] {
        let mut schedule = Schedule::new(3);
        schedule.swallow = true;
        if result != "int" {
            schedule.result = Value::symbol("draft");
        }
        witness(&Fixture::signed(source, result, schedule), Some(warnings));
    }
}

#[test]
fn host_result_contracts_refresh_after_normal_block_completion() {
    for source in [
        "def run; visit { alias=Review }.enum.name; end",
        "def run; Math[:State]=Status; visit { Math[:State]=Review }.enum.name; end",
    ] {
        for rounds in [0, 1, 3] {
            let mut schedule = Schedule::new(rounds);
            schedule.result = Value::symbol("draft");
            let result = if source.contains("Math") {
                "Math.State"
            } else {
                "alias"
            };
            witness(&Fixture::signed(source, result, schedule), Some(false));
        }
    }
}

#[test]
fn host_block_cleanup_can_replace_a_transfer_before_the_host_observes_it() {
    for body in ["7", "next 7", "break 7", "return 7", "raise \"body\""] {
        for cleanup in ["nil", "next 9", "break 9", "return 9", "raise \"cleanup\""] {
            let source = format!(
                "def run; a=[]; begin; result=visit {{ a.push(1); begin; {body}; ensure; a.push(2); {cleanup}; end }}; [result,a]; rescue; a; end; end"
            );
            let mut schedule = Schedule::new(2);
            schedule.swallow = true;
            witness(&Fixture::signed(&source, "", schedule), Some(false));
        }
    }
}

#[test]
fn host_blocks_preserve_pending_collection_writes_and_original_negative_indices() {
    for source in [
        "def run; a=[1]; a[-1]+=visit { a.push(2); break 7 }; a; end",
        "def run; a=[[1]]; a[0].push(visit { a.push([2]); break 7 }); a; end",
        "def run; a=[1]; a[0]+=visit { a=[9]; break 7 }; a; end",
        "def run; a={x:[1]}; a.x.push(visit { a[:y]=7; break 9 }); a; end",
        "def run; counter=1; counter+=visit { counter=2; break 7 }; counter; end",
    ] {
        witness(
            &Fixture::signed(source, "int", Schedule::new(1)),
            Some(false),
        );
    }
}

#[test]
fn host_block_analysis_checks_guards_before_body_effects_and_keeps_validator_errors() {
    let signature = Signature {
        params: vec![SignatureParam {
            name: "x".into(),
            ty: "Missing".into(),
            optional: false,
        }],
        result: "Missing".into(),
        accepts_block: true,
    };
    let mut schedule = Schedule::new(1);
    schedule.argument_error = true;
    let fixture = Fixture::new(
        "def run; begin; visit(nil) { counter=99 }; rescue AssertionError; counter=7; end; counter; end",
        Some(signature),
        schedule,
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert_eq!(report.contexts, 1);
    assert_eq!(report.returns, facts.integer(&mut ctx, 7).unwrap());
    assert_eq!(fixture.counters[2].load(Ordering::Relaxed), 0);
    assert_eq!(fixture.execute().unwrap().value.to_string(), "7");
    assert_eq!(fixture.counters[0].load(Ordering::Relaxed), 0);
}

#[test]
fn partially_valid_host_arguments_keep_the_successful_block_effects() {
    let signature = Signature {
        params: vec![SignatureParam {
            name: "value".into(),
            ty: "int".into(),
            optional: false,
        }],
        result: "int".into(),
        accepts_block: true,
    };
    let fixture = Fixture::new(
        "def run(flag:bool); begin; visit(flag ? 7 : \"bad\") {counter=42}; rescue; nil; end; counter; end",
        Some(signature),
        Schedule::new(1),
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = fixture.analyze(&mut ctx, &mut facts).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(!report.issues.data.is_empty(), "{report:?}");
    for flag in [false, true] {
        let output = fixture
            .script
            .call(
                "run",
                &[Value::boolean(flag)],
                CallOptions {
                    globals: fixture.globals.iter().cloned().collect(),
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(output.value.as_int(), Some(if flag { 42 } else { 0 }));
        let actual = observed(
            &mut ctx,
            &mut facts,
            &fixture.script.inner.code.program,
            &output.value,
        );
        assert_eq!(
            facts.relation(&mut ctx, actual, report.returns).unwrap(),
            Relation::Accepted,
            "{flag}: {report:?}"
        );
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn untyped_host_blocks_keep_exact_break_values_and_call_isolation() {
    let mut schedule = Schedule::new(3);
    schedule.swallow = true;
    let fixture = Fixture::new(
        "def run; visit { counter+=1; break 9 }; counter; end",
        None,
        schedule,
    );
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for _ in 0..3 {
        let report = fixture.analyze(&mut ctx, &mut facts).unwrap();
        assert!(report.incomplete.data.is_empty(), "{report:?}");
        assert!(report.issues.data.is_empty(), "{report:?}");
        assert_eq!(fixture.execute().unwrap().value.to_string(), "1");
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn host_block_presence_and_current_errors_follow_the_lexical_caller() {
    for source in [
        "def run; visit { missing if block_given? }; end",
        "def relay; visit { if block_given?; yield; else; missing; end }; end; def run; relay {7}; end",
        "def run; a=[]; begin; raise ArgumentError, \"outer\"; rescue ArgumentError; visit { begin; raise; rescue ArgumentError; a.push(7); end }; end; a; end",
    ] {
        witness(
            &Fixture::signed(source, "int", Schedule::new(3)),
            Some(false),
        );
    }
    let mut schedule = Schedule::new(1);
    schedule.last = true;
    witness(
        &Fixture::signed("def run; visit { block_given? }; end", "bool", schedule),
        Some(false),
    );
}

#[test]
fn nested_host_blocks_and_growing_captures_converge_on_the_default_stack() {
    for source in [
        "def run; a=[]; visit { a=[a] }; a; end".to_owned(),
        format!(
            "def run; {} return 9 {}; 7; end",
            "visit { ".repeat(12),
            " }".repeat(12)
        ),
    ] {
        let fixture = Fixture::signed(&source, "int", Schedule::new(3));
        witness(&fixture, Some(false));
    }
}

fn accounting_fixture() -> Fixture {
    Fixture::signed(
        "def run; a=[]; visit { a.push(7); counter+=1; raise \"try again\" if counter==1 }; [a,counter]; end",
        "int",
        Schedule::new(2),
    )
}

fn work(ctx: &mut CallContext, fixture: &Fixture) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = fixture.analyze(ctx, &mut facts)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn host_block_fixed_points_obey_exact_and_sampled_work_and_memory_limits() {
    let fixture = accounting_fixture();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &fixture).unwrap();
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
        assert_eq!(work(&mut ctx, &fixture).err().map(|e| e.kind), kind);
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
            assert_eq!(work(&mut ctx, &fixture).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn host_block_analysis_preserves_latched_cancellation_and_deadlines() {
    let fixture = accounting_fixture();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        drop(fixture.analyze(&mut ctx, &mut facts).unwrap());
        let kind = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        assert_eq!(
            fixture.analyze(&mut ctx, &mut facts).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
