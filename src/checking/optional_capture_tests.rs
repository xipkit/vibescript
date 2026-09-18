use super::{
    facts::Facts,
    normalization_tests::{analyze, witness},
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value, bytecode};

fn both(body: &str, expected: [&str; 2], warnings: bool) {
    for (flag, expected) in [false, true].into_iter().zip(expected) {
        witness(body, &[Value::boolean(flag)], expected, warnings);
    }
}

#[test]
fn optional_captures_write_existing_parents_and_keep_new_bindings_local() {
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=once {x=7}; x ||= nil; [value,x]; end",
        ["[7, nil]", "[7, 7]"],
        false,
    );
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=nil; end; end; [0].length; value=once {x=7}; x ||= nil; [value,x]; end",
        ["[7, nil]", "[7, 7]"],
        false,
    );
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=[1]; end; end; [0].length; value=once {x=[7]; x.push(9); x}; x ||= nil; [value,x]; end",
        ["[[7, 9], nil]", "[[7, 9], [7, 9]]"],
        false,
    );
}

#[test]
fn statement_branch_initialization_is_distinct_from_an_absent_capture() {
    both(
        "def once; yield; end; def run(flag:bool); if flag; x=1; end; [0].length; value=once {x=7}; [value,x]; end",
        ["[7, 7]", "[7, 7]"],
        false,
    );
    both(
        "def fallback; 4; end; def once; yield; end; def run(flag:bool); if flag; fallback=7; end; [0].length; once {fallback}; end",
        ["nil", "7"],
        false,
    );
}

#[test]
fn optional_capture_presence_survives_stable_recursion_and_deep_relays() {
    both(
        "def once; yield; end; def walk(n:int); if n>0; walk(n-1) {|v| v}; else; once {yield 7}; end; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=walk(3) {|v| x=7}; x ||= nil; [value,x]; end",
        ["[7, nil]", "[7, 1]"],
        false,
    );
    let nested = format!("{}x=7{}", "once {".repeat(32), "}".repeat(32));
    both(
        &format!(
            "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value={nested}; x ||= nil; [value,x]; end"
        ),
        ["[7, nil]", "[7, 7]"],
        false,
    );
}

#[test]
fn optional_capture_updates_preserve_pending_addresses_and_value_copies() {
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=[1]; end; end; [0].length; value=once {x ||= [0]; x[0] += once {x.push(4); 2}; x}; x ||= nil; [value,x]; end",
        ["[[2, 4], nil]", "[[3, 4], [3, 4]]"],
        false,
    );
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=[1]; end; end; [0].length; value=once {x ||= [0]; copy=x; x.push(4); copy}; x ||= nil; [value,x]; end",
        ["[[0], nil]", "[[1], [1, 4]]"],
        false,
    );
}

#[test]
fn optional_host_method_reads_fail_without_running_the_callback() {
    use crate::{Engine, HostMethod};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let mut engine = Engine::new();
    engine.register_method(
        "host",
        HostMethod::new("host", move |_, _, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Value::int(3))
        }),
    );
    let script=engine.compile("def once; yield; end; def run(flag:bool); ignored=if flag; begin; host=7; end; end; [0].length; begin; once {host}; rescue; 99; end; end").unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, &script.inner.code.program).unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(!report.issues.data.is_empty());
    for (flag, expected) in [(false, 99), (true, 7)] {
        let result = script
            .call("run", &[Value::boolean(flag)], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(expected));
        let value = facts.integer(&mut ctx, expected).unwrap();
        assert_ne!(
            facts.relation(&mut ctx, value, report.returns).unwrap(),
            super::relation::Relation::Rejected
        );
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn optional_captures_survive_nested_and_forwarded_callbacks() {
    for call in [
        "once {once {x=7}}",
        "forward {x=7}",
        "once {forward {once {x=7}}}",
    ] {
        both(
            &format!(
                "def once; yield; end; def forward; once {{yield}}; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value={call}; x ||= nil; [value,x]; end"
            ),
            ["[7, nil]", "[7, 7]"],
            false,
        );
    }
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=once {x=7; once {x=9}; x}; x ||= nil; [value,x]; end",
        ["[9, nil]", "[9, 9]"],
        false,
    );
    both(
        "def once; yield(3); end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=once {|x| once {x=7}; x}; x ||= nil; [value,x]; end",
        ["[7, nil]", "[7, 1]"],
        false,
    );
}

#[test]
fn repeated_callbacks_do_not_publish_bindings_created_inside_an_invocation() {
    both(
        "def twice; [yield,yield]; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=twice {x ||= 0; x+=1; x}; x ||= nil; [value,x]; end",
        ["[[1, 1], nil]", "[[2, 3], 3]"],
        false,
    );
    both(
        "def once; yield; end; def twice; [yield,yield]; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=twice {once {x ||= 0; x+=1; x}}; x ||= nil; [value,x]; end",
        ["[[1, 1], nil]", "[[2, 3], 3]"],
        false,
    );
}

#[test]
fn optional_capture_effects_survive_break_return_errors_and_cleanup() {
    for control in ["break 8", "next 8", "return [8,x]", "raise \"bad\""] {
        let expected = if control.starts_with("raise") {
            ["[9, nil]", "[9, 7]"]
        } else if control.starts_with("return") {
            ["[8, 7]", "[8, 7]"]
        } else {
            ["[8, nil]", "[8, 7]"]
        };
        both(
            &format!(
                "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; result=begin; once {{x=7; {control}}}; rescue; 9; end; x ||= nil; [result,x]; end"
            ),
            expected,
            false,
        );
    }
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=1; end; end; [0].length; value=once {begin; break 8; ensure; x=7; end}; x ||= nil; [value,x]; end",
        ["[8, nil]", "[8, 7]"],
        false,
    );
}

#[test]
fn optional_value_reads_follow_declarations_functions_and_missing_name_errors() {
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; Status=Review; end; end; [0].length; once {Status.name}; end",
        ["Status", "Review"],
        false,
    );
    both(
        "def fallback; 4; end; def once; yield; end; def run(flag:bool); ignored=if flag; begin; fallback=7; end; end; [0].length; once {fallback}; end",
        ["4", "7"],
        false,
    );
    both(
        "def once; yield; end; def run(flag:bool); ignored=if flag; begin; x=7; end; end; [0].length; begin; once {x}; rescue; 99; end; end",
        ["99", "7"],
        true,
    );
    both(
        "def once; yield; end; def run(flag:bool); result=once {if flag; absent=7; end; absent}; result; end",
        ["nil", "7"],
        false,
    );
}

#[test]
fn optional_global_replacements_flow_into_captures() {
    for (body, expected) in [
        (
            "ignored=if flag; begin; Hash=[7,8]; end; end; [0].length; once {Hash.length}",
            ["1", "2"],
        ),
        (
            "ignored=if flag; begin; now=7; end; end; [0].length; once {now.is_type?(:int)}",
            ["false", "true"],
        ),
        (
            "ignored=if flag; begin; format=7; end; end; [0].length; begin; once {format}; rescue; 99; end",
            ["99", "7"],
        ),
    ] {
        let source = format!("def once; yield; end; def run(flag:bool); {body}; end");
        let script = crate::Engine::new().compile(&source).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &script.inner.code.program).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        for (flag, expected) in [false, true].into_iter().zip(expected) {
            assert_eq!(
                script
                    .call("run", &[Value::boolean(flag)], CallOptions::default())
                    .unwrap()
                    .value
                    .to_string(),
                expected,
                "{source}"
            );
        }
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn bare_parameterized_functions_are_not_detached_or_implicitly_called() {
    for signature in ["x", "x=7", "*xs", "x:"] {
        witness(
            &format!("def f({signature}); 3; end; def run; begin; f; rescue; 9; end; end"),
            &[],
            "9",
            true,
        );
        both(
            &format!(
                "def f({signature}); 3; end; def once; yield; end; def run(flag:bool); ignored=if flag; begin; f=7; end; end; [0].length; begin; once {{f}}; rescue; 9; end; end"
            ),
            ["9", "7"],
            true,
        );
    }
}

#[test]
fn optional_type_aliases_resolve_when_present_and_fallback_identities_agree() {
    for body in [
        "ignored=if flag; begin; Status=Status; end; end; [0].length; [:draft].map {|x:Status| x.enum.name}",
        "ignored=if flag; begin; Status=7; end; end; [0].length; [:draft].map {|x:Status| x.enum.name}",
        "Alias=Status; once {ignored=if flag; begin; Alias=Status; end; end; [0].length; [:draft].map {|x:Alias| x.enum.name}}",
        "ignored=if flag; begin; Status=Status; end; end; [0].length; once {once {[:draft].map {|x:Status| x.enum.name}}}",
        "ignored=if flag; begin; STATUS=Status; end; end; [0].length; [:draft].map {|x:status| x.enum.name}",
    ] {
        both(
            &format!("def once; yield; end; def run(flag:bool); {body}; end"),
            ["[Status]", "[Status]"],
            false,
        );
    }
}

#[test]
fn annotations_use_the_actual_owner_when_an_absent_capture_becomes_local() {
    for (flag, expected, warnings) in [(false, "[Review]", false), (true, "99", true)] {
        for depth in 0..4 {
            let body = format!(
                "{}[:draft].map {{|x:alias| x.enum.name}}{}",
                "once {".repeat(depth),
                "}".repeat(depth)
            );
            witness(
                &format!(
                    "def once; yield; end; def run; ALIAS=Status; flag={flag}; ignored=if flag; begin; Alias=Review; end; end; [0].length; once {{Alias=Review; begin; {body}; rescue; 99; end}}; end"
                ),
                &[],
                expected,
                warnings,
            );
        }
        witness(
            &format!(
                "def once; yield; end; def run; ALIAS=Status; flag={flag}; ignored=if flag; begin; Alias=Review; end; end; [0].length; once {{Alias=Review; [:draft].map {{|x:ALIAS| x.enum.name}}}}; end"
            ),
            &[],
            "[Status]",
            false,
        );
        witness(
            &format!(
                "def once; yield; end; def run; ALIAS=Status; flag={flag}; ignored=if flag; begin; Alias=Review; end; end; [0].length; once {{|Alias| Alias=Review; [:draft].map {{|x:alias| x.enum.name}}}}; end"
            ),
            &[],
            "[Review]",
            false,
        );
    }
}

#[test]
fn optional_type_identities_with_distinct_or_missing_fallbacks_stay_explicit() {
    for body in [
        "ignored=if flag; begin; Status=Review; end; end; [0].length; [:draft].map {|x:Status| x}",
        "ignored=if flag; begin; Alias=Status; end; end; [0].length; [:draft].map {|x:Alias| x}",
        "ignored=if flag; begin; State=Status; end; end; [0].length; STATE=Review; [:draft].map {|x:state| x}",
        "ALIAS=Status; ignored=if flag; begin; Alias=Review; end; end; [0].length; [0].map {Alias=Review; [:draft].map {|x:alias| x}}",
    ] {
        let source = format!(
            "enum Status; Draft; end; enum Review; Draft; end; def run(flag:bool); {body}; end"
        );
        let program = bytecode::compile(&source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn work(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, program)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

fn accounting_program() -> bytecode::Program {
    bytecode::compile("enum Status; Draft; end; def once; yield; end; def twice; [yield,yield]; end; def run(flag:bool); ignored=if flag; begin; Status=Status; x=1; end; end; [0].length; result=twice {once {[:draft].map {|s:Status| x=7; s.symbol}}}; x ||= nil; [result,x]; end",Vec::new(),&()).unwrap()
}

#[test]
fn optional_capture_analysis_obeys_exact_and_interrupted_limits() {
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
    for sample in 0..32 {
        for memory in [false, true] {
            let limits = if memory {
                Limits {
                    memory_bytes: Some(stats.peak_memory_bytes * sample / 32),
                    ..Limits::default()
                }
            } else {
                Limits {
                    steps: Some(stats.steps * sample as u64 / 32),
                    ..Limits::default()
                }
            };
            let kind = if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
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
fn optional_capture_analysis_preserves_latched_cancellation_and_deadlines() {
    let program = accounting_program();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        drop(analyze(&mut ctx, &mut facts, &program).unwrap());
        let kind = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        assert_eq!(
            analyze(&mut ctx, &mut facts, &program).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
