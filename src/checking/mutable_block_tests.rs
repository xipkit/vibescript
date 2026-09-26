use super::{
    collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Value};

#[test]
fn array_filters_stage_writes_and_preserve_no_change_results() {
    for (body, expected) in [
        (
            "a=[1,2]; seen=[]; result=a.delete_if {seen.push(a); true}; [a,seen,result]",
            "[[], [[1, 2], [1, 2]], []]",
        ),
        (
            "a=[1,2]; result=a.delete_if {a.push(3); false}; [a,result]",
            "[[1, 2, 3, 3], [1, 2]]",
        ),
        (
            "a=[1,2]; result=a.keep_if {a.push(3); true}; [a,result]",
            "[[1, 2, 3, 3], [1, 2]]",
        ),
        (
            "a=[1,2]; result=a.delete_if {a.push(3); true}; [a,result]",
            "[[], []]",
        ),
        (
            "a=[1,2]; result=a.keep_if {a=[7]; false}; [a,result]",
            "[[7], []]",
        ),
        (
            "a=[1,2]; b=a; result=a.fill {a.push(3); 7}; [a,b,result]",
            "[[7, 7], [1, 2], [7, 7]]",
        ),
        (
            "a=[1,2]; result=a.fill {a=[9]; 7}; [a,result]",
            "[[9], [7, 7]]",
        ),
    ] {
        let source = format!("def run; {body}; end");
        super::native_tests::witness(&source, Some(expected), false);
        witness(&source, true, false);
    }
}

#[test]
fn array_filter_truthiness_and_generic_receivers_cover_execution() {
    for method in ["delete_if", "keep_if"] {
        for result in ["nil", "false", "0", "true", "[]", "\"\""] {
            witness(
                &format!(
                    "def run; a=[1,2,3]; b=a; result=a.{method} {{{result}}}; [a,b,result]; end"
                ),
                true,
                false,
            );
        }
        for values in [
            vec![],
            vec![Value::int(1)],
            vec![Value::int(1), Value::int(2)],
        ] {
            for flag in [false, true] {
                let source = format!(
                    "def run(a:array<int>,flag:bool); b=a; result=a.{method} {{a.push(7); flag}}; [a,b,result]; end"
                );
                inferred_runtime(
                    &source,
                    &[Value::array(values.clone()), Value::boolean(flag)],
                    false,
                );
            }
        }
    }
}

#[test]
fn hash_filters_delete_keys_from_the_latest_receiver() {
    for body in [
        "a={a:1,b:2}; b=a; result=a.delete_if {|k,v| a.store(:c,3); k==\"a\"}; [a,b,result]",
        "a={a:1,b:2}; result=a.keep_if {|k,v| a.b=9; a.c=3; true}; [a,result]",
        "a={a:1,b:2}; result=a.delete_if {|k,v| a.b=9; a.c=3; true}; [a,result]",
        "a={row:{a:1,b:2,c:3}}; b=a; result=a.row.delete_if {|k,v| a.row.b=9; a.row.d=4; a.row.delete(:c); k==\"a\"}; [a,b,result]",
        "a={a:1,b:2}; result=a.keep_if {|k,v| a={c:3}; false}; [a,result]",
        "a={a:1,b:2}; result=a.delete_if {|k,v| a.clear; a.store(k,7); true}; [a,result]",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
    for source in [
        "def run; {}.delete_if {missing}; end",
        "def run; {}.keep_if {missing}; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn delete_fallbacks_distinguish_present_nil_from_absence() {
    for body in [
        "a=[1,nil,1]; result=a.delete(1) {missing}; [a,result]",
        "a=[nil]; result=a.delete(nil) {missing}; [a,result]",
        "a=[]; result=a.delete(7) {|key| a.push(key); 9}; [a,result]",
        "a=[1]; result=a.delete(7) {|key| a=[2]; key}; [a,result]",
        "a={a:nil,b:2}; result=a.delete(:a) {missing}; [a,result]",
        "a={a:1}; result=a.delete(\"a\") {missing}; [a,result]",
        "a={}; result=a.delete(:a) {|key| a.store(key,7); 9}; [a,result]",
        "a={}; result=a.delete(\"a\") {|key| a.store(key,7); 9}; [a,result]",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
    for source in [
        "def run(v:int); a=[nil,1,v]; result=a.delete(v) {7}; [a,result]; end",
        "def run(v:int); a=[1]; result=a.delete(v) {7}; [a,result]; end",
    ] {
        for n in [0, 1, 2] {
            inferred_runtime(source, &[Value::int(n)], false);
        }
    }
}

#[test]
fn fill_windows_keep_unselected_values_and_yield_absolute_indexes() {
    for args in [
        "",
        "(nil)",
        "(nil,nil)",
        "(1)",
        "(-1)",
        "(-99)",
        "(1,1)",
        "(1,-1)",
        "(5)",
        "(5,0)",
        "(5,1)",
        "(0,0)",
        "(1.9,1.9)",
        "(-1.9,0.9)",
        "(0..1)",
        "(0...1)",
        "(-2..-1)",
        "(..1)",
        "(1..)",
        "(5..3)",
    ] {
        witness(
            &format!(
                "def run; a=[1,2,3]; b=a; seen=[]; result=a.fill{args} {{|i| seen.push([i,a]); i}}; [a,b,seen,result]; end"
            ),
            true,
            false,
        );
    }
    for args in ["", "(nil)", "(0,0)", "(2,0)", "(0..-1)"] {
        witness(
            &format!("def run; a=[]; result=a.fill{args} {{missing}}; [a,result]; end"),
            true,
            false,
        );
    }
}

#[test]
fn fill_generic_bounds_and_large_windows_use_bounded_analysis() {
    for body in [
        "a.fill {7}",
        "a.fill(1,2) {|i| i}",
        "a.fill(start,count) {|i| i}",
        "a.fill(start..count) {|i| i}",
    ] {
        let source = format!(
            "def run(a:array<int>,start:int,count:int); begin; result=begin; {body}; end; [a,result]; rescue; a; end; end"
        );
        for start in [-4, 0, 1, 5] {
            for count in [-1, 0, 1, 3] {
                inferred_runtime(
                    &source,
                    &[
                        Value::array(vec![Value::int(1), Value::int(2)]),
                        Value::int(start),
                        Value::int(count),
                    ],
                    false,
                );
            }
        }
    }
    for body in [
        "a.fill(0,1000000000) {|i| 7}",
        "a.fill(1000000000,0) {missing}",
        "a.fill(1000000000..1000000001) {|i| 7}",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let source = format!("def run; a=[1]; {body}; a; end");
        let report = analyze(&mut ctx, &mut facts, &source).unwrap();
        assert!(
            report.incomplete.data.is_empty() && report.issues.data.is_empty(),
            "{source}: {report:?}"
        );
        drop((facts, report));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn invalid_arguments_and_missing_blocks_fail_before_callbacks() {
    for call in [
        "[].delete_if",
        "{}.keep_if",
        "[1].delete_if(3) {missing}",
        "{}.keep_if(3) {missing}",
        "[1].fill(0,1,2) {missing}",
        "[1].fill(0..1,2) {missing}",
        "[1].fill(-99..1) {missing}",
        "[1].fill(\"bad\") {missing}",
        "[1].fill(0,\"bad\") {missing}",
        "[1].fill(0..9223372036854775807) {missing}",
        "[1].delete {missing}",
        "{}.delete(3) {missing}",
        "[1].delete(1,2) {missing}",
        "[].delete_if(bad:3) {missing}",
        "[1].fill(bad:3) {missing}",
        "{}.delete(:a,bad:3) {missing}",
    ] {
        let source =
            format!("def run; begin; {call}; rescue RuntimeError | LimitError; 7; end; end");
        witness(&source, true, true);
    }
}

#[test]
fn native_mutator_blocks_follow_each_method_contract() {
    for call in [
        "a.push(3)",
        "a.prepend(3)",
        "a.pop",
        "a.shift",
        "a.insert(0,3)",
    ] {
        witness(
            &format!("def run; a=[1,2]; result={call} {{missing}}; [a,result]; end"),
            true,
            false,
        );
    }
    for call in ["a.store(:b,3)", "a.replace({b:3})"] {
        witness(
            &format!("def run; a={{a:1}}; result={call} {{missing}}; [a,result]; end"),
            true,
            false,
        );
    }
    for receiver in ["[1,2]", "{a:1}"] {
        witness(
            &format!("def run; a={receiver}; begin; a.clear {{missing}}; rescue; a; end; end"),
            true,
            true,
        );
    }
}

#[test]
fn nonlocal_exits_abandon_staged_mutations_but_keep_callback_writes() {
    for call in ["a.fill", "a.delete_if", "a.keep_if", "a.delete(9)"] {
        for transfer in [
            "break 7",
            "next 7",
            "return 7",
            "raise \"bad\"",
            "begin; break 7; ensure; a.push(4); end",
            "begin; return 7; ensure; a.push(4); next 9; end",
        ] {
            let source = format!(
                "def run; a=[1,2]; begin; result={call} {{a.push(3); {transfer}}}; [a,result]; rescue; a; ensure; a.push(5); end; end"
            );
            witness(&source, false, false);
        }
    }
    for call in ["a.delete_if", "a.keep_if", "a.delete(:z)"] {
        witness(
            &format!(
                "def run; a={{a:1,b:2}}; result={call} {{a.store(:c,3); break 7}}; [a,result]; end"
            ),
            false,
            false,
        );
    }
}

#[test]
fn nested_mutations_refresh_pending_parent_and_original_negative_indexes() {
    for call in [
        "a.fill {a.push(2); 7}",
        "a.delete_if {a.push(2); true}",
        "a.keep_if {a.push(2); false}",
        "a.delete(9) {a.push(2); 7}",
    ] {
        for outer in ["a.push({call})", "a[-1]={call}", "a[-1]+={call}.length"] {
            let body = outer.replace("{call}", call);
            if body.contains(".length") && call.contains("delete(9)") {
                continue;
            }
            let rejected = outer.starts_with("a[-1]")
                && (call.contains("delete_if") || call.contains("keep_if"));
            witness(
                &format!(
                    "def run; a=[1]; result=begin; {body}; rescue RuntimeError; 7; end; [a,result]; end"
                ),
                false,
                rejected,
            );
        }
    }
    for call in [
        "a[0].fill {a.push([2]); 7}",
        "a[0].keep_if {a[0]=[9]; false}",
        "a[0].delete(9) {a[0]=[9]; 7}",
    ] {
        witness(
            &format!("def run; a=[[1]]; b=a; result={call}; [a,b,result]; end"),
            false,
            false,
        );
    }
}

#[test]
fn protected_match_values_reject_mutation_before_callback_dispatch() {
    for call in [
        "m.delete(:match)",
        "m.delete_if",
        "m.keep_if",
        "m.captures.fill",
        "m.captures.delete_if",
        "m.captures.keep_if",
    ] {
        witness(
            &format!(
                "def run; m=/(a)/.match(\"a\"); if m; begin; {call} {{missing}}; rescue; 7; end; end; end"
            ),
            false,
            true,
        );
    }
    witness(
        "def run; m=/(a)/.match(\"a\"); if m; copy=m.captures; copy.fill {\"x\"}; [copy,m.captures]; end; end",
        false,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(flag:bool,n:int); a=[[1],[2]]; a[-1].push(a[0].fill {a.push([3]); 7}); a.keep_if {flag}; h={a:1,b:2}; h.delete_if {|k,v| h.c=3; flag}; a.fill(n,2) {h.delete(:z) {7}}; [a,h]; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn mutating_callbacks_have_exact_quotas_and_failure_cleanup() {
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
    for memory in (0..stats.peak_memory_bytes).step_by((stats.peak_memory_bytes / 64).max(1)) {
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
    for steps in (0..stats.steps).step_by((stats.steps as usize / 64).max(1)) {
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
fn mutating_callback_analysis_keeps_cancellation_and_deadlines_latched() {
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
fn fill_depth_guards_run_after_writes_and_before_the_next_callback() {
    let mut value = Value::int(7);
    for _ in 0..crate::budget::MAX_VALUE_DEPTH {
        value = Value::array(vec![value]);
    }
    let result = inferred_runtime(
        "def run(v:any); a=[1,2]; x=[]; begin; a.fill {x.push(7); v}; rescue LimitError; [a,x]; end; end",
        std::slice::from_ref(&value),
        false,
    );
    assert_eq!(result.to_string(), "[[1, 2], [7]]");
    let result = inferred_runtime(
        "def run(v:any); a=[]; x=[]; a.delete(7) {x.push(3); v}; x.push(9); x; end",
        std::slice::from_ref(&value),
        false,
    );
    assert_eq!(result.to_string(), "[3, 9]");
    let result = inferred_runtime(
        "def run(v:any); a=[1,2]; a.keep_if {v}; a; end",
        &[value],
        false,
    );
    assert_eq!(result.to_string(), "[1, 2]");
    witness(
        "def run; a=[1,2]; x=[]; begin; a.fill {|i:string| x.push(7); 9}; rescue RuntimeError; [a,x]; end; end",
        true,
        true,
    );
}

#[test]
fn mutating_callbacks_preserve_error_classes_retry_and_lexical_returns() {
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
        for call in ["a.fill", "a.keep_if", "a.delete(9)"] {
            witness(
                &format!(
                    "def run; a=[1,2]; x=[]; begin; {call} {{x.push(7); raise {class}, \"bad\"}}; rescue {class}; [a,x]; end; end"
                ),
                true,
                false,
            );
        }
    }
    for call in ["a.fill", "a.delete_if", "a.delete(9)"] {
        witness(
            &format!(
                "def once; yield; end; def run; a=[1,2]; x=[]; done=false; begin; result={call} {{x.push(7); unless done; done=true; raise \"again\"; end; once {{next 3}}}}; [a,x,result]; rescue; retry; end; end"
            ),
            false,
            false,
        );
        witness(
            &format!(
                "def once; yield; end; def run; a=[1]; begin; {call} {{once {{a.push(3); return 7}}}}; ensure; return a; end; end"
            ),
            true,
            false,
        );
    }
}

#[test]
fn mutating_callbacks_cover_nested_parent_replacement_routes() {
    let engine = crate::Engine::legacy_unchecked();
    let mut count = 0;
    for selected in ["0", "1", "-1"] {
        for method in ["fill", "delete_if", "keep_if", "delete(9)"] {
            for change in [
                "a.push([3])",
                "a.prepend([3])",
                "a.pop",
                "a.shift",
                "a.clear",
                "a[0]=[3]",
                "a[1]=[3]",
                "a[0].push(3)",
                "a[1].push(3)",
                "a=[[1],[2]]",
                "a=a",
                "a=b",
            ] {
                let source = format!(
                    "def run; a=[[1],[2]]; b=a; result=a[{selected}].{method} {{{change}; 7}}; [a,b,result]; end"
                );
                super::address_tests::runtime_fact(&engine, &source, &[]);
                count += 1;
            }
        }
    }
    assert_eq!(count, 144);
}

#[test]
fn mutating_callback_nesting_and_complete_fills_keep_structural_contracts() {
    let mut body = "a.push(3)".to_owned();
    for _ in 0..24 {
        body = format!("[1].fill {{{body}; 7}}");
    }
    witness(
        &format!("def run; a=[1]; result=a.fill {{{body}; 7}}; [a,result]; end"),
        true,
        false,
    );
    for start in [0, 1] {
        let source =
            format!("def run -> array<int>; a=[1,2]; a.fill({start},1000000000) {{7}}; a; end");
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &source).unwrap();
        assert!(
            report.incomplete.data.is_empty() && report.issues.data.is_empty(),
            "{source}: {report:?}"
        );
        drop((facts, report));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn generic_fill_windows_preserve_element_contracts_and_suppress_empty_callbacks() {
    for args in [
        "(0,1)",
        "(5)",
        "(-5)",
        "(-1,1)",
        "(0..0)",
        "(-2..-1)",
        "(0...0)",
        "(-1...-1)",
    ] {
        let body = if matches!(args, "(0...0)" | "(-1...-1)") {
            "missing"
        } else {
            "7"
        };
        let source = format!(
            "def run(a:array<int>) -> array<int>; begin; a.fill{args} {{{body}}}; a; rescue RuntimeError; []; end; end"
        );
        for values in [
            vec![],
            vec![Value::int(1)],
            vec![Value::int(1), Value::int(2)],
        ] {
            inferred_runtime(&source, &[Value::array(values)], false);
        }
    }
    for args in ["(5...5)", "(5..3)", "(-1..-2)", "(3,0)"] {
        let source = format!(
            "def run(a:array<int>); begin; a.fill{args} {{missing}}; a; rescue RuntimeError; []; end; end"
        );
        for values in [vec![], vec![Value::int(1), Value::int(2)]] {
            inferred_runtime(&source, &[Value::array(values)], false);
        }
    }
}

#[test]
fn generic_fill_range_callback_counts_cover_absolute_and_relative_endpoints() {
    let mut count = 0;
    for start in [-4, -2, -1, 0, 1, 3, 5] {
        for end in [-4, -2, -1, 0, 1, 3, 5] {
            for dots in ["..", "..."] {
                let source = format!(
                    "def run(a:array<int>); x=[]; begin; a.fill({start}{dots}{end}) {{|i| x.push(i); 7}}; rescue RuntimeError; nil; end; x; end"
                );
                for values in [
                    vec![],
                    vec![Value::int(1)],
                    vec![Value::int(1), Value::int(2), Value::int(3)],
                ] {
                    inferred_runtime(&source, &[Value::array(values)], false);
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 294);
    for args in ["(1..)", "(-1..)", "(..1)", "(..-1)", "(0..-1)"] {
        let source = format!(
            "def run(a:array<int>); x=[]; begin; a.fill{args} {{|i| x.push(i); 7}}; rescue RuntimeError; nil; end; x; end"
        );
        for values in [
            vec![],
            vec![Value::int(1)],
            vec![Value::int(1), Value::int(2)],
        ] {
            inferred_runtime(&source, &[Value::array(values)], false);
        }
    }
}
