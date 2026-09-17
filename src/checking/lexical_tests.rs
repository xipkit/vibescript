use super::{
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine};

fn witness(source: &str, exact: bool, warnings: bool) {
    let actual = Engine::new()
        .compile(source)
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .value;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap_or_else(|e| panic!("{source}: {e}"));
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        warnings,
        "{source}: {report:?}"
    );
    let actual = literal_fact(&mut ctx, &mut facts, &actual);
    assert_ne!(
        facts.relation(&mut ctx, actual, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}"
    );
    if exact {
        assert_eq!(
            actual,
            report.returns,
            "{source}: expected {:?}, got {:?}",
            facts.node(actual),
            facts.node(report.returns)
        );
    }
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn deep_captures_cross_scopes_that_never_read_the_binding() {
    for source in [
        "def once; yield; end; def run; x=0; once {once {once {x=7}}}; x; end",
        "def once; yield; end; def run; x=7; once {once {once {x}}}; end",
        "def once; yield; end; def run; x=[]; once {once {once {x.push(7)}}}; x; end",
        "def once; yield; end; def run; x={a:[1]}; once {once {once {x[:a].push(7)}}}; x; end",
        "def twice; yield; yield; end; def once; yield; end; def run; x=[]; twice {once {once {x.push(7)}}}; x; end",
        "def once; yield; end; def run; x=[]; once {once {x.push(7)}; once {x.push(9)}}; x; end",
        "def once; yield; end; def run; x=0; once {once {x=7}; x}; x; end",
        "def once; yield; end; def run; x=0; result=once {y=1; once {once {x=7;y=9}}; [x,y]}; [x,result]; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn lexical_parameter_shadowing_keeps_outer_bindings_independent() {
    for source in [
        "def once; yield 3; end; def run; x=0; result=once {|x| once {once {x=7}}; x}; [x,result]; end",
        "def once; yield [3,4]; end; def run; x=0;y=1; result=once {|x,y| once {once {x=7;y=9}}; [x,y]}; [x,y,result]; end",
        "def once; yield 3; end; def run; it=0; result=once {once {once {it=7}}; it}; [it,result]; end",
        "def once; yield 3; end; def run; x=0; result=once {once {|x| once {x=7}; x}}; [x,result]; end",
        "def once; yield 3; end; def run; x=0; result=once {|x| once {once {|x| x=9}}; x}; [x,result]; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn deep_capture_exits_preserve_errors_cleanup_and_nonlocal_control() {
    for source in [
        "def once; yield; end; def run; x=0; begin; once {once {once {x=7; raise \"bad\"}}}; rescue; x; end; end",
        "def once; yield; end; def run; x=0; begin; once {once {once {x=7; return 9}}}; ensure; return x; end; end",
        "def once; yield; end; def run; x=0; result=once {once {once {x=7; break 9}}}; [result,x]; end",
        "def once; yield; end; def run; x=0; result=once {once {once {begin; x=7; return 2; ensure; x=9; next 3; end}}}; [result,x]; end",
        "def once; begin; yield; rescue; 9; end; end; def run; x=0; result=once {once {once {x=7; raise \"bad\"}}}; [result,x]; end",
        "def once; yield; end; def run; x=0; result=once {once {once {begin; break 3; ensure; x=7; end}}}; [result,x]; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn unbound_lexical_bindings_stay_local_until_the_owner_is_bound() {
    for source in [
        "def foo; 7; end; def once; yield; end; def run; result=once {once {once {foo()}}}; foo=9; result; end",
        "def once; yield; end; def run; result=once {once {once {x=7}}}; x=9; [result,x]; end",
        "def once; yield; end; def run; x=nil; result=once {once {once {x=7}}}; [result,x]; end",
        "def once; yield; end; def run; if false; x=0; end; result=once {once {once {x=7}}}; [result,x]; end",
        "def once; yield; end; def run; result=once {once {once {x=7}}; x=9; x}; [result]; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn deep_captures_keep_value_copies_and_match_protection() {
    for (source, exact, warnings) in [
        (
            "def foo; 3; end; def once; yield; end; def run; foo=9; begin; once {once {once {foo()}}}; rescue; 7; end; end",
            true,
            true,
        ),
        (
            "def once; yield; end; def run; x=[]; copy=x; once {once {once {x.push(7)}}}; [x,copy]; end",
            true,
            false,
        ),
        (
            "def once; yield; end; def run; x=[]; once {once {once {copy=x;copy.push(7)}}}; x; end",
            true,
            false,
        ),
        (
            "def once; yield; end; def run; x=[]; once {copy=x; once {once {copy.push(7)}}}; x; end",
            true,
            false,
        ),
        (
            "def once; yield; end; def run; x=0; once {once {once {x=\"bad\"}}}; x; end",
            true,
            true,
        ),
        (
            "def once; yield; end; def run; m=/(a)/.match(\"a\"); if m; begin; once {once {once {m.captures.push(7)}}}; rescue; m.captures; end; end; end",
            false,
            true,
        ),
        (
            "def once; yield; end; def run; m=/(a)/.match(\"a\"); if m; c=m.captures; once {once {once {c.push(\"x\")}}}; [c,m.captures]; end; end",
            false,
            false,
        ),
    ] {
        witness(source, exact, warnings);
    }
}

#[test]
fn deep_captures_preserve_control_and_rescued_error_classes_at_every_depth() {
    use crate::ErrorClass;
    for class in [
        ErrorClass::Runtime,
        ErrorClass::Standard,
        ErrorClass::Assertion,
        ErrorClass::Limit,
        ErrorClass::Type,
        ErrorClass::ZeroDivision,
        ErrorClass::LocalJump,
        ErrorClass::Argument,
    ] {
        let class = class.name();
        witness(
            &format!(
                "def once; yield; end; def run; x=0; begin; once {{once {{once {{begin; raise {class}, \"bad\"; ensure; x=7; end}}}}}}; rescue {class}; x; end; end"
            ),
            true,
            false,
        );
    }
    for body in ["7", "next 7", "break 7", "return 7", "raise \"body\""] {
        for cleanup in ["9", "next 9", "break 9", "return 9", "raise \"cleanup\""] {
            witness(
                &format!(
                    "def once; yield; end; def run; x=[]; begin; result=once {{once {{once {{begin; x.push(1); {body}; ensure; x.push(2); {cleanup}; end}}}}}}; [result,x]; rescue; x; end; end"
                ),
                true,
                false,
            );
        }
    }
    witness(
        "def once; yield; end; def twice; begin; yield; ensure; yield; end; end; def run; a=[]; begin; twice {once {once {a.push(7); return 3}}}; ensure; return a; end; end",
        true,
        false,
    );
}

#[test]
fn deep_capture_layouts_work_in_nested_loops_and_recursive_calls() {
    for source in [
        "def once; yield; end; def run; x=[]; for n in [1,2]; once {once {once {x.push(n)}}}; end; x; end",
        "def once; yield; end; def walk(n:int); x=[]; if n>0; once {once {once {x=[walk(n-1)]}}}; end; x; end; def run -> array; walk(3); end",
        "def once; yield; end; def run(flag:bool=true); x=[]; if flag; once {once {once {x.push(7)}}}; else; once {once {once {x.push(9)}}}; end; x; end",
        "def once; yield; end; def run; x=[]; begin; once {once {once {x.push(7); if x.length<2; raise \"again\"; end}}}; rescue; retry; end; x; end",
    ] {
        witness(source, false, false);
    }
}

#[test]
fn lexical_links_stay_attached_to_the_nearest_parameter_after_nested_calls() {
    for source in [
        "def once; yield 3; end; def run; x=0; result=once {|x| before=once {once {x=7}}; once {once {x=9}}; [before,x]}; [x,result]; end",
        "def once; yield 3; end; def run; _1=0; result=once {once {once {_1=7}}; _1}; [_1,result]; end",
        "def once; yield [3,[4,5]]; end; def run; x=0;y=1; result=once {|x,(y,z)| once {once {x=7;y=9}}; [x,y,z]}; [x,y,result]; end",
        "def once; yield; end; def run; x=[]; result=once {x=[]; once {once {x.push(7)}}; x}; [x,result]; end",
    ] {
        witness(source, true, false);
    }
}

#[test]
fn skipped_lexical_scopes_use_the_default_stack() {
    for depth in [2, 8, 32] {
        let source = format!(
            "def once; yield; end; def run; x=[]; {}x.push(7); {}; x; end",
            "once {".repeat(depth),
            "};".repeat(depth)
        );
        witness(&source, true, false);
    }
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let source = "def twice; yield; yield; end; def once; yield; end; def run(flag:bool); x=[]; begin; result=twice {once {once {begin; x.push(7); if flag; return x; else; raise \"bad\"; end; ensure; x.push(9); end}}}; [x,result]; rescue; x; ensure; x.push(11); end; end";
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, source)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn lexical_layouts_and_hidden_capture_storage_obey_exact_quotas_and_cleanup() {
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
fn lexical_layout_analysis_observes_latched_cancellation_and_deadlines() {
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
fn lexical_capture_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-lexical.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 16);
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
    assert_eq!(differences, 4);
    witness(
        "def once; yield; end; def run; x=0; result=once {begin; raise \"bad\"; rescue => x; once {once {x.message}}; end}; [x,result]; end",
        false,
        false,
    );
}
