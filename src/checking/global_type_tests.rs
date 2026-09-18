use super::{
    facts::{Atom, Facts},
    flow::IssueKind,
    normalization_tests::{analyze, witness},
};
use crate::{CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Value, bytecode};

#[test]
fn parameters_and_returns_resolve_current_global_aliases() {
    for (source, expected) in [
        (
            "def echo(x:Math)->Math; x; end; def run; Math=Status; echo(:draft).enum.name; end",
            "Status",
        ),
        (
            "def echo(x:Math)->Math; x; end; def run; Math=Status; first=echo(:draft); Math=Review; second=echo(:draft); [first.enum.name,second.enum.name]; end",
            "[Status, Review]",
        ),
        ("def run -> Math; Math=Status; :draft; end", "Status::Draft"),
        (
            "def echo(x:srand)->srand; x; end; def run; srand=Review; echo(:draft).enum.name; end",
            "Review",
        ),
        (
            "def change; Math=Review; 7; end; def echo(first=change,x:Math=:draft); x.enum.name; end; def run; Math=Status; echo(); end",
            "Review",
        ),
        (
            "def change; Math=Review; 7; end; def echo(first=change,x:Math:); x.enum.name; end; def run; Math=Status; echo(x: :draft); end",
            "Review",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn return_aliases_observe_ensure_and_callback_global_writes() {
    for (source, expected) in [
        (
            "def echo -> Math; begin; :draft; ensure; Math=Review; end; end; def run; Math=Status; echo.enum.name; end",
            "Review",
        ),
        (
            "def change; Math=Review; end; def echo -> Math; [1].each {change}; :draft; end; def run; Math=Status; echo.enum.name; end",
            "Review",
        ),
        (
            "def change; Math=Review; end; def echo -> Math; [1].each {begin; return :draft; ensure; change; end}; end; def run; Math=Status; echo.enum.name; end",
            "Review",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn global_aliases_refresh_across_repeated_callbacks() {
    for (source, expected) in [
        (
            "def change; Math=Review; end; def run; Math=Status; [:draft,:draft].map {|x:Math| name=x.enum.name; change; name}; end",
            "[Status, Review]",
        ),
        (
            "def once; yield(:draft); end; def run; Math=Review; once {|x:Math| x.enum.name}; end",
            "Review",
        ),
        (
            "def echo(x:Math); x.enum.name; end; def run; Math=Status; [1].map {Math=Review; [echo(:draft),[:draft].map {|x:Math| x.enum.name}]}; end",
            "[[Status, [Review]]]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn missing_and_ambiguous_global_contracts_fail_at_the_catchable_boundary() {
    for source in [
        "def echo(x:Math); 1; end; def run; Math=7; begin; echo(:draft); rescue; 99; end; end",
        "def echo -> Math; Math=7; :draft; end; def run; Math=Status; begin; echo; rescue; 99; end; end",
        "def echo(x:Missing?); 1; end; def run; begin; echo(nil); rescue; 99; end; end",
        "def echo(x:array<Missing|any>); 1; end; def run; begin; echo([]); rescue; 99; end; end",
        "enum MATH; Draft; end; def echo(x:math); x; end; def run; Math=Review; begin; echo(:draft); rescue; 99; end; end",
    ] {
        witness(source, &[], "99", true);
    }
}

#[test]
fn globals_and_source_declarations_share_exact_and_folded_lookup_rules() {
    for (source, expected) in [
        (
            "enum MATH; Draft; end; def echo(x:MATH); x.enum.name; end; def run; Math=Review; echo(:draft); end",
            "MATH",
        ),
        (
            "enum MATH; Draft; end; def echo(x:Math); x.enum.name; end; def run; Math=Review; echo(:draft); end",
            "Review",
        ),
        (
            "enum MATH; Draft; end; def echo(x:math); x.enum.name; end; def run; Math=MATH; echo(:draft); end",
            "MATH",
        ),
        (
            "enum MATH; Draft; end; def echo(x:math); x.enum.name; end; def run; Math=7; echo(:draft); end",
            "MATH",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn qualified_global_exports_resolve_without_executing_namespace_code() {
    for (source, expected) in [
        (
            "def echo(x:Math.State); x.enum.name; end; def run; Math[:State]=Status; echo(:draft); end",
            "Status",
        ),
        (
            "def echo(x:Math.STate); x.enum.name; end; def run; Math[:State]=Status; echo(:draft); end",
            "Status",
        ),
        (
            "def echo(x:Math.State); x.enum.name; end; def run; Math[:State]=Status; first=echo(:draft); Math[:State]=Review; [first,echo(:draft)]; end",
            "[Status, Review]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn parameter_contracts_resolve_in_argument_and_default_order() {
    for (source, expected) in [
        (
            "def change; Math=Review; :draft; end; def echo(x:Math); x.enum.name; end; def run; Math=Status; echo(change); end",
            "Review",
        ),
        (
            "def change; Math=Review; 7; end; def echo(x:Math,y=change); [x.enum.name,Math.name]; end; def run; Math=Status; echo(:draft); end",
            "[Status, Review]",
        ),
        (
            "def echo(x:array<{state:Math}>) -> array<hash<string,Math>>; x; end; def run; Math=Status; echo([{state: :draft}])[0].state.enum.name; end",
            "Status",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn type_lookup_failures_keep_prior_effects_and_obey_frame_boundaries() {
    for (source, expected) in [
        (
            "def change; Math=7; JSON.push(1); 9; end; def echo(first=change,x:Math=:draft); JSON.push(2); end; def run; Math=Status; JSON=[]; begin; echo(); rescue; [Math,JSON]; end; end",
            "[7, [1]]",
        ),
        (
            "def change; Math=7; JSON.push(1); 9; end; def echo(first=change,x:Math:); JSON.push(2); end; def run; Math=Status; JSON=[]; begin; echo(x: :draft); rescue; [Math,JSON]; end; end",
            "[7, [1]]",
        ),
        (
            "def echo -> Math; begin; :draft; rescue; JSON.push(1); :draft; ensure; Math=7; end; end; def run; Math=Status; JSON=[]; begin; echo; rescue; JSON.push(2); end; [Math,JSON]; end",
            "[7, [2]]",
        ),
        (
            "def change; Math=Review; 7; end; def echo(first=change,x:Missing=:draft); 1; end; def run; Math=Status; begin; echo(); rescue; Math.name; end; end",
            "Review",
        ),
    ] {
        witness(source, &[], expected, true);
    }
}

#[test]
fn qualified_aliases_keep_exact_roots_folded_members_and_non_type_fallbacks() {
    for (source, expected) in [
        (
            "def echo(x:Math.State); x.enum.name; end; def run; Math[:State]=7; Math[:STATE]=Status; echo(:draft); end",
            "Status",
        ),
        (
            "def echo(x:Math.State); x.enum.name; end; def run; Math[:State]=Status; Math[:STATE]=Review; echo(:draft); end",
            "Status",
        ),
        (
            "def echo(x:Math.STate); x.enum.name; end; def run; Math[:State]=Status; Math[:STATE]=Status; echo(:draft); end",
            "Status",
        ),
        (
            "def run; Math[:State]=Status; saved=Math; Math[:State]=Review; [:draft].map {|x:saved.State| x.enum.name}; end",
            "[Status]",
        ),
    ] {
        witness(source, &[], expected, false);
    }
    for source in [
        "def echo(x:math.State); 1; end; def run; Math[:State]=Status; begin; echo(:draft); rescue; 99; end; end",
        "def echo(x:Math.STate); 1; end; def run; Math[:State]=Status; Math[:STATE]=Review; begin; echo(:draft); rescue; 99; end; end",
        "def echo(x:Math.State); 1; end; def run; Math={State:Status}; begin; echo(:draft); rescue; 99; end; end",
        "def echo(x:Math.State); 1; end; def run; Math[:State]=Status; Math[:State]=7; begin; echo(:draft); rescue; 99; end; end",
    ] {
        witness(source, &[], "99", true);
    }
}

#[test]
fn namespace_index_writes_keep_pending_children_value_copies_and_protection() {
    for (source, expected, warnings) in [
        (
            "def change; Math[:State]=Review; 9; end; def run; Math[:items]=[1]; Math[:items].push(change); Math[:items]; end",
            "[1, 9]",
            false,
        ),
        (
            "def change; Math[:items]=[7]; 9; end; def run; Math[:items]=[1]; Math[:items].push(change); Math[:items]; end",
            "[7]",
            false,
        ),
        (
            "def run; Math[:items]=[[1]]; saved=Math; Math[:items][0].push(2); [Math[:items],saved[:items]]; end",
            "[[[1, 2]], [[1]]]",
            false,
        ),
        (
            "def run; Math[:match]='ab'.match('(a)(b)'); begin; Math[:match][:captures][0]='bad'; rescue; Math[:match][:captures]; end; end",
            "[a, b]",
            true,
        ),
    ] {
        witness(source, &[], expected, warnings);
    }
}

#[test]
fn live_resolution_does_not_invent_stale_exception_paths() {
    for source in [
        "enum Status; Draft; end; def echo(x:Math); 7; end; def run; Math=Status; begin; echo(:draft); rescue; 99; end; end",
        "enum Status; Draft; end; def run; Math=Status; begin; [:draft].each {|x:Math| x}; rescue; return 99; end; 7; end",
        "enum Status; Draft; end; def echo -> Math; begin; :draft; ensure; Math=Status; end; end; def run; begin; echo; 7; rescue; 99; end; end",
    ] {
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(report.issues.data.is_empty(), "{source}: {report:?}");
        assert_eq!(report.throws, 0, "{source}: {report:?}");
        assert_eq!(
            report.returns,
            facts.integer(&mut ctx, 7).unwrap(),
            "{source}: {report:?}"
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn definite_type_lookup_failures_have_diagnostics_without_normal_returns() {
    for (source, ambiguous) in [
        (
            "def echo(x:Missing); 7; end; def run; echo(nil); end",
            false,
        ),
        ("def run -> Missing; 7; end", false),
        (
            "enum MATH; Draft; end; enum Review; Draft; end; def echo(x:math); 7; end; def run; Math=Review; echo(:draft); end",
            true,
        ),
    ] {
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert_eq!(report.returns, Atom::Never.fact(), "{source}: {report:?}");
        assert_ne!(report.throws, 0);
        assert!(report.issues.data.iter().any(|located| matches!(located.issue.kind, IssueKind::TypeBinding { ambiguous: actual, .. } if actual == ambiguous)), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn live_resolution_preserves_the_owner_of_source_class_contracts() {
    for body in [
        "def run(x:Widget) -> Widget; x; end",
        "def echo(x:Widget) -> Widget; x; end; def run(x:Widget); echo(x); end",
        "def once(x); yield(x); end; def run(x:Widget); once(x) {|item:Widget| item}; end",
    ] {
        let source = format!("class Widget; end; {body}");
        let program = bytecode::compile(&source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(report.issues.data.is_empty(), "{source}: {report:?}");
        assert_eq!(report.throws, 0, "{source}: {report:?}");
        let expected = facts.nominal(&mut ctx, 42, 0, b"Widget", None).unwrap();
        assert_eq!(report.returns, expected, "{source}: {report:?}");
        assert_ne!(
            report.returns,
            facts.nominal(&mut ctx, 0, 0, b"Widget", None).unwrap()
        );
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn differing_live_identities_and_unexecuted_initializers_stay_explicit() {
    for source in [
        "enum Status; Draft; end; enum Review; Draft; end; def echo(x:Math); x; end; def run(flag:bool); if flag; Math=Status; else; Math=Review; end; echo(:draft); end",
        "enum Status; Draft; end; enum Review; Draft; end; def echo(x:Math.State); x; end; def run(flag:bool); if flag; Math[:State]=Status; else; Math[:State]=Review; end; echo(:draft); end",
        "class Widget; raise('must not run'); end; def echo(x:Widget); x; end; def run; echo(:draft); end",
        "enum Status; Draft; end; def run; Math.store(:State,Status); end",
    ] {
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn uncertain_namespace_provenance_does_not_invent_a_qualified_type() {
    for (source, examples) in [
        (
            "enum Status; Draft; end; def echo(x:Math.State); x.enum.name; end; def run(flag:bool); if flag; Math[:State]=Status; else; Math={State:Status}; end; begin; echo(:draft); rescue; 99; end; end",
            vec![
                (Value::boolean(true), "Status"),
                (Value::boolean(false), "99"),
            ],
        ),
        (
            "enum Status; Draft; end; def run(api:{State:any}); api[:State]=Status; begin; [:draft].map {|x:api.State| x.enum.name}; rescue; 99; end; end",
            vec![(Value::hash(vec![(b"State".to_vec(), Value::nil())]), "99")],
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        for (input, expected) in examples {
            let value = script
                .call("run", &[input], CallOptions::default())
                .unwrap()
                .value;
            assert_eq!(value.to_string(), expected, "{source}");
        }
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, &program).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn accounting_program() -> bytecode::Program {
    bytecode::compile("enum Status; Draft; end; enum Review; Draft; end; def change; Math[:State]=Review; end; def echo(x:Math.State) -> Math.State; begin; :draft; ensure; change; end; end; def run; Math[:State]=Status; results=[:draft,:draft].map {|x:Math.State| before=x.enum.name; change; before}; [results,echo(:draft).enum.name]; end", Vec::new(), &()).unwrap()
}

fn work(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, program)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn live_type_environments_obey_exact_and_interrupted_quotas() {
    let program = accounting_program();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &program).unwrap();
    let stats = ctx.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, kind) in [
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
        assert_eq!(work(&mut ctx, &program).err().map(|e| e.kind), kind);
        if let Some(kind) = kind {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..24 {
        for memory in [false, true] {
            let (limits, kind) = if memory {
                (
                    Limits {
                        memory_bytes: Some(stats.peak_memory_bytes * sample / 24),
                        ..Limits::default()
                    },
                    ErrorKind::Memory,
                )
            } else {
                (
                    Limits {
                        steps: Some(stats.steps * sample as u64 / 24),
                        ..Limits::default()
                    },
                    ErrorKind::Steps,
                )
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(work(&mut ctx, &program).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn live_type_environments_keep_cancellation_and_deadlines_latched() {
    let program = accounting_program();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        drop(analyze(&mut ctx, &mut facts, &program).unwrap());
        let kind = if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
            ErrorKind::Deadline
        } else {
            ctx.cancellation().cancel();
            ErrorKind::Cancelled
        };
        assert_eq!(
            analyze(&mut ctx, &mut facts, &program).unwrap_err().kind,
            kind
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
