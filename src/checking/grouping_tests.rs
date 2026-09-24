use super::lexical_tests::witness;
use super::{collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime};
use crate::{CallContext, CallOptions, Value};

#[test]
fn partition_and_groups_keep_original_values_order_and_key_types() {
    for body in [
        "[7,false,nil,9].partition {|n| n}",
        "[7,9,7].group_by {:a}",
        "[\"b\",\"a\",\"b\"].group_by {|n| n}",
        "[:b,:a,\"b\"].group_by_stable {|n| n}",
        "[\"b\",:a,:b].group_by_stable {|n| n}",
        "[:b,:a,\"b\"].tally",
        "[7,9,7].tally {:a}",
        "[[\"a\",7],[:a,9]].to_h",
        "[7,9].to_h {|n| [:a,n]}",
        "a=[]; result=[7,9].partition {|n| a.push(n); true}; [a,result]",
        "a=[[7],[9]]; result=a.group_by {|n| n.push(11); :a}; [a,result]",
        "a=[7,9]; result=a.group_by_stable {|n| a.clear; :a}; [a,result]",
        "a=[7,9]; result=a.partition {|n| a[1]=3; true}; [a,result]",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains("=="),
            false,
        );
    }
}

#[test]
fn adjacent_callbacks_use_original_neighbor_pairs_and_flush_the_last_group() {
    for body in [
        "[7,9,11].slice_when {true}",
        "[7,9,11].slice_when {false}",
        "[7,9,11].chunk_while {true}",
        "[7,9,11].chunk_while {false}",
        "a=[]; result=[7,9,11].slice_when {|left,right| a.push([left,right]); right==9}; [a,result]",
        "a=[]; result=[7,9,11].chunk_while {|left,right| a.push([left,right]); right==9}; [a,result]",
        "a=[7,9,11]; result=a.slice_when {|left,right| a[2]=3; true}; [a,result]",
        "a=[[7],[9],[11]]; result=a.chunk_while {|left,right| left.push(3); right.clear; true}; [a,result]",
        "a=[]; result=[7,9].slice_when {a.push([_1,_2]); true}; [a,result]",
        "[7,9].chunk_while {|left,right,extra| extra}",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains("=="),
            false,
        );
    }
}

#[test]
fn empty_and_singleton_grouping_skips_unreachable_callbacks() {
    for body in [
        "[].partition {|n:string| missing}",
        "[].group_by {missing}",
        "[].group_by_stable {missing}",
        "[].tally {missing}",
        "[].tally",
        "[].to_h {missing}",
        "[].to_h",
        "[].drop_while {missing}",
        "[].slice_when {missing}",
        "[7].slice_when {|a:string,b| missing}",
        "[].chunk_while {missing}",
        "[7].chunk_while {|a:string,b| missing}",
        "{}.transform_keys {missing}",
        "{}.transform_values {missing}",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains("=="),
            false,
        );
    }
}

#[test]
fn dropping_stops_callbacks_after_the_first_false_value() {
    for body in [
        "[7,9].drop_while {true}",
        "[7,9].drop_while {false}",
        "a=[]; result=[7,9,11].drop_while {|n| a.push(n); n==7}; [a,result]",
        "a=[]; result=[7,\"unused\"].drop_while {|n:int| a.push(n); false}; [a,result]",
        "a=[]; result=[7,9].drop_while {|n| a.push(n); next false}; [a,result]",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains("=="),
            false,
        );
    }
}

#[test]
fn hash_transform_blocks_receive_only_the_selected_key_or_value() {
    for body in [
        "{a:7}.transform_keys {|k,extra| if extra; missing; end; k}",
        "{a:7}.transform_values {|v,extra| [v,extra]}",
        "{a:[7,9]}.transform_values {|left,right| [right,left]}",
        "{a:7}.transform_keys {:b}",
        "a={a:[7]}; result=a.transform_values {|v| v.push(9)}; [a,result]",
        "a={a:7,b:9}; result=a.transform_keys {|k| a.clear; k}; [a,result]",
        "{a:7,b:9}.transform_keys {:same}",
        "{group_by:7}.group_by",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn grouping_validation_precedes_callbacks_and_uses_runtime_errors() {
    for call in [
        "[].partition",
        "[].group_by",
        "[].group_by_stable",
        "[].drop_while",
        "[].slice_when",
        "[].chunk_while",
        "{}.transform_keys",
        "{}.transform_values",
        "[7].partition(1) {x.push(9)}",
        "[7].group_by(1) {x.push(9)}",
        "[7].group_by_stable(1) {x.push(9)}",
        "[7].tally(1) {x.push(9)}",
        "[7].to_h(1) {x.push(9)}",
        "[7].drop_while(1) {x.push(9)}",
        "[7].slice_when(1) {x.push(9)}",
        "[7].chunk_while(1) {x.push(9)}",
        "[7].to_h(bad:1) {x.push(9)}",
        "[].to_h(bad:1)",
        "[7].slice_when(bad:1) {x.push(9)}",
        "[7].chunk_while(bad:1) {x.push(9)}",
        "{a:7}.transform_keys(1) {x.push(9)}",
        "{a:7}.transform_values(1) {x.push(9)}",
        "[7].transform_keys {x.push(9)}",
        "(1..3).group_by {x.push(9)}",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; x.push(11); rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for call in [
        "[7].partition(ignored:1) {true}",
        "[7].group_by(ignored:1) {:a}",
        "[7].group_by_stable(ignored:1) {:a}",
        "[:a].tally(ignored:1)",
        "[7].drop_while(ignored:1) {true}",
        "{a:7}.transform_keys(ignored:1) {:a}",
        "{a:7}.transform_values(ignored:1) {9}",
    ] {
        witness(&format!("def run; {call}; end"), false, false);
    }
}

#[test]
fn invalid_callback_results_keep_prior_writes_and_stop_later_effects() {
    for call in [
        "[7,9].group_by {|n| x.push(n); n}",
        "[7,9].group_by_stable {|n| x.push(n); []}",
        "[7,9].tally {|n| x.push(n); false}",
        "[7,9].to_h {|n| x.push(n); n}",
        "[7,9].to_h {|n| x.push(n); [:a]}",
        "[7,9].to_h {|n| x.push(n); [7,n]}",
        "[7,9].to_h {|n| x.push(n); [:a,n,11]}",
        "[7,9].partition {|n:string| x.push(n)}",
        "[7,9].slice_when {|left:string,right| x.push(right)}",
        "{a:7}.transform_keys {|k| x.push(k); 7}",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; x.push(11); rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
}

#[test]
fn generic_grouping_converges_with_captures_key_unions_and_adjacent_state() {
    for method in ["partition", "drop_while", "slice_when", "chunk_while"] {
        let source = format!(
            "def run(xs:array<int>,flag:bool); a=[]; result=xs.{method} {{|left,right| a.push(left); flag}}; [a,result]; end"
        );
        for values in [vec![], vec![7], vec![7, 9, 11]] {
            for flag in [false, true] {
                let input = Value::array(values.iter().copied().map(Value::int).collect());
                inferred_runtime(&source, &[input, Value::boolean(flag)], false);
            }
        }
    }
    for method in ["group_by", "group_by_stable", "tally"] {
        let source = format!(
            "def run(xs:array<int>,flag:bool); a=[]; result=xs.{method} {{|n| a.push(n); if flag; :a; else; \"b\"; end}}; [a,result]; end"
        );
        for values in [vec![], vec![7], vec![7, 9, 11]] {
            for flag in [false, true] {
                let input = Value::array(values.iter().copied().map(Value::int).collect());
                inferred_runtime(&source, &[input, Value::boolean(flag)], false);
            }
        }
    }
    for body in [
        "xs.to_h {|n| [:a,n]}",
        "a=[]; xs.group_by {a=[a]; :a}; a",
        "a=[]; xs.slice_when {a=[a]; false}; a",
    ] {
        let source = format!("def run(xs:array<int>); {body}; end");
        inferred_runtime(
            &source,
            &[Value::array(vec![
                Value::int(7),
                Value::int(9),
                Value::int(11),
            ])],
            false,
        );
    }
    for body in [
        "xs.tally",
        "xs.group_by_stable {|n| n}",
        "xs.group_by {|n| n}",
        "h={}; for k in xs; h[k]=7; end; h.transform_values {|v| v}",
        "h={}; for k in xs; h[k]=7; end; h.transform_keys {|k| k}",
    ] {
        let source = format!("def run(xs:array<string>); {body}; end");
        inferred_runtime(
            &source,
            &[Value::array(vec![
                Value::bytes("b"),
                Value::bytes("a"),
                Value::bytes("b"),
            ])],
            false,
        );
    }
}

#[test]
fn grouping_control_transfers_keep_captured_writes_and_ensure_replacement() {
    for method in [
        "partition",
        "group_by",
        "group_by_stable",
        "tally",
        "to_h",
        "drop_while",
        "slice_when",
        "chunk_while",
    ] {
        for transfer in ["break 7", "break", "return 7", "raise \"bad\""] {
            witness(
                &format!(
                    "def run; a=[]; begin; result=[7,9].{method} {{|v| a.push(v); {transfer}}}; [result,a]; rescue; a; ensure; a.push(11); end; end"
                ),
                true,
                false,
            );
        }
    }
    for body in ["true", "next true", "break 7", "return 7", "raise \"body\""] {
        for cleanup in [
            "false",
            "next false",
            "break 9",
            "return 9",
            "raise \"cleanup\"",
        ] {
            witness(
                &format!(
                    "def run; x=[]; begin; result=[1,2,3].slice_when {{|v| begin; x.push(v); {body}; ensure; x.push(3); {cleanup}; end}}; [x,result]; rescue; x; end; end"
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
                "def run; x=[]; begin; [7,9].group_by {{|v| begin; x.push(v); raise {class}, \"bad\"; ensure; x.push(3); end}}; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def run; x=[]; begin; begin; raise {class}, \"bad\"; rescue; [7,9].chunk_while {{x.push(3); raise}}; end; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
    }
    witness(
        "def outer; [1,2].group_by {yield}; end; def run; outer {return 7}; missing; end",
        true,
        false,
    );
    witness(
        "def run; x=[]; begin; [7].group_by {x.push(7); if x.length<2; raise \"again\"; end; :a}; rescue; retry; end; x; end",
        false,
        false,
    );
}

#[test]
fn mixed_keys_and_pairs_preserve_valid_paths_and_stop_invalid_ones() {
    for method in ["group_by", "group_by_stable", "tally"] {
        let source = format!(
            "def run(key:string|symbol|int); x=[]; begin; result=[7,9].{method} {{|n| x.push(n); key}}; [result,x]; rescue RuntimeError; x; end; end"
        );
        for key in [Value::bytes("a"), Value::symbol("a"), Value::int(7)] {
            inferred_runtime(&source, &[key], true);
        }
    }
    let source = "def run(flag:bool); x=[]; begin; result=[7,9].to_h {|n| x.push(n); if flag; [:a,n]; else; [7]; end}; [result,x]; rescue RuntimeError; x; end; end";
    for flag in [false, true] {
        inferred_runtime(source, &[Value::boolean(flag)], true);
    }
    let source = "def run(pair:array<int|string>); x=[]; begin; result=[7,9].to_h {x.push(3); pair}; [result,x]; rescue RuntimeError; x; end; end";
    for pair in [
        vec![],
        vec![Value::int(7)],
        vec![Value::bytes("a"), Value::int(7)],
        vec![Value::int(7), Value::int(9)],
    ] {
        inferred_runtime(source, &[Value::array(pair)], true);
    }
    let source = "def run(v:any); x=[]; begin; result=[7].to_h {x.push(3); v}; [result,x]; rescue RuntimeError; x; end; end";
    for v in [
        Value::nil(),
        Value::array(vec![Value::bytes("a"), Value::int(7)]),
    ] {
        inferred_runtime(source, &[v], false);
    }
    let source = "def run(v:int|string); x=[]; begin; result=[7,9].group_by {|n:int| x.push(n); v}; [result,x]; rescue RuntimeError; x; end; end";
    for v in [Value::int(1), Value::bytes("a")] {
        inferred_runtime(source, &[v], true);
    }
}

#[test]
fn grouping_preserves_protected_data_and_copied_children() {
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; c=m.captures; result=[7,9].group_by {c.push(\"x\"); :a}; [result,c,m.captures]; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; c=m.captures; result=[7,9].slice_when {c.push(\"x\"); true}; [result,c,m.captures]; end; end",
    ] {
        witness(source, false, false);
    }
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; begin; [7].group_by {m.captures.push(\"x\"); :a}; rescue; m.captures; end; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; copy=m.dup; begin; [7,9].chunk_while {copy.captures.clear; true}; rescue; copy.captures; end; end; end",
        "def owner -> int; [7,9].group_by {return \"bad\"}; end; def run; begin; owner; rescue; 7; end; end",
        "def run; x=0; [7].group_by {x=\"bad\"; :a}; x; end",
    ] {
        witness(source, false, true);
    }
}

fn nested(depth: usize) -> Value {
    let mut value = Value::int(7);
    for _ in 0..depth {
        value = Value::array(vec![value]);
    }
    value
}

#[test]
fn grouping_depth_guards_follow_callback_writes_before_the_next_iteration() {
    for (method, depth) in [
        ("partition", crate::budget::MAX_VALUE_DEPTH - 1),
        ("group_by", crate::budget::MAX_VALUE_DEPTH - 1),
        ("group_by_stable", crate::budget::MAX_VALUE_DEPTH - 2),
    ] {
        let source = format!(
            "def run(xs:array<any>); x=[]; begin; xs.{method} {{x.push(7); :a}}; x.push(9); rescue LimitError; x; end; end"
        );
        let input = Value::array(vec![nested(depth), Value::int(9)]);
        let actual = inferred_runtime(&source, std::slice::from_ref(&input), false);
        assert_eq!(actual.to_string(), "[7]", "{source}");
        let source = format!(
            "def run(xs:array<any>); x=[]; result=xs.{method} {{x.push(7); break 9}}; [x,result]; end"
        );
        let actual = inferred_runtime(&source, &[input], false);
        assert_eq!(actual.to_string(), "[[7], 9]", "{source}");
    }
    let source = "def run(v:any); x=[]; begin; {a:7}.transform_values {x.push(7); v}; x.push(9); rescue LimitError; x; end; end";
    let actual = inferred_runtime(source, &[nested(crate::budget::MAX_VALUE_DEPTH)], false);
    assert_eq!(actual.to_string(), "[7]");
}

#[test]
fn adjacent_depth_guards_run_when_a_group_is_flushed() {
    for method in ["slice_when", "chunk_while"] {
        for (condition, expected) in [
            (method == "slice_when", "[7]"),
            (method != "slice_when", "[7, 7]"),
        ] {
            let source = format!(
                "def run(xs:array<any>); x=[]; begin; xs.{method} {{x.push(7); {condition}}}; x.push(9); rescue LimitError; x; end; end"
            );
            let input = Value::array(vec![
                nested(crate::budget::MAX_VALUE_DEPTH - 1),
                Value::int(9),
                Value::int(11),
            ]);
            let actual = inferred_runtime(&source, &[input], false);
            assert_eq!(actual.to_string(), expected, "{source}");
        }
        let source = format!(
            "def run(xs:array<any>); x=[]; begin; xs.{method} {{x.push(7); true}}; x.push(9); rescue LimitError; x; end; end"
        );
        let actual = inferred_runtime(
            &source,
            &[Value::array(vec![nested(
                crate::budget::MAX_VALUE_DEPTH - 1,
            )])],
            false,
        );
        assert_eq!(actual.to_string(), "[]", "{source}");
        let source = format!(
            "def run(xs:array<any>); x=[]; result=xs.{method} {{x.push(7); break 9}}; [x,result]; end"
        );
        let actual = inferred_runtime(
            &source,
            &[Value::array(vec![
                nested(crate::budget::MAX_VALUE_DEPTH - 1),
                Value::int(9),
            ])],
            false,
        );
        assert_eq!(actual.to_string(), "[[7], 9]", "{source}");
    }
}

#[test]
fn nested_grouping_and_recursive_adjacent_callbacks_use_the_default_stack() {
    for depth in [2, 8, 24] {
        let body = format!(
            "{}:a{}",
            "[7].group_by {".repeat(depth),
            "}; :a".repeat(depth)
        );
        witness(&format!("def run; {body}; end"), true, false);
        let body = format!(
            "{}true{}",
            "[7,9].slice_when {".repeat(depth),
            "}; true".repeat(depth)
        );
        witness(&format!("def run; {body}; end"), true, false);
    }
    witness(
        "def recurse(n:int); if n>0; [7,9].slice_when {recurse(n-1); true}; else; []; end; end; def run; recurse(7); end",
        false,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run(xs:array<int>,flag:bool); a=[]; begin; groups=xs.group_by_stable {|n| a.push(n); :a}; pairs=xs.chunk_while {flag}; kept=xs.drop_while {flag}; [groups,pairs,kept,a]; ensure; a.push(7); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn grouping_fixed_points_have_exact_quotas_and_release_every_allocation() {
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
fn grouping_analysis_keeps_real_cancellation_and_deadlines_latched() {
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
fn unmodeled_dispatch_callbacks_and_pending_capture_addresses_remain_explicit() {
    {
        let source = "def run; [7,9].is_type?(\"x\".upcase); end";
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((facts, report));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn grouping_reference_decisions_have_independent_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-grouping.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 49);
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
    assert_eq!(differences, 26);
}
