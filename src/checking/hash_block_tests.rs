use super::{
    collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Value};

#[test]
fn merge_skips_blocks_without_conflicts_and_distinguishes_stored_nil() {
    for (body, expected) in [
        ("{}.merge {missing}", "{}"),
        ("{a:1}.merge {missing}", "{a: 1}"),
        ("{}.merge({a:1}) {missing}", "{a: 1}"),
        ("{a:1}.merge({b:2}) {missing}", "{a: 1, b: 2}"),
        (
            "seen=[]; h={a:nil}; r=h.merge({a:7}) {|k,o,n| seen.push([k,o,n]); n}; [h,r,seen]",
            "[{a: nil}, {a: 7}, [[a, nil, 7]]]",
        ),
        ("{a:1}.merge({a:2}) {|(*args)| args}", "{a: [a]}"),
        ("{a:1}.merge({a:2}) {it}", "{a: a}"),
    ] {
        let source = format!("def run; {body}; end");
        super::native_tests::witness(&source, Some(expected), false);
        witness(&source, true, false);
    }
}

#[test]
fn merge_folds_sources_and_copies_receiver_and_later_arguments() {
    for body in [
        "seen=[]; r={a:1}.merge({a:2},{a:3}) {|k,o,n| seen.push([k,o,n]); n}; [r,seen]",
        "h={a:[1]}; later={a:[3]}; r=h.merge({a:[2]},later) {|k,o,n| h.a.push(7); later.a.push(9); n}; [h,later,r]",
        "h={a:1}; r=h.merge({a:2}) {|k,o,n| h={x:3}; n}; [h,r]",
        "h={a:1}; copy=h; r=h.merge({a:2}) {|k,o,n| h.a=9; n}; [h,copy,r]",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
    for body in [
        "{b:1,a:2}.merge({a:3,b:4,c:5},{b:6}) {|k,o,n| n}",
        "h={a:1,b:2}; seen=[]; r=h.merge({a:3,b:4}) {|k,o,n| seen.push([k,o,n]); n}; [h,r,seen]",
        "h={a:1,b:2}; r=h.merge({c:3,d:4}) {missing}; [h,r]",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn merge_validates_every_input_before_entering_the_block() {
    for call in [
        "{a:1}.merge({a:2},9) {seen.push(3); missing}",
        "{a:1}.merge({a:2},nil) {missing}",
        "{a:1}.merge({a:2},[]) {missing}",
        "{a:1}.merge({a:2},bad:9) {missing}",
        "{a:1}::merge({a:2}) {missing}",
        "[1].merge({a:2}) {missing}",
    ] {
        let source =
            format!("def run; seen=[]; begin; {call}; rescue RuntimeError; seen; end; end");
        super::native_tests::witness(&source, Some("[]"), true);
        witness(&source, true, true);
    }
}

#[test]
fn deep_keys_visit_parents_before_children_and_each_array_occurrence() {
    for (body, expected) in [
        (
            "seen=[]; h={a:{b:[{c:1},{d:2}]}}; r=h.deep_transform_keys {|k| seen.push(k); k}; [h,r,seen]",
            "[{a: {b: [{c: 1}, {d: 2}]}}, {a: {b: [{c: 1}, {d: 2}]}}, [a, b, c, d]]",
        ),
        (
            "seen=[]; child={x:1}; r={root:[child,child]}.deep_transform_keys {|k| seen.push(k); :z}; [r,seen]",
            "[{z: [{z: 1}, {z: 1}]}, [root, x, x]]",
        ),
        ("{}.deep_transform_keys {missing}", "{}"),
        (
            "{a:[1,nil,[],{}]}.deep_transform_keys {|k| :x}",
            "{x: [1, nil, [], {}]}",
        ),
        ("{}.deep_transform_keys(ignored:1) {missing}", "{}"),
    ] {
        let source = format!("def run; {body}; end");
        super::native_tests::witness(&source, Some(expected), false);
        witness(&source, true, false);
    }
    witness(
        "def run; seen=[]; r={a:{b:1},c:{d:2}}.deep_transform_keys {|k| seen.push(k); :x}; [r,seen]; end",
        false,
        false,
    );
}

#[test]
fn deep_keys_validate_results_before_entering_children() {
    for result in ["nil", "7", "[]", "{}", "false"] {
        let source = format!(
            "def run; seen=[]; begin; {{a:{{b:1}}}}.deep_transform_keys {{|k| seen.push(k); {result}}}; rescue RuntimeError; seen; end; end"
        );
        super::native_tests::witness(&source, Some("[a]"), true);
        witness(&source, true, true);
    }
    for call in [
        "{}.deep_transform_keys",
        "{}.deep_transform_keys(1) {missing}",
        "[].deep_transform_keys {missing}",
    ] {
        witness(
            &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
            true,
            true,
        );
    }
    witness(
        "def run; h={a:{b:1}}; seen=[]; begin; h.deep_transform_keys {|k:int| seen.push(k); :x}; rescue RuntimeError; [h,seen]; end; end",
        true,
        true,
    );
}

#[test]
fn hash_callbacks_preserve_break_next_return_and_cleanup() {
    for call in ["{a:1}.merge({a:2})", "{a:{b:1}}.deep_transform_keys"] {
        for body in [
            "seen.push(7); break 9",
            "seen.push(7); return 9",
            "begin; break 9; ensure; seen.push(7); end",
            "begin; return 9; ensure; seen.push(7); end",
            "begin; raise \"bad\"; ensure; break 9; end",
        ] {
            witness(
                &format!("def run; seen=[]; result={call} {{{body}}}; [result,seen]; end"),
                true,
                false,
            );
        }
    }
    witness("def run; {a:1}.merge({a:2}) {next 7}; end", true, false);
    witness(
        "def run; {a:{b:1}}.deep_transform_keys do next :x end; end",
        true,
        false,
    );
}

#[test]
fn hash_callbacks_preserve_pending_addresses_and_captured_writes() {
    for expression in [
        "{a:1}.merge({a:2}) {|k,o,n| a.push(3); 7}",
        "{a:{b:1}}.deep_transform_keys {|k| a.push(3); :x}",
    ] {
        for before in ["a=[1,2]", "a=[[1],[2]]"] {
            for operation in ["a[-1] += begin", "a[-1] = begin"] {
                let source =
                    format!("def run; {before}; {operation}; {expression}; 7; end; a; end");
                // Array += an integer is deliberately excluded from this scalar-address probe.
                if before.contains("[[") && operation.contains("+=") {
                    continue;
                }
                witness(&source, !operation.contains("+="), false);
            }
        }
    }
    witness(
        "def run; a={row:{x:1}}; a.row.x += begin; {k:1}.merge({k:2}) {a.row={x:9}; 3}; 7; end; a; end",
        false,
        false,
    );
}

#[test]
fn deep_transforms_snapshot_input_and_rebuild_mutable_hashes() {
    for body in [
        "h={a:{b:1}}; copy=h; r=h.deep_transform_keys {|k| h.a.b=9; k}; [h,copy,r]",
        "h={a:[{b:1}]}; r=h.deep_transform_keys {|k| h.a.push({c:2}); k}; [h,r]",
        "m=\"a\".match(\"a\"); if m; r={x:m}.deep_transform_keys {|k| k}; r.x.captures.push(\"ok\"); r.x.captures; end",
        "m=\"a\".match(\"a\"); if m; r=m.merge({extra:7}); r.captures.push(\"ok\"); r.captures; end",
    ] {
        witness(&format!("def run; {body}; end"), false, false);
    }
}

#[test]
fn generic_hash_inputs_and_unknown_children_cover_runtime_results() {
    for values in [
        vec![],
        vec![Value::int(1)],
        vec![Value::int(1), Value::int(2)],
    ] {
        for source in [
            "def run(xs:array<int>); h=xs.map {|v| [\"a\",v]}.to_h; h.merge({a:7}) {|k,o,n| n}; end",
            "def run(xs:array<int>); h=xs.map {|v| [\"a\",v]}.to_h; h.merge(h) {|k,o,n| n}; end",
            "def run(xs:array<int>); {root:xs.map {|v| {key:v}}}.deep_transform_keys {|k| k}; end",
        ] {
            inferred_runtime(source, &[Value::array(values.clone())], false);
        }
    }
    for value in [
        Value::int(1),
        Value::array(vec![Value::hash(vec![(b"b".to_vec(), Value::int(2))])]),
        Value::hash(vec![(b"b".to_vec(), Value::int(2))]),
    ] {
        inferred_runtime(
            "def run(value:any); seen=[]; r={a:value}.deep_transform_keys {|k| seen.push(k); k}; [r,seen]; end",
            &[value],
            false,
        );
    }
}

#[test]
fn passive_merges_preserve_required_fields_and_skip_disjoint_callbacks() {
    for body in [
        "{a:1,b:2}.merge({b:3,c:4})",
        "{a:1,b:2}.merge({c:3,d:4}) {missing}",
        "{}.merge({a:1,b:2},{c:3,d:4}) {missing}",
        "{a:1,b:2}.merge({},{}).merge({c:3}) {missing}",
    ] {
        witness(&format!("def run; {body}; end"), true, false);
    }
}

#[test]
fn hash_callbacks_preserve_every_ordinary_error_class_and_retry() {
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
        for call in ["{a:1}.merge({a:2})", "{a:{b:1}}.deep_transform_keys"] {
            witness(
                &format!(
                    "def run; x=[]; begin; {call} {{x.push(7); raise {class}, \"bad\"}}; rescue {class}; x; end; end"
                ),
                true,
                false,
            );
            witness(
                &format!(
                    "def run; x=[]; begin; {call} {{begin; raise {class}, \"bad\"; ensure; x.push(7); break 9; end}}; ensure; x.push(3); end; x; end"
                ),
                true,
                false,
            );
        }
    }
    for call in ["{a:1}.merge({a:2})", "{a:{b:1}}.deep_transform_keys"] {
        witness(
            &format!(
                "def run; first=true; x=[]; begin; {call} {{if first; first=false; raise \"again\"; end; x.push(7); :x}}; rescue; retry; end; x; end"
            ),
            false,
            false,
        );
    }
    witness(
        "def outer; {a:1}.merge({a:2}) {yield}; end; def run; outer {return 7}; missing; end",
        true,
        false,
    );
    witness(
        "def outer; {a:{b:1}}.deep_transform_keys {yield}; end; def run; outer {return 7}; missing; end",
        true,
        false,
    );
}

#[test]
fn conflict_result_depth_guards_follow_writes_and_precede_later_sources() {
    let mut value = Value::int(7);
    for _ in 0..crate::budget::MAX_VALUE_DEPTH {
        value = Value::array(vec![value]);
    }
    let result = inferred_runtime(
        "def run(v:any); x=[]; h={a:1}; begin; h.merge({a:2},{a:3}) {x.push(7); v}; rescue LimitError; [h,x]; end; end",
        &[value],
        false,
    );
    assert_eq!(result.to_string(), "[{a: 1}, [7]]");
}

#[test]
fn deep_frames_preserve_results_and_analysis_limits_without_native_recursion() {
    use super::{arguments, calls, facts::Atom};
    for depth in [128, crate::budget::MAX_VALUE_DEPTH] {
        let source = "def run(input); input.deep_transform_keys {|k| k}; end";
        let program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut input = facts.integer(&mut ctx, 7).unwrap();
        let mut value = Value::int(7);
        for _ in 0..depth {
            input = facts
                .shape(&mut ctx, &[(b"key", input, false)], false)
                .unwrap();
            value = Value::hash(vec![(b"key".to_vec(), value)]);
        }
        let inputs = [arguments::Input::Supplied(input)];
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            calls::World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program: &program,
                contracts: &[],
                hosts: &[],
                globals: &[],
            },
            program.names["run"],
            &inputs,
        );
        match &report {
            Ok(report) => {
                assert!(
                    report.incomplete.data.is_empty() && report.issues.data.is_empty(),
                    "{report:?}"
                );
                assert_eq!(report.returns, input);
                assert_ne!(report.returns, Atom::Never.fact());
            }
            Err(error) => {
                // Analysis retains more state per level than execution. At the full
                // depth it may exhaust the normal budget, but never the native stack.
                assert_eq!(depth, crate::budget::MAX_VALUE_DEPTH);
                assert!(
                    matches!(
                        error.kind,
                        crate::ErrorKind::Memory | crate::ErrorKind::Steps
                    ),
                    "{error}"
                );
                assert_eq!(ctx.checkpoint().unwrap_err().kind, error.kind);
            }
        }
        let actual = crate::Engine::legacy_unchecked()
            .compile(source)
            .unwrap()
            .call("run", &[value], CallOptions::default())
            .unwrap();
        let mut cursor = &actual.value;
        for _ in 0..depth {
            let entries = cursor.as_hash().unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0.as_bytes(), Some(&b"key"[..]));
            cursor = &entries[0].1;
        }
        assert_eq!(cursor.as_int(), Some(7));
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn nested_hash_callbacks_keep_the_default_stack() {
    let mut body = "x.push(7); 9".to_string();
    for _ in 0..24 {
        body = format!("{{a:1}}.merge({{a:2}}) {{{body}}}");
    }
    witness(
        &format!("def run; x=[]; r=begin; {body}; end; [r,x]; end"),
        true,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(flag:bool); x=[]; h={a:{b:1},c:[{d:2}]}; r=h.deep_transform_keys {|k| x.push(k); if flag; k; else; :x; end}; s={a:1,b:2}.merge({a:3,b:4},{a:5}) {|k,o,n| x.push(k); n}; [r,s,x]; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn hash_callbacks_have_exact_quotas_and_failure_cleanup() {
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
fn hash_callback_analysis_keeps_cancellation_and_deadlines_latched() {
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
fn hash_callbacks_cover_nested_parent_mutation_and_control_matrix() {
    let mut cases = 0;
    for call in ["{a:1}.merge({a:2})", "{a:{b:1}}.deep_transform_keys"] {
        for change in [
            "a.push([3])",
            "a.prepend([3])",
            "a.clear",
            "a.push([9],[8])",
            "a=[[9]]",
            "a[0]=[7]",
            "a.pop",
            "a.shift",
            "a.insert(0,[7])",
            "a[-1].clear",
            "a[-1].push(3)",
            "a[-1]=[9]",
        ] {
            for exit in [":x", "break 7", "return 9"] {
                witness(
                    &format!(
                        "def run; a=[[1],[2]]; r=a[-1].push(begin; {call} {{{change}; {exit}}}; end); [a,r]; end"
                    ),
                    false,
                    false,
                );
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 72);
}

#[test]
fn unknown_hash_arguments_and_children_include_symbol_keys() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut hash = crate::hash::Hash::empty();
    hash.insert(&mut ctx, Value::symbol("a"), Value::int(7))
        .unwrap();
    let input = Value::from_hash(&mut ctx, hash).unwrap();
    for source in [
        "def run(input:any); seen=[]; r={a:1}.merge(input) {|key,old,new| seen.push(key); new}; [r,seen]; end",
        "def run(input:any); seen=[]; r={root:input}.deep_transform_keys {|key| seen.push(key); key}; [r,seen]; end",
    ] {
        inferred_runtime(source, std::slice::from_ref(&input), false);
    }
    drop(input);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn structural_hash_arguments_are_validated_without_dispatching_on_them() {
    for input in [
        Value::hash(vec![]),
        Value::hash(vec![
            (b"a".to_vec(), Value::int(2)),
            (b"b".to_vec(), Value::int(3)),
        ]),
    ] {
        for source in [
            "def run(other:hash<string,int>); {a:1}.merge(other) {|k,o,n| n}; end",
            "def run(other:hash<string,int>); {a:1}.merge(other); end",
            "def run(other:hash<string,int>); {outer:other}.deep_transform_keys {|k| k}; end",
        ] {
            inferred_runtime(source, std::slice::from_ref(&input), false);
        }
    }
}
