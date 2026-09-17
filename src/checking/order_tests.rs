use super::{
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts},
    iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Value};

#[test]
fn native_loop_preserves_zero_argument_yields_and_all_lexical_transfers() {
    for body in [
        "loop {break 7}",
        "loop {break}",
        "loop {return 7}; missing",
        "loop {|(*args)| break args}",
        "loop {|a,b| break [a,b]}",
        "x=[]; result=loop {x.push(7); break 9}; [x,result]",
        "x=[]; result=loop {begin; break 7; ensure; x.push(9); end}; [x,result]",
        "loop {begin; break 7; ensure; return 9; end}; missing",
        "loop {begin; return 7; ensure; break 9; end}",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
    witness(
        "def helper; yield; end; def run; loop {helper {return 7}}; missing; end",
        true,
        false,
    );
    witness(
        "def forward; loop {yield}; end; def run; forward {break 7}; end",
        true,
        false,
    );
    witness("def loop; 7; end; def run; loop; end", true, false);
    witness(
        "def run; x=[]; loop {x.push(7); if x.length>2; break; end; next}; x; end",
        false,
        false,
    );
}

#[test]
fn native_loop_validation_precedes_callbacks_and_infinite_loops_have_no_tail() {
    for call in [
        "loop",
        "loop()",
        "loop(7) {x.push(9)}",
        "loop(bad:7) {x.push(9)}",
        "loop {|n:int| x.push(9)}",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for body in [
        "loop {}",
        "loop {next}",
        "x=[]; loop {x=[x]}",
        "loop {begin; break 7; ensure; next; end}",
    ] {
        let source = format!("def run; {body}; missing; end");
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &source).unwrap();
        assert!(
            report.issues.data.is_empty() && report.incomplete.data.is_empty(),
            "{source}: {report:?}"
        );
        assert_eq!(report.returns, Atom::Never.fact());
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn native_sort_and_key_extrema_preserve_known_order_and_stable_ties() {
    super::native_tests::witness(
        "def run -> int; Time.at(0.125,in:\"UTC\").min; end",
        Some("0"),
        false,
    );
    for body in [
        "[9,7,11,7].sort",
        "[].sort",
        "[nil].sort",
        "[false,true,false].sort",
        "[nil,nil].sort",
        "[\"z\",\"a\",\"b\"].sort",
        "[:z,:a,:b].sort",
        "[[2,7],[1,8],[2,9]].sort_by {|row| row[0]}",
        "[[2,7],[1,8],[2,9]].min_by {|row| row[0]}",
        "[[2,7],[1,8],[2,9]].max_by {|row| row[0]}",
        "[9,7,11,7].min",
        "[9,7,11,7].max",
        "[9,7,11,7].minmax",
        "[].min",
        "[].max",
        "[].minmax",
        "[].sort_by {missing}",
        "[].min_by {missing}",
        "[].max_by {missing}",
        "[7].sort_by {{a:1}}",
        "[7].min_by {{a:1}}",
        "[7].max_by {{a:1}}",
        "[7,9].min_by {nil}",
        "[7,9].max_by {nil}",
        "[7,9].sort_by {nil}",
        "[[2],[1,9],[1,7],[]].sort",
        "[[2],[1,9],[1,7],[]].minmax",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
    for body in [
        "[1.0,1,0.0,-0.0].sort",
        "[1,1.0].minmax",
        "[[1],[1.0]].sort_by {|row| row}",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn ordering_callbacks_observe_source_snapshots_and_the_runtime_schedule() {
    for body in [
        "a=[]; result=[9,7,11].sort {|l,r| a.push([l,r]); 0}; [a,result]",
        "a=[]; result=[9,7,11].sort {|l,r| a.push([l,r]); -1}; [a,result]",
        "a=[]; result=[9,7,11].sort {|l,r| a.push([l,r]); 1}; [a,result]",
        "a=[]; result=[9,7,11].sort_by {|n| a.push(n); n}; [a,result]",
        "a=[9,7,11]; seen=[]; result=a.sort_by {|n| seen.push(n); a.clear; n}; [a,seen,result]",
        "a=[9,7,11]; seen=[]; result=a.min_by {|n| seen.push(n); a.clear; n}; [a,seen,result]",
        "a=[9,7,11]; seen=[]; result=a.sort {|l,r| seen.push([l,r]); a.clear; 0}; [a,seen,result]",
        "[].sort {missing}",
        "[7].sort {|l:string,r:string| missing}",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
    witness(
        "def run; seen=[]; result=[9,7,11].sort {|l,r| seen.push([l,r]); l<=>r}; [seen,result]; end",
        false,
        false,
    );
}

#[test]
fn ordering_validation_and_noncomparable_keys_fail_at_the_right_effect_boundary() {
    for call in [
        "[].sort(1) {x.push(9)}",
        "[].sort_by",
        "[].min_by",
        "[].max_by",
        "[].min {x.push(9)}",
        "[].max {x.push(9)}",
        "[].minmax {x.push(9)}",
        "[7,9].sort {x.push(3); nil}",
        "[7,9].sort {|l:string,r| x.push(3); 0}",
        "[nil,7].sort",
        "[{},{}].min",
        "[nil,7].max",
        "[nil,7].minmax",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    for (method, expected) in [
        ("sort_by", "[7, 9, 11]"),
        ("min_by", "[7, 9]"),
        ("max_by", "[7, 9]"),
    ] {
        super::native_tests::witness(
            &format!(
                "def run; x=[]; begin; [7,9,11].{method} {{|n| x.push(n); {{a:7}}}}; rescue RuntimeError; x; end; end"
            ),
            Some(expected),
            true,
        );
    }
    for call in [
        "[7,9].sort(ignored:3)",
        "[7,9].sort_by(ignored:3) {|n| n}",
        "[7,9].min(ignored:3)",
        "[7,9].max(ignored:3)",
        "[7,9].minmax(ignored:3)",
        "[7,9].min_by(ignored:3) {|n| n}",
        "[7,9].max_by(ignored:3) {|n| n}",
    ] {
        witness(&format!("def run; {call}; end"), true, false);
    }
}

#[test]
fn ordering_callbacks_preserve_break_return_and_cleanup() {
    for call in [
        "[9,7,11].sort",
        "[9,7,11].sort_by",
        "[9,7,11].min_by",
        "[9,7,11].max_by",
    ] {
        for body in [
            "x.push(3); break 7",
            "x.push(3); break",
            "begin; break 7; ensure; x.push(9); end",
        ] {
            witness(
                &format!("def run; x=[]; result={call} {{{body}}}; [x,result]; end"),
                true,
                false,
            );
        }
        witness(
            &format!("def run; {call} {{return 7}}; missing; end"),
            true,
            false,
        );
        witness(
            &format!(
                "def helper; yield; end; def run; {call} {{helper {{return 7}}}}; missing; end"
            ),
            true,
            false,
        );
    }
}

#[test]
fn generic_ordering_and_comparator_branches_retain_valid_runtime_results() {
    for body in [
        "xs.sort",
        "xs.min",
        "xs.max",
        "xs.minmax",
        "xs.sort_by {|n| n}",
        "xs.min_by {|n| n}",
        "xs.max_by {|n| n}",
        "a=[]; result=xs.sort {|l,r| a.push(l); l<=>r}; [a,result]",
    ] {
        for values in [
            vec![],
            vec![Value::int(7)],
            vec![Value::int(9), Value::int(7), Value::int(11)],
        ] {
            inferred_runtime(
                &format!("def run(xs:array<int>); {body}; end"),
                &[Value::array(values)],
                false,
            );
        }
    }
    for direction in [Value::int(-1), Value::int(0), Value::int(1)] {
        inferred_runtime(
            "def run(direction:int); a=[]; result=[9,7,11].sort {|l,r| a.push([l,r]); direction}; [a,result]; end",
            &[direction],
            false,
        );
    }
}

#[test]
fn ordering_and_loop_callbacks_keep_ordinary_errors_retry_and_cleanup_replacement() {
    for call in [
        "loop",
        "[9,7,11].sort",
        "[9,7,11].sort_by",
        "[9,7,11].min_by",
        "[9,7,11].max_by",
    ] {
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
                    "def run; x=[]; begin; {call} {{begin; x.push(3); raise {class}, \"bad\"; ensure; x.push(9); end}}; rescue {class}; x; end; end"
                ),
                true,
                false,
            );
            witness(
                &format!(
                    "def run; x=[]; begin; begin; raise {class}, \"bad\"; rescue; {call} {{x.push(3); raise}}; end; rescue {class}; x; end; end"
                ),
                true,
                false,
            );
        }
        for body in ["next 0", "break 7", "return 7", "raise \"body\""] {
            for cleanup in ["break 9", "return 9", "raise \"cleanup\""] {
                witness(
                    &format!(
                        "def run; x=[]; begin; result={call} {{begin; x.push(3); {body}; ensure; x.push(9); {cleanup}; end}}; [x,result]; rescue; x; end; end"
                    ),
                    true,
                    false,
                );
            }
        }
    }
    witness(
        "def run; x=[]; begin; loop {x.push(7); if x.length<2; raise \"again\"; end; break}; rescue; retry; end; x; end",
        false,
        false,
    );
    witness(
        "def run; x=[]; begin; [9,7].sort_by {|n| x.push(n); if x.length<2; raise \"again\"; end; n}; rescue; retry; end; x; end",
        false,
        false,
    );
}

#[test]
fn mixed_comparator_results_and_key_kinds_keep_successes_beside_errors() {
    for value in [
        Value::int(-1),
        Value::float(0.0),
        Value::nil(),
        Value::bytes("bad"),
    ] {
        inferred_runtime(
            "def run(value:int|float|nil|string); x=[]; begin; result=[9,7].sort {|l,r| x.push([l,r]); value}; [x,result]; rescue RuntimeError; x; end; end",
            &[value],
            true,
        );
    }
    for values in [
        vec![Value::int(7), Value::int(9)],
        vec![Value::bytes("b"), Value::bytes("a")],
        vec![Value::int(7), Value::bytes("a")],
    ] {
        for body in [
            "xs.sort",
            "xs.min",
            "xs.max",
            "xs.minmax",
            "xs.sort_by {|n| n}",
            "xs.min_by {|n| n}",
        ] {
            inferred_runtime(
                &format!(
                    "def run(xs:array<int|string>); begin; {body}; rescue RuntimeError; 3; end; end"
                ),
                &[Value::array(values.clone())],
                true,
            );
        }
    }
    for value in [
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
        Value::float(f64::NEG_INFINITY),
    ] {
        inferred_runtime(
            "def run(value:float); [9,7].sort {value}; end",
            std::slice::from_ref(&value),
            false,
        );
    }
    for (method, expected) in [
        ("sort_by", "[9, 7, 11]"),
        ("min_by", "[9, 7]"),
        ("max_by", "[9, 7]"),
    ] {
        let source = format!(
            "def run(value:float); x=[]; begin; [9,7,11].{method} {{|n| x.push(n); value}}; rescue RuntimeError; x; end; end"
        );
        assert_eq!(
            inferred_runtime(&source, &[Value::float(f64::NAN)], false).to_string(),
            expected
        );
    }
}

#[test]
fn stable_ordering_crosses_the_runtime_merge_boundary() {
    let values = (0..24)
        .map(|n| format!("[{},{}]", (23 - n) % 4, n))
        .collect::<Vec<_>>()
        .join(",");
    witness(
        &format!("def run; [{values}].sort_by {{|row| row[0]}}; end"),
        true,
        false,
    );
    let values = (0..21)
        .rev()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    witness(
        &format!(
            "def run; x=[]; result=[{values}].sort {{|l,r| x.push([l,r]); 0}}; [x,result]; end"
        ),
        true,
        false,
    );
}

#[test]
fn ordering_fact_comparisons_cover_runtime_values_without_assuming_array_identity() {
    use super::ordering::{EQUAL, GREATER, LESS, UNORDERED};
    let scalars = vec![
        Value::nil(),
        Value::boolean(false),
        Value::boolean(true),
        Value::int(0),
        Value::int(1),
        Value::int(9007199254740993),
        Value::float(0.0),
        Value::float(-0.0),
        Value::float(1.0),
        Value::float(9007199254740992.0),
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
        Value::bytes("a"),
        Value::bytes("b"),
        Value::symbol("a"),
        Value::symbol("b"),
        Value::hash(vec![]),
    ];
    let mut values = scalars.clone();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut inputs: Vec<_> = scalars
        .iter()
        .map(|v| match v.0 {
            crate::value::Kind::Float(n) => facts.float(&mut ctx, n).unwrap(),
            _ => literal_fact(&mut ctx, &mut facts, v),
        })
        .collect();
    for (index, value) in scalars.iter().enumerate() {
        let key = inputs[index];
        inputs.push(facts.tuple(&mut ctx, &[key]).unwrap());
        values.push(Value::array(vec![value.clone()]));
    }
    values.push(Value::array(vec![]));
    inputs.push(facts.tuple(&mut ctx, &[]).unwrap());
    for (a, left) in values.iter().enumerate() {
        for (b, right) in values.iter().enumerate() {
            let expected = match (&left.0, &right.0) {
                (crate::value::Kind::Bool(a), crate::value::Kind::Bool(b)) => Some(a.cmp(b) as i64),
                _ => crate::ordering::spaceship(&mut ctx, left, right)
                    .unwrap()
                    .as_int(),
            };
            let bit = match expected {
                Some(-1) => LESS,
                Some(0) => EQUAL,
                Some(1) => GREATER,
                None => UNORDERED,
                _ => unreachable!(),
            };
            let result = facts.order_result(&mut ctx, inputs[a], inputs[b]).unwrap();
            assert_ne!(result & bit, 0, "{left:?} / {right:?}: {result}");
            if left.as_array().is_none() && right.as_array().is_none() {
                assert_eq!(result, bit, "{left:?} / {right:?}");
            }
        }
    }
    let open = facts.shape(&mut ctx, &[], true).unwrap();
    let closed = facts.shape(&mut ctx, &[], false).unwrap();
    let a = facts.tuple(&mut ctx, &[open]).unwrap();
    let b = facts.tuple(&mut ctx, &[closed]).unwrap();
    assert_eq!(
        facts.order_result(&mut ctx, a, b).unwrap(),
        EQUAL | UNORDERED
    );
    let nan = facts.float(&mut ctx, f64::NAN).unwrap();
    let array = facts.tuple(&mut ctx, &[nan]).unwrap();
    assert_eq!(
        facts.order_result(&mut ctx, array, array).unwrap(),
        EQUAL | UNORDERED
    );
    let int = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let hash = facts.shape(&mut ctx, &[], false).unwrap();
    let a = facts.tuple(&mut ctx, &[int, Atom::Nil.fact()]).unwrap();
    let b = facts.tuple(&mut ctx, &[two, hash]).unwrap();
    assert_eq!(facts.order_result(&mut ctx, a, b).unwrap(), LESS);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn ordering_comparisons_walk_deep_shared_facts_on_the_default_stack() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut a = facts.integer(&mut ctx, 1).unwrap();
    let mut b = facts.float(&mut ctx, 1.0).unwrap();
    for _ in 0..4000 {
        a = facts.tuple(&mut ctx, &[a, a]).unwrap();
        b = facts.tuple(&mut ctx, &[b, b]).unwrap();
    }
    assert_eq!(
        facts.order_result(&mut ctx, a, b).unwrap(),
        super::ordering::EQUAL
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn ordering_and_loop_preserve_protected_values_and_unwrapped_deep_keys() {
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; [m,m.dup].sort {0}; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; [m].min_by {m}; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; c=m.captures; [7,9].sort_by {c.push(\"x\"); 0}; [c,m.captures]; end; end",
    ] {
        witness(source, false, false);
    }
    for call in ["loop", "[9,7].sort", "[9,7].sort_by", "[9,7].min_by"] {
        witness(
            &format!(
                "def run; m=/(a)/.match(\"a\"); if m; begin; {call} {{m.captures.push(\"x\")}}; rescue; m.captures; end; end; end"
            ),
            false,
            true,
        );
    }
    let mut value = Value::int(7);
    for _ in 0..128 {
        value = Value::array(vec![value]);
    }
    for (call, expected) in [
        ("[9,7].sort_by", "[3, 3, 9]"),
        ("[9,7].min_by", "[3, 3, 9]"),
        ("[9,7].max_by", "[3, 3, 9]"),
    ] {
        let source = format!("def run(v:any); x=[]; {call} {{x.push(3); v}}; x.push(9); x; end");
        assert_eq!(
            inferred_runtime(&source, std::slice::from_ref(&value), false).to_string(),
            expected
        );
    }
    let source = "def run(v:any); x=[]; result=loop {x.push(3); break v}; x.push(9); x; end";
    assert_eq!(
        inferred_runtime(source, &[value], false).to_string(),
        "[3, 9]"
    );
}

#[test]
fn ordering_and_loop_nesting_keeps_lexical_owners_and_default_stack_limits() {
    let mut body = "x.push(7)".to_owned();
    for i in 0..24 {
        body = if i % 2 == 0 {
            format!("loop {{{body}; break 3}}")
        } else {
            format!("[7].sort_by {{{body}; 0}}")
        };
    }
    witness(&format!("def run; x=[]; {body}; x; end"), true, false);
    witness(
        "def recurse(n:int); if n>0; [7,9].min_by {recurse(n-1); 0}; else; 3; end; end; def run; recurse(7); end",
        false,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run(direction:int,flag:bool); x=[]; loop {x.push(7); if flag; break; end}; result=[9,7,11].sort {|l,r| x.push(l); direction}; [x,result]; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn ordering_schedules_and_loop_fixed_points_have_exact_quotas_and_failure_cleanup() {
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
fn ordering_analysis_keeps_cancellation_and_deadlines_latched() {
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
fn ordering_reference_decisions_have_independent_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-ordering.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 68);
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
    assert_eq!(differences, 30);
}
