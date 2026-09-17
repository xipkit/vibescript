use super::{
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts, Node},
    iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Value};

#[test]
fn pattern_selection_runs_only_matching_callbacks_and_maps_their_results() {
    for body in [
        "[7,9,7].grep(7)",
        "[7,9,7].grep_v(7)",
        "[7,9,11].grep(7..9)",
        "[7,9,11].grep_v(9..7)",
        "[\"ab\",\"a\",7].grep(/^a$/)",
        "[\"ab\",\"a\",7].grep_v(/^a$/)",
        "[1,1.0,2].grep(1)",
        "[0.0,-0.0,7].grep(0)",
        "[false,nil,7].grep(false)",
        "[7,9].grep(3) {missing}",
        "[7,7].grep_v(7) {missing}",
        "[].grep(7) {missing}",
        "[].grep_v(7) {missing}",
        "a=[]; result=[7,9,7].grep(7) {|n| a.push(n); 3}; [a,result]",
        "a=[]; result=[7,9,7].grep_v(7) {|n| a.push(n); 3}; [a,result]",
        "[7,9].grep(7,ignored:3) {|n| n}",
        "[7,9].grep_v(7,ignored:3)",
        "a=[7,9,7]; seen=[]; result=a.grep(7) {|n| a.clear; seen.push(n); 3}; [a,seen,result]",
        "[7,9].grep(begin; 7; end) {|n:int| n}",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains(".0"),
            false,
        );
    }
}

#[test]
fn uniqueness_retains_first_elements_and_invokes_every_key_callback() {
    for body in [
        "[7,9,7].uniq",
        "[7,9,7].uniq {|n| n}",
        "[7,9,7].uniq {3}",
        "[1,1.0,1,1.0].uniq {|n| n}",
        "[[1],[1.0],[2]].uniq {|n| [n]}",
        "[{a:1},{a:1.0},{b:1}].uniq {|n| [n]}",
        "[0.0,-0.0,0.0].uniq {|n| n}",
        "[:a,\"a\",:a,\"a\"].uniq {|n| n}",
        "[].uniq {missing}",
        "a=[]; result=[7,7,7].uniq {|n| a.push(n); 3}; [a,result]",
        "a=[]; result=[7,7,7].uniq {|n| a.push(n); a}; [a,result]",
        "a=[7,9,7]; copy=a; result=a.uniq {|n| a.clear; n}; [a,copy,result]",
        "[7,9].uniq {nil}",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains(".0"),
            false,
        );
    }
}

#[test]
fn lookup_hits_skip_blocks_and_missing_keys_preserve_values_and_order() {
    for body in [
        "[7,nil].fetch(0) {missing}",
        "a=[7]; result=a.fetch(begin; a.clear; 0; end) {missing}; [a,result]",
        "[7,nil].fetch(1) {missing}",
        "[7,nil].fetch(-1) {missing}",
        "[7,9].fetch(0.0) {missing}",
        "[7,9].fetch(-1.0) {missing}",
        "[7].fetch(9.0) {|key:int| key}",
        "[7].fetch(-9.0) {|key:int| key}",
        "[7].fetch(9,3) {|key| key}",
        "[7].fetch(9,3)",
        "[7].fetch(0,3)",
        "{a:nil}.fetch(:a) {missing}",
        "{a:7}.fetch(\"a\") {missing}",
        "{a:7}.fetch(:b) {|key| key}",
        "{a:7}.fetch(\"b\") {|key| key}",
        "{a:7}.fetch(:b,nil)",
        "{}.fetch_values {missing}",
        "{a:7,b:nil}.fetch_values(:a,:b,:a) {missing}",
        "{a:7,b:nil}.fetch_values(:a,:b,:a)",
        "{a:7}.fetch_values(:b,:a,\"b\") {|key| key}",
        "a=[]; result=[7].fetch(0,begin; a.push(9); 3; end) {missing}; [a,result]",
        "a=[]; result=[7].fetch(9,begin; a.push(9); 3; end) {|key| a.push(key); 11}; [a,result]",
        "a={a:7}; seen=[]; result=a.fetch_values(:b,:a,:b) {|key| a[:a]=9; seen.push(key); 3}; [a,seen,result]",
        "a={a:7}; result=a.fetch_values(:b,:a) {|key| a[:b]=9; 3}; [a,result]",
        "{}.fetch(:a,ignored:7) {|key| key}",
        "{}.fetch_values(:a,ignored:7) {|key| key}",
    ] {
        witness(
            &format!("def run; {body}; end"),
            !body.contains(".0"),
            false,
        );
    }
}

#[test]
fn invalid_selection_calls_fail_before_callbacks_and_keep_previous_effects() {
    for call in [
        "[].grep {x.push(9)}",
        "[].grep_v(7,9) {x.push(9)}",
        "[7].uniq(3) {x.push(9)}",
        "[7].uniq(ignored:3) {x.push(9)}",
        "[7].fetch {x.push(9)}",
        "[7].fetch(0,1,2) {x.push(9)}",
        "[7].fetch(0.5) {x.push(9)}",
        "[7].fetch(9223372036854775808.0) {x.push(9)}",
        "[7].fetch(\"bad\") {x.push(9)}",
        "{a:7}.fetch(0) {x.push(9)}",
        "{a:7}.fetch_values(0) {x.push(9)}",
        "[7].fetch(9)",
        "{}.fetch(:a)",
        "{}.fetch_values(:a)",
    ] {
        witness(
            &format!("def run; x=[]; begin; {call}; rescue RuntimeError; x; end; end"),
            true,
            true,
        );
    }
    witness(
        "def run; x=[]; begin; {}.fetch_values(:a,0,:b) {|key| x.push(key); 7}; rescue RuntimeError; x; end; end",
        true,
        true,
    );
    witness(
        "def run; x=[]; begin; {a:7}.fetch_values(:a,:b,0) {|key| x.push(key); 9}; rescue; x; end; end",
        true,
        true,
    );
    for call in [
        "[7].grep(9)",
        "[7].grep_v(7)",
        "[7].fetch(0)",
        "{a:7}.fetch(:a)",
        "{a:7}.fetch_values(:a)",
    ] {
        witness(
            &format!("def run; {call} {{|n:string| missing}}; end"),
            true,
            false,
        );
    }
}

#[test]
fn selection_callbacks_keep_control_transfers_cleanup_and_lexical_returns() {
    for call in [
        "[7,7].grep(7)",
        "[7,7].grep_v(9)",
        "[7,7].uniq",
        "[7].fetch(9)",
        "{}.fetch_values(:a,:b)",
    ] {
        for body in [
            "x.push(3); break 9",
            "x.push(3); break",
            "x.push(3); next 9",
            "begin; break 9; ensure; x.push(3); end",
            "begin; break 9; ensure; next 7; end",
        ] {
            witness(
                &format!("def run; x=[]; result={call} {{{body}}}; [x,result]; end"),
                true,
                false,
            );
        }
        witness(
            &format!(
                "def run; x=[]; {call} {{begin; x.push(3); return x; ensure; x.push(9); end}}; missing; end"
            ),
            true,
            false,
        );
        witness(
            &format!(
                "def helper; yield; end; def run; x=[]; {call} {{helper {{x.push(3); return x}}}}; missing; end"
            ),
            true,
            false,
        );
    }
}

#[test]
fn generic_selection_calls_converge_and_preserve_possible_hits_and_misses() {
    for (source, args) in [
        (
            "def run(xs:array<int>); xs.grep(7) {|n| [n]}; end",
            vec![Value::array(vec![Value::int(7), Value::int(9)])],
        ),
        (
            "def run(xs:array<int>); xs.grep_v(7) {|n| [n]}; end",
            vec![Value::array(vec![Value::int(7), Value::int(9)])],
        ),
        (
            "def run(xs:array<int>); xs.uniq {|n| n}; end",
            vec![Value::array(vec![
                Value::int(7),
                Value::int(7),
                Value::int(9),
            ])],
        ),
        (
            "def run(xs:array<int>); a=[]; result=xs.uniq {|n| a.push(n); 3}; [a,result]; end",
            vec![Value::array(vec![Value::int(7), Value::int(7)])],
        ),
        (
            "def run(xs:array<int>,key:int); xs.fetch(key) {|n:int| n}; end",
            vec![Value::array(vec![Value::int(7)]), Value::int(-1)],
        ),
        (
            "def run(xs:array<int>,key:float); xs.fetch(key) {|n:int| n}; end",
            vec![Value::array(vec![Value::int(7)]), Value::float(9.0)],
        ),
        (
            "def run(xs:array<int>); xs.grep(7) {missing}; end",
            vec![Value::array(vec![])],
        ),
    ] {
        let rejected = source.contains("missing");
        inferred_runtime(source, &args, rejected);
    }
    for body in [
        "x=[]; result={a:7}.fetch(key) {|n| x.push(n); 3}; [x,result]",
        "x=[]; result={a:7}.fetch_values(:a,key,:a) {|n| x.push(n); 3}; [x,result]",
    ] {
        for key in [Value::bytes("a"), Value::bytes("b")] {
            inferred_runtime(&format!("def run(key:string); {body}; end"), &[key], false);
        }
    }
}

#[test]
fn uniqueness_fact_comparisons_follow_runtime_scalar_and_nested_equality() {
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
        Value::symbol("a"),
    ];
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut inputs: Vec<_> = scalars
        .iter()
        .map(|value| {
            if let crate::value::Kind::Float(value) = value.0 {
                facts.float(&mut ctx, value).unwrap()
            } else {
                literal_fact(&mut ctx, &mut facts, value)
            }
        })
        .collect();
    let mut values = scalars.clone();
    for (index, value) in scalars.iter().enumerate() {
        let fact = inputs[index];
        inputs.push(facts.tuple(&mut ctx, &[fact]).unwrap());
        inputs.push(
            facts
                .shape(&mut ctx, &[(b"a", fact, false)], false)
                .unwrap(),
        );
        values.push(Value::array(vec![value.clone()]));
        values.push(Value::hash(vec![(b"a".to_vec(), value.clone())]));
    }
    for (ai, a) in values.iter().enumerate() {
        for (bi, b) in values.iter().enumerate() {
            let expected = crate::sets::contains(&mut ctx, std::slice::from_ref(a), b).unwrap();
            let left = inputs[ai];
            let right = inputs[bi];
            let actual = facts.set_equal(&mut ctx, left, right).unwrap();
            assert!(
                matches!(facts.node(actual), Node::Boolean(value) if *value == expected),
                "{a:?} / {b:?}: {:?}",
                facts.node(actual)
            );
        }
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn mixed_keys_patterns_and_optional_fields_keep_valid_and_error_paths() {
    for key in [
        Value::int(0),
        Value::float(-1.0),
        Value::float(0.5),
        Value::bytes("bad"),
    ] {
        inferred_runtime(
            "def run(key:int|float|string); x=[]; begin; result=[7].fetch(key) {|n:int| x.push(n); 9}; [x,result]; rescue RuntimeError; x; end; end",
            &[key],
            true,
        );
    }
    for key in [Value::symbol("a"), Value::bytes("b"), Value::int(7)] {
        inferred_runtime(
            "def run(key:string|symbol|int); x=[]; begin; result={a:nil}.fetch_values(key,:a,key) {|n| x.push(n); 9}; [x,result]; rescue RuntimeError; x; end; end",
            &[key],
            true,
        );
    }
    for key in [
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
        Value::float(f64::NEG_INFINITY),
        Value::float(9223372036854775808.0),
    ] {
        assert_eq!(inferred_runtime("def run(key:float); x=[]; begin; [7].fetch(key) {x.push(9)}; rescue RuntimeError; x; end; end", &[key], false).to_string(), "[]");
    }
    for flag in [false, true] {
        for body in [
            "h=if flag; {a:nil}; else; {}; end; x=[]; result=h.fetch(:a) {x.push(7); 9}; [x,result]",
            "h=if flag; {a:nil}; else; {a:7}; end; h.fetch(:a) {missing}",
            "key=if flag; :a; else; :b; end; x=[]; result={a:7}.fetch_values(key,:a) {|n| x.push(n); 9}; [x,result]",
            "pattern=if flag; 7; else; 9; end; x=[]; result=[7,9].grep(pattern) {|n| x.push(n); 3}; [x,result]",
            "pattern=if flag; 7; else; 9; end; x=[]; result=[7,9].grep_v(pattern) {|n| x.push(n); 3}; [x,result]",
            "key=if flag; 7; else; 9; end; [7,9,7].uniq {key}",
        ] {
            inferred_runtime(
                &format!("def run(flag:bool); {body}; end"),
                &[Value::boolean(flag)],
                false,
            );
        }
    }
    let big = Value::parse_integer("1000000000000000000000000000000", 10).unwrap();
    assert_eq!(inferred_runtime("def run(key:int); x=[]; begin; [7].fetch(key) {x.push(9)}; rescue RuntimeError; x; end; end", &[big], false).to_string(), "[]");
}

#[test]
fn selection_errors_retry_and_invalid_parameters_preserve_completed_writes() {
    for call in [
        "[7,7].grep(7)",
        "[7,7].grep_v(9)",
        "[7,7].uniq",
        "[7].fetch(9)",
        "{}.fetch_values(:a,:b)",
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
        witness(
            &format!(
                "def run; x=[]; begin; {call} {{x.push(7); if x.length<2; raise \"again\"; end; 3}}; rescue; retry; end; x; end"
            ),
            false,
            false,
        );
    }
    for call in [
        "[7,9].grep(7)",
        "[7,9].grep_v(7)",
        "[7,9].uniq",
        "[7].fetch(9)",
    ] {
        witness(
            &format!(
                "def run; x=[]; begin; {call} {{|n:string| x.push(7)}}; rescue RuntimeError; x; end; end"
            ),
            true,
            true,
        );
    }
    witness(
        "def run; x=[]; begin; {}.fetch_values(\"a\",:b) {|key:string| x.push(key); 7}; rescue RuntimeError; x; end; end",
        true,
        true,
    );
    for body in ["7", "next 7", "break 7", "return 7", "raise \"body\""] {
        for cleanup in ["9", "next 9", "break 9", "return 9", "raise \"cleanup\""] {
            witness(
                &format!(
                    "def run; x=[]; begin; result={{}}.fetch_values(:a,:b) {{begin; x.push(3); {body}; ensure; x.push(9); {cleanup}; end}}; [x,result]; rescue; x; end; end"
                ),
                true,
                false,
            );
        }
    }
}

#[test]
fn selection_value_copies_preserve_protection_and_native_depth_guards() {
    for source in [
        "def run; m=/(a)/.match(\"a\"); if m; m.fetch(:captures) {missing}; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; m.fetch_values(:captures,:captures) {missing}; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; copy=m.fetch(:captures); [7,7].uniq {copy.push(\"x\"); 3}; [copy,m.captures]; end; end",
        "def run; m=/(a)/.match(\"a\"); if m; [m,m.dup].uniq {|n| n}; end; end",
    ] {
        witness(source, false, false);
    }
    for call in [
        "[7].grep(7)",
        "[7].uniq",
        "{}.fetch(:a)",
        "{}.fetch_values(:a)",
    ] {
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
        ("[7,7].grep(7)", "[3]"),
        ("[7,7].grep_v(9)", "[3]"),
        ("{}.fetch_values(:a,:b)", "[3]"),
        ("[7,7].uniq", "[3, 3, 9]"),
        ("{}.fetch(:a)", "[3, 9]"),
        ("[7].fetch(9)", "[3, 9]"),
    ] {
        let source = format!(
            "def run(v:any); x=[]; begin; {call} {{x.push(3); v}}; x.push(9); rescue LimitError; x; end; x; end"
        );
        assert_eq!(
            inferred_runtime(&source, std::slice::from_ref(&value), false).to_string(),
            expected
        );
        let source =
            format!("def run(v:any); x=[]; result={call} {{x.push(3); break 7}}; [x,result]; end");
        assert_eq!(
            inferred_runtime(&source, std::slice::from_ref(&value), false).to_string(),
            "[[3], 7]"
        );
    }
}

#[test]
fn uniqueness_nan_keys_keep_the_root_and_nested_equality_distinction() {
    for (body, expected) in [
        ("xs.uniq {|n| n}", "[NaN]"),
        ("xs.uniq {|n| [n]}", "[NaN, NaN]"),
        ("xs.uniq {|n| {a:n}}", "[NaN, NaN]"),
    ] {
        let source = format!("def run(xs:array<float>); {body}; end");
        assert_eq!(
            inferred_runtime(
                &source,
                &[Value::array(vec![
                    Value::float(f64::NAN),
                    Value::float(f64::NAN)
                ])],
                false
            )
            .to_string(),
            expected
        );
    }
}

#[test]
fn nested_and_recursive_selection_callbacks_use_the_default_stack() {
    let mut body = "x.push(7)".to_owned();
    for i in 0..24 {
        let call = [
            "[7].grep(7)",
            "[7].uniq",
            "{}.fetch_values(:a)",
            "{}.fetch(:a)",
        ][i % 4];
        body = format!("{call} {{{body}; 3}}");
    }
    witness(&format!("def run; x=[]; {body}; x; end"), true, false);
    witness(
        "def recurse(n:int); if n>0; [7,7].uniq {recurse(n-1); 3}; else; []; end; end; def run; recurse(7); end",
        false,
        false,
    );
    witness(
        "def forward; {}.fetch(:a) {yield}; end; def run; forward {return 7}; missing; end",
        true,
        false,
    );
}

#[test]
fn uniqueness_equality_handles_deep_shared_facts_without_recursive_frames() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut left = facts.integer(&mut ctx, 1).unwrap();
    let mut right = facts.float(&mut ctx, 1.0).unwrap();
    for _ in 0..4000 {
        left = facts.tuple(&mut ctx, &[left, left]).unwrap();
        right = facts.tuple(&mut ctx, &[right, right]).unwrap();
    }
    let value = facts.set_equal(&mut ctx, left, right).unwrap();
    assert!(matches!(facts.node(value), Node::Boolean(true)));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn shared_abstract_uniqueness_facts_do_not_imply_identical_values() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let tuple = facts.tuple(&mut ctx, &[Atom::Int.fact()]).unwrap();
    let shape = facts
        .shape(&mut ctx, &[(b"a", Atom::Int.fact(), false)], false)
        .unwrap();
    let optional = facts
        .shape(&mut ctx, &[(b"a", Atom::Nil.fact(), true)], false)
        .unwrap();
    let array = facts.array(&mut ctx, Atom::Int.fact()).unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    let either = facts.union(&mut ctx, &[one, two]).unwrap();
    for value in [
        Atom::Unknown.fact(),
        Atom::Any.fact(),
        Atom::Int.fact(),
        Atom::Float.fact(),
        tuple,
        shape,
        optional,
        array,
        either,
    ] {
        assert_eq!(
            facts.set_equal(&mut ctx, value, value).unwrap(),
            Atom::Bool.fact()
        );
    }
    let protected = facts
        .protected(&mut ctx, shape, crate::hash::Tag::Match)
        .unwrap();
    assert_eq!(
        facts.set_equal(&mut ctx, shape, protected).unwrap(),
        Atom::Bool.fact()
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def run(xs:array<int>,key:string); x=[]; begin; a=xs.grep(7) {|n| x.push(n); [n]}; b=xs.uniq {|n| x.push(n); [n]}; c={a:7}.fetch_values(key,:a,key) {|k| x.push(k); 9}; [a,b,c,x]; ensure; x.push(3); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn selection_fixed_points_have_exact_quotas_and_reclaim_failed_allocations() {
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
fn selection_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn unsupported_selection_receivers_and_retained_addresses_remain_explicit() {
    for source in [
        "def run(xs); xs.grep(7) {|n| n}; end",
        "def run(h:hash<string,int>); h.fetch(:a) {7}; end",
        "def run(h:hash<string,int>); h.fetch_values(:a) {7}; end",
        "def run; [7,9].fill {|n| n}; end",
        "def run; [7,9].map! {|n| n}; end",
        "def run; a=[1]; a.push({}.fetch(:a) {a.clear; 7}); a; end",
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
fn selection_reference_decisions_have_independent_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-selections.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 63);
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
