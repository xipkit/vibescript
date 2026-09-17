use super::lexical_tests::witness;
use super::{collection_tests::analyze, facts::Facts};
use crate::{CallContext, CallOptions};

#[test]
fn forwarding_keeps_each_lexical_environment_and_updates_both_capture_stores() {
    for source in [
        "def once; yield; end; def outer; once {yield 7}; end; def run; outer {|v| v}; end",
        "def once; yield; end; def outer; x=1; result=once {x=2; yield x}; [x,result]; end; def run; x=0; result=outer {|v| x=v; x+1}; [x,result]; end",
        "def twice; yield; yield; end; def outer; a=[]; result=twice {a.push(1); yield a.length}; [a,result]; end; def run; a=[]; result=outer {|v| a.push(v); a.length}; [a,result]; end",
        "def once; yield; end; def middle; once {yield 7}; end; def outer; x=[]; result=middle {|v| x.push(v); yield v+1}; [x,result]; end; def run; x=[]; result=outer {|v| x.push(v); v+1}; [x,result]; end",
        "def once; yield; end; def outer; once {once {once {yield 7}}}; end; def run; x=[]; result=outer {|v| x.push(v); v+1}; [x,result]; end",
        "def once; yield; end; def outer; once {yield [7,9]}; end; def run; outer {|x,y| [x,y]}; end",
    ] {
        witness(source, false, false);
    }
}

#[test]
fn forwarded_returns_leave_the_original_home_and_apply_its_contract() {
    for source in [
        "def once; yield; 99; end; def outer; once {yield}; 88; end; def run; outer {return 7}; 77; end",
        "def once; yield; 99; end; def outer; once {return 7}; 88; end; def run; [outer {return 9},77]; end",
        "def once; yield; end; def middle; once {yield}; 88; end; def outer; middle {yield}; 77; end; def run; outer {return 7}; 99; end",
        "def once; yield; end; def middle; once {yield}; 88; end; def outer; middle {return 7}; 77; end; def run; [outer {return 9},99]; end",
        "def once -> string; yield; \"done\"; end; def outer -> string; once {yield}; \"done\"; end; def run -> int; outer {return 7}; 99; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn forwarded_break_is_consumed_by_the_nearest_dynamic_loop_or_receiver() {
    for source in [
        "def once; yield; 99; end; def outer; result=once {yield; 88}; [result,77]; end; def run; outer {break 7}; end",
        "def once; yield; 99; end; def outer; result=once {yield; 88}; [result,77]; end; def run; outer {break}; end",
        "def once; yield; 99; end; def outer; result=once {yield; 88}; [result,77]; end; def run; outer {next 7}; end",
        "def once; yield; end; def outer; once {a=[]; for n in [1,2]; a.push(yield n); end; a}; end; def run; outer {break 7}; end",
        "def once; yield; end; def outer; for n in [1,2]; once {yield n}; end; 9; end; def run; outer {break 7}; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn forwarding_preserves_cleanup_replacement_and_writes_from_each_home() {
    for body in ["7", "next 7", "break 7", "return 7", "raise \"body\""] {
        for cleanup in ["9", "next 9", "break 9", "return 9", "raise \"cleanup\""] {
            witness(
                &format!(
                    "def once; yield; end; def outer; x=[]; result=once {{begin; x.push(1); yield; ensure; x.push(2); {cleanup}; end}}; [x,result]; end; def run; x=[]; begin; result=outer {{x.push(7); {body}}}; [x,result]; rescue; x; end; end"
                ),
                true,
                false,
            );
        }
    }
    witness(
        "def twice; begin; yield; ensure; yield; end; end; def outer; twice {yield}; end; def run; x=[]; begin; outer {x.push(7); return 3}; ensure; return x; end; end",
        true,
        false,
    );
    witness(
        "def once; yield; end; def outer; x=[]; result=once {begin; yield; ensure; x.push(9); end}; [x,result]; end; def run; x=[]; result=outer {begin; x.push(7); return x; ensure; next 3; end}; [x,result]; end",
        true,
        false,
    );
}

#[test]
fn forwarded_errors_use_dynamic_rescue_context_and_keep_state_on_retry() {
    for class in [
        "RuntimeError",
        "StandardError",
        "AssertionError",
        "LimitError",
        "TypeError",
        "ZeroDivisionError",
        "LocalJumpError",
        "ArgumentError",
    ] {
        witness(
            &format!(
                "def once; yield; end; def outer; x=[]; begin; once {{x.push(1); yield;}}; rescue {class}; x; end; end; def run; x=[]; result=outer {{x.push(7); raise {class}, \"bad\"}}; [x,result]; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def once; begin; raise {class}, \"bad\"; rescue; yield; end; end; def outer; once {{yield}}; end; def run; begin; outer {{raise}}; rescue {class}; 7; end; end"
            ),
            true,
            false,
        );
    }
    witness(
        "def once; yield; end; def outer; begin; once {yield}; rescue; retry; end; end; def run; x=[]; outer {x.push(7); if x.length<2; raise \"again\"; end}; x; end",
        false,
        false,
    );
}

#[test]
fn forwarding_keeps_block_presence_parameters_and_value_semantics() {
    for source in [
        "def once; yield; end; def outer; once {block_given?}; end; def run; [outer,outer {7}]; end",
        "def once; yield; end; def outer; once {once {block_given?}}; end; def run; [outer,outer {7}]; end",
        "def once; yield; end; def outer; once {yield}; end; def run; begin; outer; rescue LocalJumpError; 7; end; end",
        "def once; yield 3; end; def outer; x=0; result=once {|x| yield x; x}; [x,result]; end; def run; x=1; result=outer {|x| x=7; x}; [x,result]; end",
        "def once; yield; end; def outer; once {yield [7,[8,9]]}; end; def run; outer {|x,(y,z)| [x,y,z]}; end",
        "def once; yield; end; def outer; once {yield 7,9}; end; def run; outer {[_1,_2]}; end",
        "def once; yield; end; def outer; once {yield 7}; end; def run; x=[]; copy=x; result=outer {|v| x.push(v); copy}; [x,copy,result]; end",
        "def once; yield; end; def outer; once {yield}; end; def run; x=[]; result=outer {copy=x;copy.push(7)}; [x,result]; end",
    ] {
        witness(source, true, source.contains("rescue LocalJumpError"));
    }
    witness(
        "def once; yield; end; def outer; once {yield}; end; def run; x=0; outer {x=\"bad\"}; x; end",
        true,
        true,
    );
    witness(
        "def once; yield; end; def outer -> int; once {return \"bad\"}; end; def run; begin; outer {return 7}; rescue; 9; end; end",
        true,
        true,
    );
}

#[test]
fn forwarded_captures_keep_match_data_protected_and_copies_independent() {
    witness(
        "def once; yield; end; def outer; once {yield}; end; def run; m=/(a)/.match(\"a\"); if m; begin; outer {m.captures.push(7)}; rescue; m.captures; end; end; end",
        false,
        true,
    );
    witness(
        "def once; yield; end; def outer; once {yield}; end; def run; m=/(a)/.match(\"a\"); if m; c=m.captures; outer {c.push(\"x\")}; [c,m.captures]; end; end",
        false,
        false,
    );
}

#[test]
fn forwarding_layers_use_the_default_stack_and_keep_return_homes_distinct() {
    for depth in [2, 8, 24] {
        for body in [
            "x.push(7)",
            "x.push(7); return 9",
            "x.push(7); break 9",
            "x.push(7); raise \"bad\"",
        ] {
            let mut source = String::from("def layer0; yield; end;");
            for i in 1..=depth {
                source.push_str(&format!("def layer{i}; layer{} {{yield}}; end;", i - 1));
            }
            source.push_str(&format!("def run; x=[]; begin; result=layer{depth} {{{body}}}; [x,result]; rescue; x; ensure; return x; end; end"));
            witness(&source, true, false);
        }
    }
}

#[test]
fn stable_recursion_converges_and_expanding_environments_remain_explicit() {
    use super::{collection_tests::analyze, facts::Facts};
    use crate::{CallContext, CallOptions};
    for source in [
        "def once; yield; end; def outer(n:int); if n>0; once {yield n}; outer(n-1); else; 7; end; end; def run; x=[]; outer(3) {|v| x.push(v)}; x; end",
        "def once; yield; end; def outer(n:int); if n>0; outer(n-1) {|v| v}; else; once {yield 7}; end; end; def run; outer(3) {|v| v}; end",
    ] {
        // Missing blocks in the first recursive program are rescued by the caller below.
        let source = source.replace("outer(n-1);", "begin; outer(n-1); rescue; 7; end;");
        witness(&source, false, source.contains("begin"));
    }
    for source in [
        "def outer(n:int); if n>0; outer(n-1) {yield}; else; yield; end; end; def run; outer(3) {7}; end",
        "def left(n:int); if n>0; right(n-1) {yield}; else; yield; end; end; def right(n:int); left(n) {yield}; end; def run; left(3) {7}; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(report.contexts < 20, "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def twice; begin; yield; ensure; yield; end; end; def outer; x=[]; result=twice {begin; x.push(1); yield; ensure; x.push(2); end}; [x,result]; end; def run(flag:bool); x=[]; begin; result=outer {x.push(7); if flag; return x; else; raise \"bad\"; end}; [x,result]; rescue; x; ensure; x.push(9); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn forwarded_environments_and_control_routes_obey_exact_quotas_and_cleanup() {
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
fn forwarding_analysis_observes_latched_cancellation_and_deadlines() {
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
fn forwarding_reference_decisions_have_independent_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-forwarding.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 18);
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
    assert_eq!(differences, 6);
}
