use super::lexical_tests::witness;
use super::{collection_tests::analyze, facts::Facts};
use crate::{CallContext, CallOptions, Value};

#[test]
fn predicates_stop_before_unreachable_callbacks_and_keep_exact_indices() {
    for body in [
        "a=[]; result=[7,9].find {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,9].index {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,9].find_index {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,9].rindex {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,9].any? {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,9].all? {|n| a.push(n); false}; [result,a]",
        "a=[]; result=[7,9].none? {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,9,11].one? {|n| a.push(n); true}; [result,a]",
        "a=[]; result=[7,false,9].take_while {|n| a.push(n); n}; [result,a]",
        "[1,2].find(nil) {true}",
        "[7,false,nil,true].count {|v| v}",
        "[false,7,8].one?",
        "[false,7,8].any?",
        "[false,7,8].all?",
        "[false,nil].none?",
        "[7,false,nil,true].count",
        "[7,9,7].index(7)",
        "[7,9,7].rindex(7)",
        "[7,9,7].count(7)",
        "[1,2].count(1.0)",
        "[1,2].count(1..2)",
        "[1,2].any?(1..2)",
        "[\"a\",\"b\"].any?(/a/)",
        "[\"a\",\"b\"].count(/a/)",
        "{find:7}.find",
        "{reduce:7,count:9}.count",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn empty_identities_and_unseeded_reductions_skip_unused_callbacks() {
    for body in [
        "[].reduce {|a:string,n| missing}",
        "[7].reduce {|a:string,n| missing}",
        "[].reduce(9) {missing}",
        "[].reduce(9,\"arbitrary\") {missing}",
        "[7].reduce(\"arbitrary\")",
        "[].find {missing}",
        "[].index {missing}",
        "[].rindex {missing}",
        "[].sum {missing}",
        "[].sum(7) {missing}",
        "[].count {missing}",
        "[].any? {missing}",
        "[].all? {missing}",
        "[].none? {missing}",
        "[].one? {missing}",
        "[].take_while {missing}",
        "(1...1).reduce(7) {missing}",
        "(1...1).find {missing}",
        "(3..1).count",
        "(9223372036854775807..9223372036854775807).count",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn reduction_seeds_copies_and_operator_modes_follow_runtime_contracts() {
    for body in [
        "[1,2,3].reduce {|a,n| a+n}",
        "[1,2,3].reduce(7) {|a,n| a+n}",
        "a=[]; result=[1,2,3].reduce(7) {|a,n| a+n}; [a,result]",
        "[1,2,3].reduce(\"+\")",
        "[1,2,3].reduce(7,\"+\") {missing}",
        "[1,2,3].reduce(7,:+) {missing}",
        "a=[]; result=[7,9].reduce([]) {|sum,n| a.push(n); sum.push(n)}; [a,result]",
        "a=[]; result=[7,9].reduce(a) {|sum,n| sum.push(n)}; [a,result]",
        "[1,2,3].sum",
        "[1,2,3].sum(7)",
        "a=[]; result=[7,9].sum {|n| a.push(n); n}; [a,result]",
        "[\"a\",\"b\"].sum(\"\") {|n| n}",
        "[[7],[9]].sum([])",
        "[[7],[9]].reduce([],\"+\")",
        "[7,9].reduce([],\"<<\")",
        "[[7],[9]].reduce([7,9],\"&\")",
        "[[7],[9]].reduce([7,9],\"-\")",
        "(1..3).reduce(7) {|a,n| a+n}",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn patterns_and_operators_ignore_attached_blocks_and_their_captures() {
    for body in [
        "a=[]; result=[1,2].count(1) {a.push(9); missing}; [result,a]",
        "a=[]; result=[1,2].any?(1..2) {a.push(9); missing}; [result,a]",
        "a=[]; result=[1,2].all?(1..2) {a.push(9); missing}; [result,a]",
        "a=[]; result=[1,2].none?(3..4) {a.push(9); missing}; [result,a]",
        "a=[]; result=[1,2].reduce(7,\"+\") {a.push(9); missing}; [result,a]",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn predicate_and_reducer_validation_precedes_callback_effects() {
    for call in [
        "[].reduce",
        "[].find",
        "[].take_while",
        "[].index",
        "[].rindex",
        "[1].find(7) {x.push(9)}",
        "[1].find(nil,nil) {x.push(9)}",
        "[1].any?(bad:7) {x.push(9)}",
        "[1].all?(bad:7) {x.push(9)}",
        "[1].none?(bad:7) {x.push(9)}",
        "[1].sum(bad:7) {x.push(9)}",
        "[1].one?(7) {x.push(9)}",
        "[1].index(7) {x.push(9)}",
        "[].reduce(7,1) {x.push(9)}",
        "[].reduce(7)",
        "(1..3).find(nil) {x.push(9)}",
        "(1..3).count(7) {x.push(9)}",
        "(1..3).reduce(7,\"+\") {x.push(9)}",
        "(1..3).reduce(7)",
        "(-9223372036854775808..9223372036854775807).count",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; x.push(11); rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for call in [
        "[7,9].count(ignored:1) {true}",
        "[7,9].one?(ignored:1) {true}",
        "[7,9].take_while(ignored:1) {true}",
    ] {
        witness(&format!("def run; {call}; end"), true, false);
    }
}

#[test]
fn reductions_reject_bad_values_after_captures_and_before_later_effects() {
    for call in [
        "[7].reduce(0) {|sum:string,n| x.push(9)}",
        "[1,2].reduce {|sum:string,n| x.push(9)}",
        "[1,2].sum {|n| x.push(n); \"bad\"}",
        "[1,2].sum(\"\") {|n| x.push(n); n}",
        "[1].reduce(7,\"<<\")",
        "[1].reduce([],\"&\")",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; x.push(11); rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    witness(
        "def run; x=[]; begin; [0].reduce(7,\"/\"); x.push(11); rescue ZeroDivisionError; x; end; end",
        true,
        false,
    );
}

#[test]
fn reducer_and_predicate_control_transfers_keep_capture_writes() {
    for method in [
        "reduce(0)",
        "sum",
        "find",
        "index",
        "rindex",
        "count",
        "any?",
        "all?",
        "none?",
        "one?",
        "take_while",
    ] {
        for transfer in ["break 7", "break", "return 7", "next 7", "raise \"bad\""] {
            let source = format!(
                "def run; a=[]; begin; result=[7,9].{method} {{|v| a.push(v); {transfer}}}; [result,a]; rescue; a; ensure; a.push(11); end; end"
            );
            witness(&source, false, false);
        }
    }
    witness(
        "def outer; [1,2].reduce(0) {|a,n| yield n}; end; def run; outer {return 7}; missing; end",
        true,
        false,
    );
}

#[test]
fn generic_reducers_and_predicates_converge_with_captured_growth() {
    for method in [
        "find",
        "index",
        "rindex",
        "count",
        "any?",
        "all?",
        "none?",
        "one?",
        "take_while",
    ] {
        let source = format!(
            "def run(xs:array<int>, flag:bool); a=[]; result=xs.{method} {{|v| a.push(v); flag}}; [a,result]; end"
        );
        for values in [vec![], vec![7], vec![7, 9, 11]] {
            for flag in [false, true] {
                let input = Value::array(values.iter().copied().map(Value::int).collect());
                super::iteration_tests::inferred_runtime(
                    &source,
                    &[input, Value::boolean(flag)],
                    false,
                );
            }
        }
    }
    for body in [
        "xs.reduce(0) {|sum,n| sum+n}",
        "xs.reduce {|sum,n| sum+n}",
        "xs.sum {|n| n}",
        "xs.reduce([]) {|sum,n| [sum,n]}",
        "a=[]; result=xs.reduce(0) {|sum,n| a=[a]; [sum,n]}; [a,result]",
    ] {
        for values in [vec![], vec![7], vec![7, 9, 11]] {
            let input = Value::array(values.iter().copied().map(Value::int).collect());
            super::iteration_tests::inferred_runtime(
                &format!("def run(xs:array<int>); {body}; end"),
                &[input],
                false,
            );
        }
    }
}

#[test]
fn reducer_cleanup_replacement_and_rescued_error_context_stay_lexical() {
    for method in ["reduce(0)", "find", "sum"] {
        for body in ["7", "break 3", "return 4", "next 5", "raise \"body\""] {
            for cleanup in ["7", "break 9", "return 10", "next 11", "raise \"cleanup\""] {
                witness(
                    &format!(
                        "def run; a=[]; begin; result=[1,2].{method} {{a.push(1); begin; {body}; ensure; a.push(2); {cleanup}; end}}; [result,a]; rescue; a; end; end"
                    ),
                    false,
                    false,
                );
            }
        }
    }
    for class in [
        "RuntimeError",
        "ArgumentError",
        "TypeError",
        "StandardError",
        "AssertionError",
        "LocalJumpError",
        "ZeroDivisionError",
        "LimitError",
    ] {
        witness(
            &format!(
                "def run; x=[]; begin; begin; raise {class}, \"bad\"; rescue; [1,2].find {{x.push(7); raise}}; end; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
    }
    witness(
        "def run; a=[]; begin; [7].find {|n| a.push(n); if a.length<2; raise \"again\"; end; true}; rescue; retry; end; a; end",
        false,
        false,
    );
}

#[test]
fn predicate_effects_keep_snapshots_types_and_match_data_protection() {
    for source in [
        "def run; a=[7,9]; result=a.reduce([]) {|sum,n| a.clear; sum.push(n)}; [a,result]; end",
        "def run; a=[[7],[9]]; result=a.find {|v| v.push(11); true}; [a,result]; end",
        "def run; x=[]; result=[7,9].reduce(x) {|sum,n| sum.push(n)}; [x,result]; end",
        "def run; x=0; result=[7,9].find {|x| x=3; true}; [x,result]; end",
        "def run; m=/(a)/.match(\"a\"); if m; c=m.captures; [1,2].find {c.push(\"x\"); true}; [c,m.captures]; end; end",
    ] {
        witness(source, false, false);
    }
    for source in [
        "def run; x=0; [7,9].find {x=\"bad\"; true}; x; end",
        "def owner -> int; [7,9].sum {return \"bad\"}; end; def run; x=[]; begin; owner; missing; rescue; x; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; begin; [1].reduce(m) {|a,n| a.captures.push(\"bad\")}; rescue; m.captures; end; end; end",
    ] {
        witness(source, false, true);
    }
}

#[test]
fn reducer_argument_unions_preserve_successful_paths_and_typed_returns() {
    let source = "def run(fallback:nil|int); a=[]; begin; result=[7].find(fallback) {a.push(9); true}; [result,a]; rescue; a; end; end";
    for input in [Value::nil(), Value::int(1)] {
        super::iteration_tests::inferred_runtime(source, &[input], true);
    }
    let source = "def run(flag:bool); x=[]; op=if flag; \"+\"; else; 1; end; begin; result=[7].reduce(0,op) {missing}; [result,x]; rescue; x; end; end";
    for flag in [false, true] {
        super::iteration_tests::inferred_runtime(source, &[Value::boolean(flag)], true);
    }
    for input in [Value::int(7), Value::bytes("bad")] {
        super::iteration_tests::inferred_runtime(
            "def run(v:int|string); x=[]; begin; [1,2].reduce(0) {|sum:int,n| x.push(7); v}; rescue; x; end; end",
            &[input],
            true,
        );
    }
}

#[test]
fn range_predicates_and_deep_reducers_stay_bounded_on_the_default_stack() {
    for body in [
        "(-9223372036854775808..9223372036854775807).find {break 7}",
        "(-9223372036854775808..9223372036854775807).reduce {|a,n| break 7}",
        "(1..100).count {false}",
        "(9223372036854775807..9223372036854775807).reduce {|a,n| missing}",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
    let source = "def run; (-9223372036854775808..9223372036854775807).count {false}; end";
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.incomplete.data.is_empty() && result.issues.data.is_empty());
    assert_eq!(result.returns, facts.integer(&mut ctx, 0).unwrap());
    drop((result, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
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
    for depth in [2, 8, 24] {
        let body = format!("{}7{}", "[1].reduce(0) {".repeat(depth), "}".repeat(depth));
        witness(&format!("def run; {body}; end"), true, false);
    }
    witness(
        "def recurse(n:int); if n>0; [1].reduce(0) {recurse(n-1)}; else; 7; end; end; def run; recurse(7); end",
        false,
        false,
    );
}

#[test]
fn nested_construction_guards_preserve_captured_writes_at_the_error_site() {
    let mut input = Value::int(7);
    for _ in 0..128 {
        input = Value::array(vec![input]);
    }
    for source in [
        "def run(v:any); x=[]; begin; [1,2].reduce(v) {|a,n| x.push(7); [a]}; x.push(9); rescue LimitError; x; end; end",
        "def run(v:any); x=[]; begin; [1,2].reduce(v) {|a,n| x.push(7); {a:a}}; x.push(9); rescue LimitError; x; end; end",
        "def run(v:any); x=[]; begin; x.push(7); [v]; x.push(9); rescue LimitError; x; end; end",
        "def run(v:any); x=[]; begin; x.push(7); {a:v}; x.push(9); rescue LimitError; x; end; end",
    ] {
        super::iteration_tests::inferred_runtime(source, std::slice::from_ref(&input), false);
    }
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run(xs:array<int>,flag:bool); a=[]; begin; result=xs.reduce([]) {|acc,n| a.push(n); if flag; return a; else; acc.push(n); end}; found=xs.any? {|n| a.push(n); flag}; count=xs.count {|n| a.push(n); flag}; [result,found,count,a]; ensure; a.push(7); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn reducer_fixed_points_have_exact_quota_boundaries_and_release_allocations() {
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
fn reducer_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn remaining_dispatch_and_pending_address_effects_are_explicit() {
    for source in [
        "def run(op:string); [1,2].reduce(0,op); end",
        "def run; [1,2].reduce(0,\"custom\"); end",
        "def run; [1,2].fill {|n| n}; end",
        "def run; a=[1]; a.push([1].find {a.clear; true}); a; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((facts, report));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn reduction_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-reductions.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 43);
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
    assert_eq!(differences, 20);
}
