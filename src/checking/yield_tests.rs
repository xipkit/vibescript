use super::{
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts, Node},
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine};
use std::sync::Arc;

fn witness(source: &str, warnings: bool) {
    let actual = Engine::new()
        .compile(source)
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .call("run", &[], CallOptions::default());
    let mut ctx = CallContext::new(CallOptions::default());
    ctx.output_writer = Some(Arc::new(|_, _| panic!("analysis executed output")));
    ctx.random_source = Some(Arc::new(|_, _| panic!("analysis executed entropy")));
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap_or_else(|e| panic!("{source}: {e}"));
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        warnings,
        "{source}: {report:?}"
    );
    match actual {
        Ok(actual) => {
            let value = literal_fact(&mut ctx, &mut facts, &actual.value);
            assert_ne!(report.returns, Atom::Never.fact(), "{source}: {report:?}");
            assert_ne!(
                facts.relation(&mut ctx, value, report.returns).unwrap(),
                Relation::Rejected,
                "{source}: actual {value:?}, {report:?}"
            );
            let same = facts
                .scalar_binary(&mut ctx, "==", value, report.returns)
                .unwrap();
            assert!(
                !matches!(facts.node(same.value), Node::Boolean(false)),
                "{source}: result contradicts execution: {report:?}"
            );
        }
        Err(error) => {
            let class = error.class().unwrap_or_else(|| panic!("{source}: {error}"));
            assert_ne!(
                report.throws & (1 << class as u8),
                0,
                "{source}: {error}, {report:?}"
            );
        }
    }
    drop((facts, report));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn attached_script_calls_and_repeated_yields_match_execution() {
    for source in [
        "def once; yield 7; end; def run -> int; once {|x| x}; end",
        "def twice; yield 1; yield 2; end; def run -> int; sum=0; twice {|x| sum+=x}; sum; end",
        "def once; yield; end; def run; x=7; once {|x| x=\"shadow\"}; x+1; end",
        "def once; yield [2,3]; end; def run; once {|a,b| a+b}; end",
        "def once; yield 2,3; end; def run; once {_1+_2}; end",
        "def once; yield 2; end; def run; once {it+1}; end",
        "def once; yield; end; def run; a=[]; once {a.push(7)}; a[0]+1; end",
        "def once; yield; end; def run; a=[]; once {b=a; b.push(7)}; a; end",
        "def once; yield; end; def run; a={x:[1]}; once {a[:x].push(2)}; a; end",
        "def once; yield; end; def run; x=0; once {x+=1}; once {x+=2}; x; end",
        "def once; yield; end; def run; x=0; once {once {x+=1}; x+=2}; x; end",
        "def once; yield; end; def run; once {once {return 7}; \"bad\"}; \"bad\"; end",
    ] {
        witness(source, false);
    }
}

#[test]
fn block_presence_is_lexical_and_unused_blocks_are_not_executed() {
    for source in [
        "def ignore; 7; end; def run -> int; ignore {1+\"bad\"}; end",
        "def once; if block_given?; yield 7; else; \"bad\"; end; end; def run -> int; once {|x| x}; end",
        "def once; yield; end; def run; once {block_given?}; end",
        "def once; yield; end; def other; once {block_given?}; end; def run; other {7}; end",
        "def once; yield; end; def run; once {next 7; missing}; end",
    ] {
        witness(source, false);
    }
}

#[test]
fn block_transfers_and_captured_writes_survive_cleanup() {
    for source in [
        "def once; yield; missing; end; def run; once {break 7}; end",
        "def once; yield; missing; end; def run; once {return 7}; missing; end",
        "def once; yield; end; def run; x=0; result=once {x=7; break 9}; [result,x]; end",
        "def once; yield; end; def run; x=0; begin; once {x=7; return 9}; ensure; return x; end; end",
        "def once; begin; yield; ensure; return 9; end; end; def run; x=0; result=once {x=7; return 2}; [result,x]; end",
        "def once; begin; yield; ensure; return 9; end; end; def run; once {break 2}; end",
        "def once; yield; end; def run; once {begin; break 2; ensure; next 9; end}; end",
        "def once; yield; end; def run; once {begin; next 2; ensure; return 9; end}; end",
        "def once; yield; end; def run; once {begin; return 2; ensure; break 9; end}; end",
        "def repeat; while true; yield; end; 9; end; def run; repeat {break 7}; end",
        "def repeat; result=while true; yield; end; result; end; def run; repeat {break 7}; end",
        "def repeat; result=while true; yield; end; result; end; def run; repeat {break}; end",
    ] {
        witness(source, false);
    }
}

#[test]
fn block_errors_preserve_writes_before_rescue_and_retry() {
    for source in [
        "def once; yield; end; def run; x=0; begin; once {x=7; raise \"bad\"}; rescue; x; end; end",
        "def twice; begin; yield 1; rescue; yield 2; end; end; def run; x=0; twice {|n| x+=n; if n==1; raise \"bad\"; end}; x; end",
        "def once; yield; end; def run; x=0; begin; once {begin; raise \"bad\"; ensure; x=7; end}; rescue; x; end; end",
        "def once; begin; yield; rescue; 9; end; end; def run; x=0; result=once {x=7; raise \"bad\"}; [result,x]; end",
        "def once; yield; end; def run; begin; raise \"bad\"; rescue; once {raise}; end; end",
        "def once; yield; end; def run; x=0; begin; once {x=7; break 2}; ensure; x=9; end; x; end",
    ] {
        witness(source, false);
    }
}

#[test]
fn block_contracts_and_arguments_reject_before_or_after_callback_as_required() {
    for source in [
        "def once; yield 7; end; def run -> int; once {|x| \"bad\"}; end",
        "def once(x:int); yield; end; def run; once(\"bad\") {1+\"bad\"}; end",
        "def once -> int; yield; end; def run; x=0; begin; once {x=7; \"bad\"}; rescue; x; end; end",
        "def once -> int; yield; end; def run; x=0; begin; once {x=7; break \"bad\"}; rescue; x; end; end",
        "def once -> int; yield; end; def run -> int; once {return \"bad\"}; end",
        "def once; yield; end; def run; x=0; once {x=\"bad\"}; x+1; end",
    ] {
        witness(source, true);
    }
}

#[test]
fn attached_calls_route_all_body_and_cleanup_combinations() {
    let bodies = [
        "7",
        "next 7",
        "break 7",
        "return 7",
        "raise \"body\"",
        "1/0",
    ];
    let cleanup = [
        "9",
        "next 9",
        "break 9",
        "return 9",
        "raise \"cleanup\"",
        "1/0",
    ];
    for body in bodies {
        for cleanup in cleanup {
            witness(
                &format!(
                    "def once; yield; 11; end; def run; x=[]; begin; result=once {{begin; x.push(1); {body}; ensure; x.push(2); {cleanup}; end}}; [result,x]; rescue; x; end; end"
                ),
                false,
            );
        }
    }
    for body in bodies {
        for cleanup in ["9", "return 9", "raise \"cleanup\"", "1/0"] {
            witness(
                &format!(
                    "def once; begin; yield; 11; ensure; {cleanup}; end; end; def run; x=[]; begin; result=once {{x.push(1); {body}}}; [result,x]; rescue; x; end; end"
                ),
                false,
            );
        }
    }
}

#[test]
fn capture_effects_invalidate_predicates_and_keep_earlier_argument_values() {
    for source in [
        "def once; yield; end; def run; x=1; if (x>0)==(once {x=2; true}); if x==2; 7; else; 1+\"bad\"; end; end; end",
        "def pair(a,b); [a,b]; end; def once; yield; end; def run; x=1; pair(x,once {x=2}); end",
        "def once; yield; end; def run; x=1; result=x+(once {x=2}); if x==2; [result,x]; else; 1+\"bad\"; end; end",
        "def twice; yield; yield; end; def run; x=0; twice {x+=1}; if x==2; x; else; 1+\"bad\"; end; end",
        "def twice; begin; yield; rescue; yield; end; end; def run; x=0; twice {x+=1; if x==1; raise \"again\"; end}; if x==2; x; else; 1+\"bad\"; end; end",
        "def twice; yield; yield; end; def run; x=[]; twice {|x| x=[7]}; if x.empty?; 7; else; 1+\"bad\"; end; end",
        "def once; yield; end; def run; x=[]; copy=x; once {x.push(7)}; if copy.empty?; x; else; 1+\"bad\"; end; end",
        "def twice; begin; yield; rescue; yield; end; end; def run; x=[]; twice {x.push(7); if x.length==1; raise \"again\"; end}; x; end",
    ] {
        witness(source, false);
    }
}

#[test]
fn exit_errors_and_control_transfers_do_not_execute_the_normal_tail() {
    for source in [
        "def once; yield; missing; end; def run; begin; once {raise \"bad\"}; missing; rescue; 7; end; end",
        "def once; yield; missing; end; def run; once {break}; end",
        "def once; yield; missing; end; def run; once {return 7}; missing; end",
        "def once; begin; yield; ensure; raise \"bad\"; end; end; def run; begin; once {return 7}; missing; rescue; 9; end; end",
        "def once; yield; end; def run; x=[]; begin; once {x.push(7); raise \"bad\"}; rescue ZeroDivisionError; missing; rescue RuntimeError; x; end; end",
        "def once; yield; end; def run; once {while true; break 7; end; 9}; end",
        "def once; yield; end; def run; n=0; begin; once {n+=1; if n<2; raise \"retry\"; end}; rescue; retry; end; n; end",
        "def twice; yield; yield; end; def run; done=false; result=twice {done=true; break 7}; if done; result; else; missing; end; end",
    ] {
        witness(source, false);
    }
    let source =
        "def once -> int; yield; end; def run; begin; once {\"bad\"}; missing; rescue; 7; end; end";
    witness(source, true);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert_eq!(report.issues.data.len(), 1, "{report:?}");
    assert!(matches!(
        report.issues.data[0].issue.kind,
        super::flow::IssueKind::Return { .. }
    ));
}

#[test]
fn block_contexts_distinguish_capture_types_presence_and_recursive_calls() {
    for source in [
        "def once; yield; end; def echo(x); once {x}; end; def run; [echo(7),echo(\"yes\")]; end",
        "def once; yield; end; def value; once {block_given?}; end; def run; [value(),value {7}]; end",
        "def once; yield; end; def build(n:int); a=[]; if n>0; once {a=[build(n-1)]}; end; a; end; def run -> array; build(3); end",
        "def visit(xs,n:int); if n>0; visit([xs],n-1) {|x| x}; else; yield xs; end; end; def run -> array; visit([],3) {|x| x}; end",
        "def once; yield; end; def left(n:int); if n>0; once {[right(n-1)]}; else; []; end; end; def right(n:int); once {left(n)}; end; def run -> array; left(3); end",
    ] {
        witness(source, false);
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(report.contexts < 20, "{source}: {report:?}");
    }
}

#[test]
fn native_blocks_and_retained_mutations_stay_incomplete() {
    for source in [
        "def once; yield; end; def run; a=[[1]]; a[0].push(once {a[0]=[2]; 7}); a; end",
        "def once; yield; end; def run; a=[1]; a[-1]+=once {a.push(2); 7}; a; end",
        "def once; yield; end; def run; a=[1]; a.push(once {a=a; 7}); a; end",
        "def run; [1].uniq {|x| x}; end",
        "def run; [1].sort_by {|x| x}; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    witness(
        "def once; yield; end; def run; a=[1]; a.push(once {a.length}); a; end",
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def twice; begin; yield true; rescue; yield false; end; end; def run(flag:bool); x=[1]; begin; result=twice {|v| begin; x.push(2); if flag; return x; elsif v; raise \"bad\"; else; break 7; end; ensure; x.push(3); end}; [result,x]; ensure; x.push(4); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert!(report.contexts >= 3);
    Ok(())
}

#[test]
fn yield_contexts_and_exit_snapshots_obey_exact_quotas_and_release_failures() {
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
fn yield_analysis_keeps_cancellation_and_deadlines_latched() {
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
fn direct_capture_writes_and_exit_values_remain_exact() {
    for source in [
        "def once; yield; end; def run; x=0; once {x=7}; x; end",
        "def twice; yield; yield; end; def run; a=[]; twice {a.push(7)}; a; end",
        "def twice; yield; yield; end; def run; a=[]; result=twice {a.push(7); break 9}; [a,result]; end",
        "def once; yield; end; def run; x=0; begin; once {x=7; return 2}; ensure; return x; end; end",
        "def twice; begin; yield; rescue; yield; end; end; def run; a=[]; done=false; twice {a.push(7); unless done; done=true; raise \"again\"; end}; a; end",
        "def once; yield; end; def run; x=0; begin; once {x=7; raise \"bad\"}; rescue; x; end; end",
        "def once; begin; yield; ensure; return 9; end; end; def run; x=0; result=once {x=7; return 2}; [x,result]; end",
        "def once; yield; end; def run; a=[1]; a.push(once {a; 7}); a; end",
    ] {
        let actual = Engine::new()
            .compile(source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        let actual = literal_fact(&mut ctx, &mut facts, &actual);
        assert!(
            report.incomplete.data.is_empty() && report.issues.data.is_empty(),
            "{source}: {report:?}"
        );
        assert_eq!(
            report.returns,
            actual,
            "{source}: expected {:?}, actual {:?}",
            facts.node(actual),
            facts.node(report.returns)
        );
    }
}

#[test]
fn long_block_call_chains_use_the_default_stack_and_iterative_solver() {
    let mut source = String::from("def once; yield; end; def run; f0; end;");
    for i in 0..64 {
        let body = if i == 63 {
            "7".to_string()
        } else {
            format!("f{}", i + 1)
        };
        source.push_str(&format!("def f{i}; once {{{body}}}; end;"));
    }
    witness(&source, false);
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, &source).unwrap();
    assert_eq!(report.contexts, 193);
    assert_eq!(report.returns, facts.integer(&mut ctx, 7).unwrap());
}

#[test]
fn attached_script_call_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-yield.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 22);
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
fn yield_propagates_each_ordinary_error_and_keeps_input_guards_catchable() {
    use crate::ErrorClass;
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
        let class = class.name();
        witness(
            &format!("def once; yield; end; def run; once {{raise {class}, \"bad\"}}; end"),
            false,
        );
        witness(
            &format!(
                "def once; yield; end; def run; x=0; begin; once {{x=7; raise {class}, \"bad\"}}; rescue {class}; x; end; end"
            ),
            false,
        );
        witness(
            &format!(
                "def once; begin; yield; rescue {class}; 9; end; end; def run; x=0; result=once {{x=7; raise {class}, \"bad\"}}; [result,x]; end"
            ),
            false,
        );
    }
    witness(
        "def once; yield; end; def run; x=0; begin; once {x=7; /a/.match((\"x\"*1024)*1025)}; rescue LimitError; x; end; end",
        false,
    );
}
