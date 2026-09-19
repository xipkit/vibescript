use super::lexical_tests::witness;
use super::{collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime};
use crate::{CallContext, CallOptions, Value};

#[test]
fn literal_windows_preserve_width_order_partial_slices_and_destructuring() {
    for body in [
        "a=[]; result=[7,9,11].each_slice(2) {|pair| a.push(pair)}; [a,result]",
        "a=[]; result=[7,9,11].each_cons(2) {|pair| a.push(pair)}; [a,result]",
        "a=[]; [7,9,11].each_slice(2) {|a0,b| a.push([a0,b])}; a",
        "a=[]; [7,9,11].each_cons(2) {|a0,b| a.push([a0,b])}; a",
        "a=[]; [7,9].each_cons(1) {|a0,b| a.push([a0,b])}; a",
        "a=[]; [7,9].each_slice(9223372036854775807) {|pair| a.push(pair)}; a",
        "[7,9].each_cons(9223372036854775807) {missing}",
        "[].each_slice(1) {missing}",
        "[].each_cons(1) {missing}",
        "a=[]; [7,9,11].each_cons(2) {a.push([_1,_2])}; a",
        "a=[[7],[9],[11]]; copy=a; result=a.each_slice(2) {|pair| pair[0].push(3); a.clear}; [a,copy,result]",
        "a=[7,9,11]; seen=[]; a.each_cons(2) {|pair| a[2]=3; seen.push(pair)}; [a,seen]",
        "a=[7,9]; seen=[]; a.each_cons(begin; a.clear; 1; end) {|pair| seen.push(pair)}; [a,seen]",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn finite_cycles_preserve_order_and_empty_cycles_skip_callbacks() {
    for body in [
        "a=[]; result=[7,9].cycle(1) {|n| a.push(n)}; [a,result]",
        "a=[]; result=[7,9].cycle(2) {|n| a.push(n)}; [a,result]",
        "[7].cycle(0) {missing}",
        "[7].cycle(-1) {missing}",
        "[].cycle {missing}",
        "[].cycle(nil) {missing}",
        "a=[7,9]; seen=[]; a.cycle(2) {|n| seen.push(n); a.clear}; [a,seen]",
        "a=[]; result=[7,9].cycle {a.push(3); break 7}; [a,result]",
        "[7,9].cycle(nil) {return 7}; missing",
        "a=[]; result=[7].cycle(9223372036854775807) {a.push(3); break 7}; [a,result]",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn numeric_iterations_keep_endpoints_direction_and_receiver_results() {
    for body in [
        "a=[]; result=1.times {|n| a.push(n)}; [a,result]",
        "a=[]; result=2.times {|n| a.push(n)}; [a,result]",
        "a=[]; result=7.upto(9) {|n| a.push(n)}; [a,result]",
        "a=[]; result=9.downto(7) {|n| a.push(n)}; [a,result]",
        "a=[]; result=7.step(11,2) {|n| a.push(n)}; [a,result]",
        "a=[]; result=11.step(7,-2) {|n| a.push(n)}; [a,result]",
        "a=[]; result=(7..11).step(2) {|n| a.push(n)}; [a,result]",
        "a=[]; result=(11...7).step(2) {|n| a.push(n)}; [a,result]",
        "a=[]; (-9223372036854775808..9223372036854775807).step(9223372036854775807) {|n| a.push(n)}; a",
        "a=[]; 9223372036854775807.step(-9223372036854775808,-9223372036854775808) {|n| a.push(n)}; a",
        "a=[]; 9223372036854775807.upto(9223372036854775807) {|n| a.push(n)}; a",
        "a=[]; (-9223372036854775808).downto(-9223372036854775808) {|n| a.push(n)}; a",
        "0.times {missing}",
        "(-7).times {missing}",
        "9.upto(7) {missing}",
        "7.downto(9) {missing}",
        "7.step(9,-1) {missing}",
        "9.step(7,1) {missing}",
        "(7...7).step(1) {missing}",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn single_callbacks_keep_copies_and_choose_the_correct_return_value() {
    for body in [
        "7.tap {|n| n+9}",
        "7.yield_self {|n| [n,9]}",
        "nil.yield_self {|n| n}",
        "false.tap {true}",
        "[7,9].yield_self {|a,b| [b,a]}",
        "a=[7]; result=a.tap {|n| n.push(9); a.clear}; [a,result]",
        "a=[7]; result=a.yield_self {|n| n.push(9); a.clear; n}; [a,result]",
        "{a:7}.tap {|n| n[:a]=9}",
        "{a:7}.yield_self {|n| n[:a]}",
        "{tap:7}.tap",
        "{yield_self:7}.yield_self",
        "7.tap {break 9}",
        "7.yield_self {return 9}; missing",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn scheduled_validation_precedes_callbacks_and_preserves_keyword_rules() {
    for call in [
        "[].each_slice",
        "[].each_cons(0) {x.push(9)}",
        "[].each_slice(-1) {x.push(9)}",
        "[7].each_cons(1.0) {x.push(9)}",
        "[7].each_slice(1,2) {x.push(9)}",
        "[7].cycle(\"bad\") {x.push(9)}",
        "[7].cycle(1,2) {x.push(9)}",
        "[].cycle",
        "0.times",
        "2.times(1) {x.push(9)}",
        "7.upto {x.push(9)}",
        "7.upto(\"bad\") {x.push(9)}",
        "7.downto(1,bad:3) {x.push(9)}",
        "7.step(9,0) {x.push(9)}",
        "7.step(9,1.0) {x.push(9)}",
        "(1..3).step(0) {x.push(9)}",
        "(1..3).step(-1) {x.push(9)}",
        "(1..3).step(1,bad:3) {x.push(9)}",
        "(..3).step(1) {x.push(9)}",
        "(1..).step(1) {x.push(9)}",
        "7.tap",
        "7.yield_self(1) {x.push(9)}",
        "7.tap(bad:3) {x.push(9)}",
        "{}.tap(x.push(7)) {x.push(9)}",
        "[7].times {x.push(9)}",
        "7.each_slice(1) {x.push(9)}",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; x.push(11); rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for call in [
        "[7,9].each_slice(1,ignored:3) {|n| n}",
        "[7,9].each_cons(1,ignored:3) {|n| n}",
        "[7].cycle(1,ignored:3) {true}",
        "1.times(ignored:3) {true}",
    ] {
        witness(&format!("def run; {call}; end"), true, false);
    }
}

#[test]
fn generic_schedules_converge_without_executing_counts_or_infinite_paths() {
    for (body, ty, inputs) in [
        (
            "a=[]; xs.each_slice(2) {|pair| a.push(pair)}; a",
            "array<int>",
            vec![
                Value::array(vec![]),
                Value::array(vec![Value::int(7), Value::int(9), Value::int(11)]),
            ],
        ),
        (
            "a=[]; xs.each_cons(2) {|a0:int,b:int| a.push([a0,b])}; a",
            "array<int>",
            vec![
                Value::array(vec![]),
                Value::array(vec![Value::int(7), Value::int(9), Value::int(11)]),
            ],
        ),
        (
            "a=[]; xs.cycle(2) {|n| a.push(n)}; a",
            "array<int>",
            vec![
                Value::array(vec![]),
                Value::array(vec![Value::int(7), Value::int(9)]),
            ],
        ),
        (
            "a=[]; xs.times {|n| a.push(n)}; a",
            "int",
            vec![Value::int(0), Value::int(3)],
        ),
        (
            "a=[]; 7.upto(xs) {|n| a.push(n)}; a",
            "int",
            vec![Value::int(6), Value::int(9)],
        ),
        (
            "a=[]; 9.downto(xs) {|n| a.push(n)}; a",
            "int",
            vec![Value::int(10), Value::int(7)],
        ),
        (
            "a=[]; 7.step(11,xs) {|n| a.push(n)}; a",
            "int",
            vec![Value::int(-1), Value::int(2)],
        ),
        ("xs.tap {|n| n+1}", "int", vec![Value::int(7)]),
    ] {
        let source = format!("def run(xs:{ty}); {body}; end");
        for input in inputs {
            inferred_runtime(&source, &[input], false);
        }
    }
    for source in [
        "def run; [7].cycle {}; missing; end",
        "def run; [7].cycle(nil) {}; missing; end",
        "def run; [7,9].cycle {next}; missing; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(
            report.issues.data.is_empty() && report.incomplete.data.is_empty(),
            "{source}: {report:?}"
        );
        assert_eq!(report.returns, super::facts::Atom::Never.fact());
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn scheduled_control_and_cleanup_preserve_writes_and_lexical_returns() {
    for call in [
        "[7,9].each_slice(1)",
        "[7,9].each_cons(1)",
        "[7,9].cycle(3)",
        "[7,9].cycle",
        "3.times",
        "7.upto(9)",
        "9.downto(7)",
        "7.step(9)",
        "(7..9).step(1)",
        "7.tap",
        "7.yield_self",
    ] {
        for transfer in ["break 7", "break", "return 7", "raise \"bad\""] {
            witness(
                &format!(
                    "def run; a=[]; begin; result={call} {{a.push(3); {transfer}}}; [a,result]; rescue; a; ensure; a.push(11); end; end"
                ),
                true,
                false,
            );
        }
    }
    for body in ["7", "next 7", "break 7", "return 7", "raise \"body\""] {
        for cleanup in ["9", "next 9", "break 9", "return 9", "raise \"cleanup\""] {
            witness(
                &format!(
                    "def run; x=[]; begin; result=[7,9].each_slice(1) {{begin; x.push(3); {body}; ensure; x.push(5); {cleanup}; end}}; [x,result]; rescue; x; end; end"
                ),
                true,
                false,
            );
        }
    }
    witness(
        "def outer; [7].cycle {yield}; end; def run; outer {return 7}; missing; end",
        true,
        false,
    );
    witness(
        "def run; x=[]; begin; [7].each_slice(1) {x.push(7); if x.length<2; raise \"again\"; end}; rescue; retry; end; x; end",
        false,
        false,
    );
}

#[test]
fn scheduled_error_classes_and_type_failures_keep_completed_effects() {
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
                "def run; x=[]; begin; [7].cycle {{begin; x.push(3); raise {class}, \"bad\"; ensure; x.push(5); end}}; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def run; x=[]; begin; begin; raise {class}, \"bad\"; rescue; 3.times {{x.push(3); raise}}; end; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
    }
    for source in [
        "def run; x=[]; begin; [7,9].each_slice(1) {|n:int| x.push(7)}; rescue; x; end; end",
        "def run; x=[]; begin; [7,9,11].each_slice(2) {|a:int,b:int| x.push(7)}; rescue; x; end; end",
        "def run; x=[]; begin; 3.times {|n:string| x.push(7)}; rescue; x; end; end",
        "def run; x=[]; begin; [7].cycle(3) {|n:string| x.push(7)}; rescue; x; end; end",
        "def run; x=0; 7.tap {x=\"bad\"}; x; end",
        "def owner -> int; [7].cycle {return \"bad\"}; end; def run; begin; owner; rescue; 7; end; end",
    ] {
        witness(source, true, true);
    }
}

#[test]
fn mixed_schedule_arguments_keep_successful_paths_and_invalid_alternatives() {
    for method in ["each_slice", "each_cons"] {
        let source = format!(
            "def run(width:int|string); x=[]; begin; [7,9].{method}(width) {{|pair| x.push(pair)}}; rescue RuntimeError; x.push(3); end; x; end"
        );
        for width in [
            Value::int(0),
            Value::int(1),
            Value::int(3),
            Value::bytes("bad"),
        ] {
            inferred_runtime(&source, &[width], true);
        }
    }
    let source = "def run(n:int|nil|string); x=[]; begin; [7,9].cycle(n) {x.push(3); break 7}; rescue; x.push(5); end; x; end";
    for n in [
        Value::nil(),
        Value::int(0),
        Value::int(2),
        Value::bytes("bad"),
    ] {
        inferred_runtime(source, &[n], true);
    }
    let source = "def run(n:int|string); x=[]; begin; 7.step(9,n) {x.push(3)}; rescue; x.push(5); end; x; end";
    for n in [
        Value::int(-1),
        Value::int(0),
        Value::int(1),
        Value::bytes("bad"),
    ] {
        inferred_runtime(source, &[n], true);
    }
    let source =
        "def run(n:int); x=[]; begin; (1..3).step(n) {x.push(3)}; rescue; x.push(5); end; x; end";
    for n in [Value::int(-1), Value::int(0), Value::int(1), Value::int(3)] {
        inferred_runtime(source, &[n], false);
    }
    let source = "def run(xs:array<int>|int); x=[]; begin; xs.each_cons(1) {x.push(3)}; rescue; x.push(5); end; x; end";
    for xs in [Value::int(7), Value::array(vec![Value::int(7)])] {
        inferred_runtime(source, &[xs], true);
    }
}

#[test]
fn integer_bound_guards_are_catchable_before_callbacks_and_later_effects() {
    let large = CallContext::new(CallOptions::default())
        .parse_integer("1000000000000000000000000000000", 10)
        .unwrap();
    for call in [
        "(n).times",
        "(n).upto(7)",
        "7.upto(n)",
        "7.downto(n)",
        "7.step(n)",
        "7.step(9,n)",
        "(1..3).step(n)",
    ] {
        let source = format!(
            "def run(n:int); x=[]; begin; {call} {{x.push(7)}}; x.push(9); rescue LimitError; x; end; end"
        );
        let actual = inferred_runtime(&source, std::slice::from_ref(&large), false);
        assert_eq!(actual.to_string(), "[]", "{source}");
    }
    for call in ["[7].cycle(n)", "[7].each_slice(n)", "[7].each_cons(n)"] {
        let source = format!(
            "def run(n:int); x=[]; begin; {call} {{x.push(7)}}; x.push(9); rescue RuntimeError; x; end; end"
        );
        assert_eq!(
            inferred_runtime(&source, std::slice::from_ref(&large), false).to_string(),
            "[]",
            "{source}"
        );
    }
}

#[test]
fn scheduled_copies_keep_protection_and_construction_guards() {
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; c=m.captures; [7].cycle(2) {c.push(\"x\")}; [c,m.captures]; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; m.yield_self {|n| n.captures}; end; end",
    ] {
        witness(source, false, false);
    }
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; begin; m.tap {|n| n.captures.push(\"x\")}; rescue; m.captures; end; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; copy=m.dup; begin; [7].each_slice(1) {copy.captures.clear}; rescue; copy.captures; end; end; end",
    ] {
        witness(source, false, true);
    }
    let mut value = Value::int(7);
    for _ in 0..crate::budget::MAX_VALUE_DEPTH - 1 {
        value = Value::array(vec![value]);
    }
    let input = Value::array(vec![value]);
    let source = "def run(xs:array<any>); x=[]; begin; xs.each_cons(1) {|pair| x.push(7); [pair]}; x.push(9); rescue LimitError; x; end; end";
    assert_eq!(
        inferred_runtime(source, std::slice::from_ref(&input), false).to_string(),
        "[7]"
    );
    let source = "def run(xs:array<any>); x=[]; result=xs.each_cons(1) {x.push(7); break 9}; [x,result]; end";
    assert_eq!(
        inferred_runtime(source, &[input], false).to_string(),
        "[[7], 9]"
    );
}

#[test]
fn large_counts_converge_and_impossible_windows_do_not_allocate_their_width() {
    for body in [
        "[7].cycle(9223372036854775807) {}",
        "9223372036854775807.times {}",
        "(-9223372036854775808).upto(9223372036854775807) {}",
        "9223372036854775807.downto(-9223372036854775808) {}",
        "(-9223372036854775808..9223372036854775807).step(1) {}",
        "[7].each_cons(9223372036854775807) {missing}",
        "a=[]; [7].cycle(9223372036854775807) {a=[a]}; a",
    ] {
        let source = format!("def run; {body}; end");
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &source).unwrap();
        assert!(
            report.incomplete.data.is_empty() && report.issues.data.is_empty(),
            "{source}: {report:?}"
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for source in [
        "def run; [7].cycle {}; missing; end",
        "def run; 9223372036854775807.times {}; missing; end",
    ] {
        let error = crate::Engine::new()
            .compile(source)
            .unwrap()
            .call(
                "run",
                &[],
                CallOptions {
                    limits: crate::Limits {
                        steps: Some(256),
                        ..crate::Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, crate::ErrorKind::Steps);
    }
    let mut ctx = CallContext::new(CallOptions {
        limits: crate::Limits {
            steps: Some(256),
            ..crate::Limits::default()
        },
        ..CallOptions::default()
    });
    let mut facts = Facts::new(&mut ctx).unwrap();
    let error = analyze(
        &mut ctx,
        &mut facts,
        "def run(xs:array<int>); xs.each_cons(9223372036854775807) {}; end",
    )
    .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Steps);
    assert_eq!(ctx.checkpoint().unwrap_err(), error);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn nested_schedules_and_recursive_callbacks_stay_on_the_default_stack() {
    for depth in [2, 8, 24] {
        for prefix in ["[7].each_cons(1) {", "1.times {", "7.yield_self {"] {
            let body = format!("{}7{}", prefix.repeat(depth), "}".repeat(depth));
            witness(&format!("def run; {body}; end"), true, false);
        }
    }
    witness(
        "def recurse(n:int); if n>0; [7].cycle(1) {recurse(n-1)}; else; 7; end; end; def run; recurse(7); end",
        false,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run(xs:array<int>,count:int); a=[]; begin; xs.each_cons(2) {|l,r| a.push(l)}; xs.cycle(count) {|n| a.push(n)}; a; ensure; a.push(7); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn schedule_fixed_points_have_exact_quotas_and_reclaim_interrupted_allocations() {
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
fn schedule_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn unmodeled_receivers_callbacks_and_pending_addresses_stay_explicit() {
    for source in [
        "def run(v); v.tap {|n| n}; end",
        "def run(h:hash<string,int>); h.tap {|n| n}; end",
        "def run; {tap:7}.tap {missing}; end",
        "def run; [7,9].map! {|n| n}; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn schedule_reference_decisions_have_independent_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-schedules.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 50);
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
    assert_eq!(differences, 28);
}
