use super::{
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine, ErrorClass, ErrorKind, Limits, Result, Value};

fn witness(source: &str, args: &[Value], expected: &str) {
    witness_checked(source, args, expected, false);
}

fn witness_checked(source: &str, args: &[Value], expected: &str, rejected: bool) {
    let actual = Engine::legacy_unchecked()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert_eq!(actual.value.to_string(), expected, "{source}");
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    let concrete = literal_fact(&mut ctx, &mut facts, &actual.value);
    assert_ne!(
        facts.relation(&mut ctx, concrete, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn normal_attempts_skip_rescues_and_keep_else_and_ensure_results_separate() {
    for (source, expected) in [
        (
            "def run(flag=nil) -> int; begin; !flag; rescue; missing; end; 7; end",
            "7",
        ),
        ("def run -> int; begin; 7; rescue; missing; end; end", "7"),
        (
            "def run -> int; begin; 7; rescue; missing; else; 8; ensure; 9; end; end",
            "8",
        ),
        (
            "def run; x=1; result=begin; x=2; x; ensure; x=3; end; [result,x]; end",
            "[2, 3]",
        ),
        ("def run; begin; 7; ensure; x=9; end; x; end", "9"),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn explicit_errors_select_ordered_rescues_and_preserve_scope() {
    for (source, expected) in [
        (
            "def run -> int; begin; raise \"bad\"; rescue; 7; else; missing; end; end",
            "7",
        ),
        (
            "def run -> int; begin; raise TypeError, \"bad\"; rescue ArgumentError; missing; rescue TypeError; 7; rescue; missing; end; end",
            "7",
        ),
        (
            "def run -> int; begin; raise LimitError, \"bad\"; rescue StandardError; missing; rescue RuntimeError; 7; end; end",
            "7",
        ),
        (
            "def run; x=1; begin; x=2; raise \"bad\"; rescue => error; [x,error.type]; end; end",
            "[2, RuntimeError]",
        ),
        (
            "def run; e=9; begin; raise \"bad\"; rescue => e; 7; end; e; end",
            "9",
        ),
        (
            "def run; begin; raise \"bad\"; rescue TypeError; x=2; rescue; x; end; end",
            "nil",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn ensure_preserves_and_replaces_pending_returns_and_loop_transfers() {
    for (source, expected) in [
        (
            "def run -> int; begin; return 7; ensure; 9; end; missing; end",
            "7",
        ),
        (
            "def run -> int; begin; return \"bad\"; ensure; return 9; end; end",
            "9",
        ),
        (
            "def run -> int; begin; raise \"bad\"; ensure; return 9; end; end",
            "9",
        ),
        (
            "def run; x=0; for n in [1,2]; begin; break 7; ensure; x=9; end; end; x; end",
            "9",
        ),
        (
            "def run; x=[]; for n in [1,2]; begin; next; ensure; x.push(n); end; end; x; end",
            "[1, 2]",
        ),
        (
            "def run -> int; begin; begin; return 7; ensure; return 8; end; ensure; return 9; end; end",
            "9",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn retries_keep_mutations_and_run_only_exited_ensures() {
    for (source, expected) in [
        (
            "def run; again=true; clean=0; result=begin; if again; again=false; raise \"again\"; end; 7; rescue; retry; ensure; clean+=1; end; [result,clean]; end",
            "[7, 1]",
        ),
        (
            "def run; again=true; trace=[]; begin; if again; again=false; raise \"again\"; end; rescue; begin; retry; ensure; trace.push(1); end; ensure; trace.push(2); end; trace; end",
            "[1, 2]",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn errors_and_ambient_reraises_propagate_through_script_call_summaries() {
    for (source, expected) in [
        (
            "def fail; raise TypeError, \"bad\"; end; def run -> int; begin; fail; missing; rescue TypeError; 7; end; end",
            "7",
        ),
        (
            "def fail; begin; raise \"bad\"; ensure; return 7; end; end; def run -> int; begin; fail; rescue; missing; end; end",
            "7",
        ),
        (
            "def reraised; raise; end; def run -> int; begin; begin; raise TypeError, \"bad\"; rescue; reraised; end; rescue TypeError; 7; end; end",
            "7",
        ),
        (
            "def reraised; begin; raise; rescue TypeError; 7; rescue; 9; end; end; def run; a=reraised; b=begin; raise TypeError, \"bad\"; rescue; reraised; end; [a,b]; end",
            "[9, 7]",
        ),
        (
            "def run; x=true; result=begin; x; ensure; x=false; end; [result,x]; end",
            "[true, false]",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn previously_incomplete_exception_paths_now_match_runtime_values() {
    for (source, expected) in [
        ("def run; begin; 1; rescue; 2; end; end", "1"),
        ("def run; begin; 1; rescue; 2; ensure; 3; end; end", "1"),
        (
            "def run; for x in [7]; begin; x; ensure; 1; end; end; end",
            "7",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn operation_failures_enter_rescue_before_unreachable_tails_and_pending_effects() {
    for (source, expected) in [
        (
            "def run -> int; begin; 7/0; missing; rescue ZeroDivisionError; 9; end; end",
            "9",
        ),
        (
            "def fail; 7/0; end; def run -> int; begin; fail; missing; rescue ZeroDivisionError; 9; end; end",
            "9",
        ),
        ("def run; x=1; begin; x=2; x=7/0; rescue; x; end; end", "2"),
        (
            "def run; a=[1]; begin; a.push(7/0); rescue; a; end; end",
            "[1]",
        ),
        (
            "def run; a=[1]; begin; a[0]=7/0; rescue; a; end; end",
            "[1]",
        ),
        (
            "def run -> int; begin; begin; raise TypeError, \"bad\"; rescue TypeError; rescue; missing; ensure; 1; end; rescue TypeError; 9; end; end",
            "9",
        ),
        (
            "def run -> int; begin; begin; 1; rescue; missing; else; raise \"bad\"; end; rescue; 7; end; end",
            "7",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn rejected_operations_preserve_receivers_and_keep_static_contradictions_visible() {
    for (source, expected) in [
        ("def run; a=[1]; begin; a[-9]=2; rescue; a; end; end", "[1]"),
        (
            "def run; a=[1]; begin; a.pop(\"bad\"); rescue; a; end; end",
            "[1]",
        ),
        (
            "def f(x: int); x; end; def run; begin; f(\"bad\"); rescue; 7; end; end",
            "7",
        ),
        (
            "def f -> int; \"bad\"; end; def run; begin; f; rescue; 7; end; end",
            "7",
        ),
        ("def run; begin; missing; rescue; 7; end; end", "7"),
        (
            "def run; begin; raise 7; rescue TypeError; 9; end; end",
            "9",
        ),
        (
            "def f(x); x; end; def run; begin; f(*7); rescue; 9; end; end",
            "9",
        ),
    ] {
        witness_checked(source, &[], expected, true);
    }
}

#[test]
fn retry_cannot_cross_calls_and_callee_cleanup_precedes_the_error() {
    for (source, expected) in [
        (
            "def helper; begin; retry; rescue; return 99; ensure; return 7; end; end; def run; begin; raise TypeError, \"bad\"; rescue; helper; end; end",
            "7",
        ),
        (
            "def helper; begin; retry; rescue; return 99; ensure; 1; end; end; def run; begin; begin; raise TypeError, \"bad\"; rescue; helper; end; rescue LocalJumpError; 7; end; end",
            "7",
        ),
        ("def run; begin; retry; rescue; 7; end; end", "7"),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn escaping_error_summaries_retain_runtime_classes_after_cleanup() {
    for (source, expected) in [
        ("def run; raise \"bad\"; end", ErrorClass::Runtime),
        ("def run; raise TypeError, \"bad\"; end", ErrorClass::Type),
        ("def run; 7/0; end", ErrorClass::ZeroDivision),
        (
            "def run; begin; raise TypeError, \"bad\"; ensure; 7; end; end",
            ErrorClass::Type,
        ),
        (
            "def run; begin; return 7; ensure; raise ArgumentError, \"bad\"; end; end",
            ErrorClass::Argument,
        ),
        (
            "def f; raise LimitError, \"bad\"; end; def run; begin; f; rescue StandardError; 9; end; end",
            ErrorClass::Limit,
        ),
    ] {
        let error = Engine::legacy_unchecked()
            .compile(source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.class(), Some(expected), "{source}: {error}");
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(report.issues.data.is_empty(), "{source}: {report:?}");
        assert_eq!(report.throws, 1 << expected as u8, "{source}: {report:?}");
        assert_eq!(
            report.returns,
            super::facts::Atom::Never.fact(),
            "{source}: {report:?}"
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn nested_error_and_control_combinations_include_runtime_values() {
    let mut count = 0;
    for body in [
        "7",
        "raise \"bad\"",
        "raise TypeError, \"bad\"",
        "raise LimitError, \"bad\"",
        "7/0",
        "return 7",
        "break 7",
        "next",
        "begin; raise ArgumentError, \"bad\"; rescue; raise; end",
    ] {
        for cleanup in [
            "9",
            "return 9",
            "raise ArgumentError, \"cleanup\"",
            "break 9",
            "next",
        ] {
            for filter in [
                "TypeError",
                "RuntimeError",
                "ZeroDivisionError",
                "StandardError",
                "ArgumentError",
            ] {
                let source = format!(
                    "def run; trace=[]; result=begin; for n in [1]; begin; trace.push(1); {body}; rescue {filter}; trace.push(2); 8; ensure; trace.push(3); {cleanup}; end; end; rescue RuntimeError => e; [e.type,trace]; end; [result,trace]; end"
                );
                let actual = Engine::legacy_unchecked()
                    .compile(&source)
                    .unwrap()
                    .call("run", &[], CallOptions::default())
                    .unwrap();
                witness(&source, &[], &actual.value.to_string());
                count += 1;
            }
        }
    }
    assert_eq!(count, 225);
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def fail(flag: bool); if flag; raise TypeError, \"bad\"; else; 7; end; end; def run(flag: bool) -> int; a=[]; result=begin; a.push(fail(flag)); a[0]; rescue TypeError; begin; 9; ensure; a.push(2); end; ensure; a.push(3); end; result; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn exception_work_and_pending_state_obey_exact_quotas_and_release_failures() {
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
fn cancellation_and_deadlines_never_become_rescuable_analysis_errors() {
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
fn host_failure_paths_are_analyzed_without_running_callbacks_or_validators() {
    use super::calls::{self, Host, World};
    let method = crate::HostMethod::new("host", |_, _, _| panic!("checker invoked host"))
        .with_contract(
            |_, _, _| panic!("checker invoked argument validator"),
            |_, _| panic!("checker invoked result validator"),
        )
        .with_signature(crate::Signature {
            params: vec![],
            result: "int".into(),
            accepts_block: false,
        })
        .unwrap();
    let mut engine = Engine::legacy_unchecked();
    engine.register_method("host", method.clone());
    let script = engine.compile("def run -> int; begin; host(); rescue RuntimeError => e; if e.type == \"TypeError\"; 7; else; 9; end; ensure; 1; end; end").unwrap();
    let program = &script.inner.code.program;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut contracts = crate::budget::Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(&mut ctx, ty, |_, _| Ok(None)).unwrap();
        contracts.push(&mut ctx, fact).unwrap();
    }
    let method = method.value();
    let crate::value::Kind::Host(method) = &method.0 else {
        unreachable!()
    };
    let host = Host::new(&mut ctx, &mut facts, method.compiled_signature()).unwrap();
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program,
            contracts: &contracts.data,
            hosts: &[host],
            globals: &[],
        },
        program.names["run"],
        &[],
    )
    .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert_eq!(report.throws, 0);
    drop((report, contracts, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn recursive_error_contexts_keep_absence_and_rescued_classes() {
    let source = "def f(flag: bool); if flag; begin; raise TypeError, \"bad\"; rescue; f(false); end; else; raise; end; end; def run(flag: bool) -> int; begin; f(flag); rescue TypeError; 7; rescue RuntimeError; 9; end; end";
    witness(source, &[Value::boolean(true)], "7");
    witness(source, &[Value::boolean(false)], "9");
}

#[test]
fn ensure_writes_invalidate_saved_predicates_without_changing_saved_values() {
    let source = "def run(flag: bool) -> int; if (begin; flag; ensure; flag=false; end); if flag; missing; else; 7; end; else; 9; end; end";
    witness(source, &[Value::boolean(true)], "7");
    witness(source, &[Value::boolean(false)], "9");
}

#[test]
fn exception_reference_decisions_keep_caught_error_and_cleanup_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-exceptions.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 43);
    let mut differences = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        witness_checked(
            source,
            &[],
            case["runtime"]["display"].as_str().unwrap(),
            case["rust_rejected"].as_bool().unwrap(),
        );
        if case["go_rejected"] != case["rust_rejected"] {
            assert!(!case["difference"].as_str().unwrap().is_empty());
            differences += 1;
        }
    }
    assert_eq!(differences, 5);
}

#[test]
fn out_of_range_float_selectors_keep_recoverable_failure_paths() {
    for source in [
        "def run; begin; [1][1.0e30]; rescue; 7; end; end",
        "def run; begin; \"a\"[-1.0e30]; rescue; 7; end; end",
        "def run; begin; [1].at(1.0e30); rescue; 7; end; end",
        "def run; a=[1]; x=0; begin; a[1.0e30]=2; rescue; x=7; end; x; end",
    ] {
        witness(source, &[], "7");
    }
}
