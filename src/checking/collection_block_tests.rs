use super::lexical_tests::witness;
use super::{collection_tests::analyze, facts::Facts};
use crate::{CallContext, CallOptions};

#[test]
fn array_block_results_preserve_order_indices_and_snapshots() {
    for body in [
        "[1,2,3].map {|x| x}",
        "[1,2,3].map_with_index {|x,i| [x,i]}",
        "a=[]; result=[7,9].each {|x| a.push(x)}; [a,result]",
        "a=[]; result=[7,9].each_with_index {|x,i| a.push([x,i])}; [a,result]",
        "a=[]; result=[7,9].reverse_each {|x| a.push(x)}; [a,result]",
        "[true,nil,false,7].select {|x| x}",
        "[true,nil,false,7].reject {|x| x}",
        "[true,nil,false,7].filter_map {|x| x}",
        "[1,2].flat_map {|x| [x,x]}",
        "[1,2].collect_concat {|x| x}",
        "a=[1,2]; result=a.each {|x| a.push(x)}; [a,result]",
        "a=[1,2]; copy=a; result=a.map {|x| a.clear; x}; [a,copy,result]",
        "[[1,2],[3,4]].map {|x,y| [y,x]}",
        "[7,9].map {_1}",
        "[1,2].map(7,ignored:9) {|x| x}",
        "[1,2].select(7,ignored:9) {true}",
        "[1,2].each(7,ignored:9) {true}",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains("for n"),
            false,
        );
    }
}

#[test]
fn hash_and_range_blocks_have_their_own_parameter_and_result_contracts() {
    for body in [
        "{a:7}.map {|x| x}",
        "{a:7}.map {|k,v| [k,v]}",
        "{a:7}.map_with_index {|pair,i| [pair,i]}",
        "a=[]; result={b:7,a:9}.each {|k,v| a.push([k,v])}; [a,result]",
        "a=[]; result={a:7}.each_key {|k| a.push(k)}; [a,result]",
        "a=[]; result={a:7}.each_value {|v| a.push(v)}; [a,result]",
        "{a:7}.select {|k| k==\"a\"}",
        "{a:7,b:nil}.reject {|k,v| v}",
        "(1..3).map {|x| x}",
        "(3..1).select {true}",
        "(1...1).map {missing}",
        "(9223372036854775807..9223372036854775807).map {|x| x}",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn native_callbacks_keep_writes_on_break_return_next_and_error() {
    for body in [
        "a=[]; result=[1,2].map {|x| a.push(x); next 7}; [a,result]",
        "a=[]; result=[1,2].map {|x| a.push(x); break 7}; [a,result]",
        "a=[]; result=[1,2].each {|x| a.push(x); break}; [a,result]",
        "a=[]; begin; [1,2].map {|x| a.push(x); return 7}; ensure; return a; end",
        "a=[]; begin; [1,2].map {|x| a.push(x); raise \"bad\"}; rescue; a; end",
        "a=[]; for n in [1,2]; result=[7,9].each {|x| a.push(x); break 3}; a.push(result); end; a",
        "a=[]; result=[7,9].each {|x| begin; a.push(x); break 3; ensure; a.push(11); end}; [a,result]",
        "a=[]; result=[7,9].each {|x| begin; a.push(x); return 3; ensure; next 11; end}; [a,result]",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains("for n"),
            false,
        );
    }
    witness(
        "def outer; [1,2].map {|x| yield x}; end; def run; a=[]; result=outer {|x| a.push(x); x}; [a,result]; end",
        true,
        false,
    );
    witness(
        "def outer; [1,2].map {|x| yield x}; end; def run; a=[]; begin; outer {|x| a.push(x); return 7}; ensure; return a; end; end",
        true,
        false,
    );
}

#[test]
fn generic_collection_callbacks_converge_with_growing_captured_values() {
    let source = "def run(xs:array<int>); a=[]; xs.each {a=[a]}; a; end";
    for length in [0, 1, 3] {
        super::iteration_tests::inferred_runtime(
            source,
            &[crate::Value::array(
                (0..length).map(crate::Value::int).collect(),
            )],
            false,
        );
    }
}

#[test]
fn collection_argument_errors_precede_callbacks_and_keep_their_error_class() {
    for call in [
        "[1].each",
        "[].map",
        "{a:1}.each_value",
        "(1..3).select",
        "[1].map_with_index(7) {x.push(9)}",
        "[1].reverse_each(7) {x.push(9)}",
        "[1].reject(7) {x.push(9)}",
        "[1].flat_map(7) {x.push(9)}",
        "[1].filter_map(bad:7) {x.push(9)}",
        "[1].each_with_index(bad:7) {x.push(9)}",
        "{a:1}.map(bad:7) {x.push(9)}",
        "{a:1}.each(7) {x.push(9)}",
        "(1..3).map(7) {x.push(9)}",
        "(1..3).each(bad:7) {x.push(9)}",
        "(..3).map {x.push(9)}",
        "(1..).map {x.push(9)}",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for call in [
        "7.each {x.push(9)}",
        "[1].each_key {x.push(9)}",
        "\"abc\".map {x.push(9)}",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for body in [
        "x=[]; result={a:7}.each(ignored:3) {|k,v| x.push([k,v])}; [x,result]",
        "x=[]; result=[1,2].reject(ignored:3) {false}; [x,result]",
        "a=[1]; result=a.map(a.push(2)) {|x| x}; [a,result]",
        "a=[1]; result=a.each(a.clear) {|x| a.push(x)}; [a,result]",
        "[].map {|x:string| missing}",
        "{}.select {missing}",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn collection_validation_and_exits_apply_block_and_owner_types() {
    for source in [
        "def run; x=[]; begin; [1,2].map {|n:string| x.push(n)}; rescue; x; end; end",
        "def owner -> int; [1,2].each {return \"bad\"}; end; def run; begin; owner; rescue; 7; end; end",
        "def run; x=0; [1,2].each {x=\"bad\"}; x; end",
        "def accept(x:int); x; end; def run; begin; [1,2].map {accept(\"bad\")}; rescue; 7; end; end",
    ] {
        witness(source, true, true);
    }
    witness(
        "def run -> int; [1,2].map {return 7}; \"unreachable\"; end",
        true,
        false,
    );
    witness(
        "def run; result=[1,2].map {break \"done\"}; result; end",
        true,
        false,
    );
}

#[test]
fn collection_cleanup_preserves_each_control_transfer_and_error_class() {
    for body in ["7", "next 7", "break 7", "return 7", "raise \"body\""] {
        for cleanup in ["9", "next 9", "break 9", "return 9", "raise \"cleanup\""] {
            witness(
                &format!(
                    "def run; x=[]; begin; result=[1,2].map {{|v| begin; x.push(v); {body}; ensure; x.push(3); {cleanup}; end}}; [x,result]; rescue; x; end; end"
                ),
                true,
                false,
            );
        }
    }
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
                "def run; x=[]; begin; [1,2].map {{|v| begin; x.push(v); raise {class}, \"bad\"; ensure; x.push(3); end}}; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def run; begin; begin; raise {class}, \"bad\"; rescue; [1,2].each {{raise}}; end; rescue {class}; 7; end; end"
            ),
            true,
            false,
        );
    }
    witness(
        "def run; x=[]; begin; [7].map {|v| x.push(v); if x.length<2; raise \"again\"; end}; rescue; retry; end; x; end",
        false,
        false,
    );
}

#[test]
fn collection_blocks_isolate_parameter_copies_and_protect_match_data() {
    for source in [
        "def run; a=[[1],[2]]; result=a.map {|v| v.push(7)}; [a,result]; end",
        "def run; x=0; result=[7,9].map {|x| x=3; x}; [x,result]; end",
        "def run; x=[]; copy=x; [1,2].each {|v| x.push(v)}; [x,copy]; end",
        "def run; x=[]; [1,2].each {|v| copy=x; copy.push(v)}; x; end",
        "def once; yield; end; def run; x=[]; [1,2].each {|v| once {once {x.push(v)}}}; x; end",
        "def run; a=[[1]]; result=a.map {|v| a[0].push(9); v}; [a,result]; end",
    ] {
        witness(source, true, false);
    }
    witness(
        "def run; m=/(a)/.match(\"a\"); if m; begin; [1,2].each {m.captures.push(7)}; rescue; m.captures; end; end; end",
        false,
        true,
    );
    witness(
        "def run; m=/(a)/.match(\"a\"); if m; c=m.captures; [1,2].each {c.push(\"x\")}; [c,m.captures]; end; end",
        false,
        false,
    );
    witness(
        "def run; m=/(a)/.match(\"a\"); if m; m.select {|k,v| true}; end; end",
        false,
        false,
    );
}

#[test]
fn generic_collection_types_cover_runtime_values_and_union_receivers() {
    use crate::Value;
    let arrays = [vec![], vec![1], vec![1, 2, 3]];
    for method in [
        "each",
        "each_with_index",
        "reverse_each",
        "map",
        "map_with_index",
        "flat_map",
        "filter_map",
        "select",
        "reject",
    ] {
        let source = format!(
            "def run(xs:array<int>); a=[]; result=xs.{method} {{|v,i| a.push(v); [v,i]}}; [a,result]; end"
        );
        for values in &arrays {
            let input = Value::array(values.iter().copied().map(Value::int).collect());
            super::iteration_tests::inferred_runtime(&source, &[input], false);
        }
    }
    for method in [
        "each",
        "each_with_index",
        "each_key",
        "each_value",
        "map",
        "map_with_index",
        "select",
        "reject",
    ] {
        let source = format!(
            "def run(keys:array<string>); h={{}}; for key in keys; h[key]=7; end; a=[]; result=h.{method} {{|k,v| a.push([k,v]); v}}; [a,result]; end"
        );
        for length in 0..=2 {
            let input = Value::array(
                (0..length)
                    .map(|n| Value::bytes(if n == 0 { b"b".to_vec() } else { b"a".to_vec() }))
                    .collect(),
            );
            super::iteration_tests::inferred_runtime(&source, &[input], false);
        }
    }
    for method in ["map", "each", "select", "reject"] {
        let source = format!(
            "def run(xs:array<int>|range); a=[]; result=xs.{method} {{|v| a.push(v); v}}; [a,result]; end"
        );
        let input = Value::array(vec![Value::int(7)]);
        super::iteration_tests::inferred_runtime(&source, &[input], false);
    }
}

#[test]
fn large_ranges_and_nested_native_callbacks_use_bounded_analysis_and_the_default_stack() {
    witness(
        "def run; a=[]; result=(-9223372036854775808..9223372036854775807).map {|v| a.push(v); break 7}; [a,result]; end",
        false,
        false,
    );
    for depth in [2, 8, 24] {
        let source = format!(
            "def run; x=[]; {}x.push(7);{}; x; end",
            "[1].each {".repeat(depth),
            "};".repeat(depth)
        );
        witness(&source, true, false);
    }
    witness(
        "def visit(xs,n:int); if n>0; xs.map {|v| visit([v],n-1)}; else; xs; end; end; def run -> array; visit([7],3); end",
        false,
        false,
    );
}

#[test]
fn collection_guards_remain_catchable_after_callback_writes() {
    use crate::Value;
    super::iteration_tests::inferred_runtime(
        "def run(r:range); begin; r.map {7}; rescue RuntimeError; \"missing bound\"; end; end",
        &[Value::range(None, Some(3), false)],
        false,
    );
    let mut deep = Value::nil();
    for _ in 0..128 {
        deep = Value::array(vec![deep]);
    }
    super::iteration_tests::inferred_runtime(
        "def run(v:any); x=[]; begin; [1].map {x.push(7); v}; rescue LimitError; x; end; end",
        &[deep],
        false,
    );
}

#[test]
fn mixed_type_boundaries_keep_successful_paths_and_reject_before_later_effects() {
    use crate::Value;
    for source in [
        "def run(v:int|string); x=[]; begin; result=[v].map {|n:int| x.push(n); n}; [result,x]; rescue; x; end; end",
        "def owner(v:int|string) -> int; [1].each {return v}; end; def run(v:int|string); begin; owner(v); rescue; 7; end; end",
        "def run(v:int|string); begin; [[v]].map {|xs:array<int>| xs}; rescue; 7; end; end",
        "def owner(v:int|string) -> array<int>; [1].each {return [v]}; end; def run(v:int|string); begin; owner(v); rescue; 7; end; end",
    ] {
        for value in [Value::int(9), Value::bytes(b"bad")] {
            super::iteration_tests::inferred_runtime(source, &[value], true);
        }
    }
    witness(
        "def owner -> int; [1].each {return \"bad\"}; end; def run; x=[]; begin; owner; x.push(9); rescue; x; end; end",
        true,
        true,
    );
    witness(
        "def run; x=[]; begin; [1].map {|n:string| missing; x.push(9)}; rescue; x; end; end",
        true,
        true,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run(xs:array<int>,flag:bool); a=[]; begin; result=xs.map {|n| a.push(n); if flag; return a; else; [n]; end}; [a,result]; ensure; a.push(7); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn native_collection_fixed_points_and_captures_obey_exact_quotas_and_cleanup() {
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
fn native_collection_analysis_observes_latched_cancellation_and_deadlines() {
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
fn unknown_bang_collection_methods_are_known_errors() {
    for source in [
        "def run; [1,2].map! {|n| n}; end",
        "def run; [1,2].map! {|n| true}; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(!report.issues.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn native_collection_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-collection-blocks.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 30);
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
