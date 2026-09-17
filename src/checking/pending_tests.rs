use super::{
    collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime,
    lexical_tests::witness,
};
use crate::{CallContext, CallOptions, Value};

#[test]
fn previously_incomplete_pending_callbacks_have_runtime_witnesses() {
    for source in [
        "def once; yield; end; def run; a=[[1]]; a[0].push(once {a[0]=[2]; 7}); a; end",
        "def once; yield; end; def run; a=[1]; a[-1]+=once {a.push(2); 7}; a; end",
        "def once; yield; end; def run; a=[1]; a.push(once {a=a; 7}); a; end",
        "def run; a=[1]; a.push([1].map {a.push(7); 9}); a; end",
        "def run; a=[1]; a.push([1].find {a.clear; true}); a; end",
        "def run; a=[1]; a.push([7].group_by {a.clear; :a}); a; end",
        "def run; a=[1]; a.push(7.yield_self {a.clear; 9}); a; end",
        "def run; a=[1]; a.push({}.fetch(:a) {a.clear; 7}); a; end",
    ] {
        witness(source, !source.contains("+="), false);
    }
}

#[test]
fn pending_roots_distinguish_mutation_replacement_and_write_order() {
    for body in [
        "a.push(once {a.push(2); 7}); a",
        "a.push(once {a=a; a.push(2); 7}); a",
        "a.push(once {a=[1]; a.push(2); 7}); a",
        "a.push(once {a.push(2); a=[1]; 7}); a",
        "a.push(once {b=a; a=[]; a=b; 7}); a",
        "a.push(once {a.clear; a.push(2); 7}); a",
        "a.push(once {a.push(once {a.push(2); 3}); 7}); a",
        "a.push(once {once {a.push(2)}; once {a.push(3)}; 7}); a",
        "a[-1]=once {a.push(2); 7}; a",
        "a[0]=once {a=[1]; 7}; a",
    ] {
        witness(
            &format!("def once; yield; end; def run; a=[1]; {body}; end"),
            true,
            false,
        );
    }
}

#[test]
fn pending_nested_paths_keep_selected_children_and_original_indexes() {
    for body in [
        "a[-1].push(once {a.push([3]); 7})",
        "a[-1].push(once {a[-1]=[2]; 7})",
        "a[0].push(once {a[1]=[2]; 7})",
        "a[0].push(once {a[0]=[1]; 7})",
        "a[0].push(once {a[0]=a[0]; 7})",
        "a[0].push(once {a[0].push(2); a.push([3]); 7})",
        "a[0].push(once {a.clear; a.push([1]); 7})",
        "a[0].push(once {a[0]=[1]; a[0].push(2); 7})",
        "a[0].push(once {a[0].clear; a[0].push(2); 7})",
    ] {
        witness(
            &format!(
                "def once; yield; end; def run; a=[[1],[2]]; b=a; result={body}; [a,b,result]; end"
            ),
            false,
            false,
        );
    }
    for body in [
        "a.x.push(once {a.store(:y,[3]); 7})",
        "a.x.push(once {a.store(:x,[3]); 7})",
        "a.x.push(once {a.delete(:x); a.store(:x,[1]); 7})",
        "a.x.push(once {a.x.clear; a.x.push(3); 7})",
    ] {
        witness(
            &format!(
                "def once; yield; end; def run; a={{x:[1],y:[2]}}; b=a; result={body}; [a,b,result]; end"
            ),
            false,
            false,
        );
    }
}

#[test]
fn native_callback_schedules_refresh_outer_addresses() {
    for call in [
        "[7,9].map",
        "[7,9].each",
        "[7,9].select",
        "[7,9].reject",
        "[7,9].group_by",
        "[7,9].partition",
        "[7,9].uniq",
        "[7,9].sort_by",
        "[7,9].each_slice(1)",
        "[7,9].each_cons(1)",
        "[7,9].cycle(2)",
        "[7,9].grep(0..9)",
        "[7,9].fetch(3)",
        "{}.fetch(:a)",
        "{}.fetch_values(:a,:b)",
        "7.tap",
        "7.yield_self",
        "2.times",
    ] {
        let key = if call.ends_with("group_by") {
            ":key"
        } else {
            "0"
        };
        witness(
            &format!("def run; a=[1]; a.push({call} {{a.push(3); {key}}}); a; end"),
            true,
            false,
        );
    }
    for call in ["loop", "[7,9].sort", "[7,9].min_by", "[7,9].max_by"] {
        witness(
            &format!("def run; a=[1]; a.push({call} {{a.push(3); break 7}}); a; end"),
            true,
            false,
        );
    }
}

#[test]
fn callback_transfers_and_cleanup_preserve_pending_write_order() {
    for body in [
        "a.push(once {a.push(2); next 7}); a",
        "a.push(once {a.push(2); break 7}); a",
        "begin; a.push(once {a.push(2); return 7}); ensure; return a; end",
        "begin; a.push(once {a.push(2); raise \"bad\"}); rescue; a; end",
        "a.push(once {begin; a.push(2); break 7; ensure; a.push(3); end}); a",
        "a.push(once {begin; a.push(2); return 7; ensure; a.push(3); next 9; end}); a",
        "a.push(once {begin; a.push(2); raise \"bad\"; rescue; a.push(3); 7; end}); a",
        "a.push(once {done=false; begin; a.push(2); unless done; done=true; raise \"again\"; end; rescue; retry; end; 7}); a",
    ] {
        witness(
            &format!("def once; yield; end; def run; a=[1]; {body}; end"),
            false,
            false,
        );
    }
}

#[test]
fn forwarded_callbacks_and_shadowed_parameters_keep_observer_owners() {
    for source in [
        "def once; yield; end; def outer; once {yield}; end; def run; a=[1]; a.push(outer {a.push(2); 7}); a; end",
        "def once; yield; end; def outer; once {once {yield}}; end; def run; a=[1]; a.push(outer {a.push(2); 7}); a; end",
        "def once; yield; end; def outer; a=[3]; a.push(once {a.push(4); yield}); a; end; def run; a=[1]; a.push(outer {a.push(2); 7}); a; end",
        "def once; yield [3]; end; def run; a=[1]; a.push(once {|a| once {a.push(4)}; 7}); a; end",
        "def once; yield [3]; end; def run; a=[1]; a.push(once {once {|a| a.push(4)}; a.push(2); 7}); a; end",
        "def once; yield; end; def outer; once {yield}; once {yield}; end; def run; a=[1]; a.push(outer {a.push(2); 7}); a; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn branches_and_fixed_points_cover_pending_callback_results() {
    for body in [
        "a.push(once {if flag; a.push([3]); else; a=[[1],[2]]; end; 7})",
        "a[-1].push(once {if flag; a.pop; else; a[0].push(3); end; 7})",
        "a[0].push(once {if flag; a[0]=[1]; end; 7})",
        "a[0].push(once {while flag; a[0].push(2); break; end; 7})",
        "a[0].push(loop {a[0].push(2); break 7})",
    ] {
        let source = format!(
            "def once; yield; end; def run(flag:bool); a=[[1],[2]]; result={body}; [a,result]; end"
        );
        for flag in [false, true] {
            inferred_runtime(&source, &[Value::boolean(flag)], false);
        }
    }
    for n in [0, 1, 3] {
        inferred_runtime(
            "def once; yield; end; def run(n:int); a=[1]; a.push(once {for i in 0...n; a.push(2); end; 7}); a; end",
            &[Value::int(n)],
            false,
        );
    }
}

#[test]
fn nested_observers_use_the_default_stack_and_preserve_copies() {
    let mut body = "a.push(2)".to_owned();
    for _ in 0..24 {
        body = format!("once {{{body}}}");
    }
    witness(
        &format!(
            "def once; yield; end; def run; a=[1]; b=a; a.push(once {{{body}; 7}}); [a,b]; end"
        ),
        true,
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def once; yield; end; def forward; once {yield}; end; def run(flag:bool); a=[[1],[2]]; a[-1].push(once {forward {if flag; a[-1].push(3); else; a[0]=[4]; end}; [7,9].map {a.push([3]); 0}; 7}); a; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn pending_callback_observations_have_exact_quotas_and_failure_cleanup() {
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
fn pending_callback_analysis_keeps_cancellation_and_deadlines_latched() {
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
fn callback_address_facts_cover_parent_mutation_routes() {
    let engine = crate::Engine::new();
    let mut cases = 0;
    for root in ["[[1],[2]]", "[[1],[1]]", "[[],[1]]"] {
        for selected in ["0", "1", "-1"] {
            for mutation in [
                "a.push([3])",
                "a.prepend([3])",
                "a.insert(0,[3])",
                "a.insert(1,[3])",
                "a.insert(-1,[3])",
                "a.pop",
                "a.shift",
                "a.pop(0)",
                "a.shift(0)",
                "a.pop(1)",
                "a.shift(1)",
                "a.clear",
                "a.delete([1])",
                "a.fill([3])",
                "a.fill([3],0,1)",
                "a[0]=[3]",
                "a[1]=[3]",
                "a[-1]=[3]",
                "a[0]=a[0]",
                "a[0]=a[1]",
                "a=a",
                "a=b",
                "a=[[1],[1]]",
                "a=[]",
                "a[0].push(3)",
                "a[1].push(3)",
            ] {
                let source = format!(
                    "def once; yield; end; def run; a={root}; b=a; result=a[{selected}].push(once {{once {{{mutation}}}; 7}}); [a,b,result]; end"
                );
                super::address_tests::runtime_fact(&engine, &source, &[]);
                cases += 1;
            }
        }
    }
    for selected in ["x", "y"] {
        for mutation in [
            "a.store(:x,[3])",
            "a.store(:y,[3])",
            "a.store(:z,[3])",
            "a.delete(:x)",
            "a.delete(:y)",
            "a.delete(:z)",
            "a.clear",
            "a.replace({x:[3],y:[4]})",
            "a.x=[3]",
            "a.y=[3]",
            "a.x=a.x",
            "a.x=a.y",
            "a=a",
            "a=b",
            "a={x:[1],y:[1]}",
            "a.x.push(3)",
            "a.y.push(3)",
        ] {
            let source = format!(
                "def once; yield; end; def run; a={{x:[1],y:[1]}}; b=a; result=a.{selected}.push(once {{once {{{mutation}}}; 7}}); [a,b,result]; end"
            );
            super::address_tests::runtime_fact(&engine, &source, &[]);
            cases += 1;
        }
    }
    assert_eq!(cases, 268);
}

#[test]
fn rescue_bindings_and_nested_writes_do_not_replace_outer_capture_owners() {
    for source in [
        "def once; yield; end; def run; a=[1]; result=a.push(once {begin; raise \"bad\"; rescue =>a; a.message; end; 7}); [a,result]; end",
        "def once; yield; end; def run; a=[1]; result=a.push(once {begin; raise \"bad\"; rescue =>a; once {a.message}; end; a.push(2); 7}); [a,result]; end",
        "def once; yield; end; def run; a=[1]; result=a.push(once {begin; raise \"bad\"; rescue =>a; once {a.message}; ensure; a.push(2); end; 7}); [a,result]; end",
    ] {
        witness(source, false, false);
    }
}

#[test]
fn callback_summaries_distinguish_suspended_paths_with_identical_capture_values() {
    for operation in ["a[0]=[1]", "a[1]=[2]", "a[0].clear", "a.clear", "a=a"] {
        let source = format!(
            "def apply(i:int); a=[[1],[2]]; result=a[i].push([7].map {{{operation}; 3}}); [a,result]; end; def run; [apply(0),apply(1),apply(0)]; end"
        );
        witness(&source, true, false);
    }
    for flag in [false, true] {
        inferred_runtime(
            "def once; yield; end; def run(flag:bool); a=[[1],[2]]; result=a[if flag; 0; else; 1; end].push(once {a[0]=[1]; 3}); [a,result]; end",
            &[Value::boolean(flag)],
            false,
        );
    }
    witness(
        "def add(n:int); a=[1]; a.push([7].map {if n>0; add(n-1); end; a.push(2); 3}); a; end; def run; add(3); end",
        false,
        false,
    );
}
