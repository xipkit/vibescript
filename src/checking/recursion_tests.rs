use super::{
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result};

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result =
        analyze(&mut ctx, &mut facts, source).unwrap_or_else(|error| panic!("{source}: {error}"));
    assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
    assert_eq!(
        !result.issues.data.is_empty(),
        rejected,
        "{source}: {result:?}"
    );
    assert!(
        result.contexts <= 8,
        "{source}: {} contexts",
        result.contexts
    );
    if !rejected {
        let actual =
            crate::Engine::new()
                .compile(source)
                .unwrap()
                .call("run", &[], CallOptions::default());
        match actual {
            Ok(actual) => {
                let actual = literal_fact(&mut ctx, &mut facts, &actual.value);
                assert_ne!(
                    facts.relation(&mut ctx, actual, result.returns).unwrap(),
                    Relation::Rejected,
                    "{source}"
                );
            }
            Err(error) => {
                assert_eq!(error.kind, ErrorKind::Recursion, "{source}: {error}");
                assert_eq!(result.returns, super::facts::Atom::Never.fact(), "{source}");
            }
        }
    }
    drop((result, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn recursive_collection_arguments_converge() {
    for (source, rejected) in [
        (
            "def grow(a,n: int); if n>0; grow(a.push(1),n-1); else; a; end; end; def run -> array<int>; grow([],3); end",
            false,
        ),
        (
            "def grow(a,n: int); if n>0; grow(a.push(\"bad\"),n-1); else; a; end; end; def run -> array<int>; grow([],3); end",
            true,
        ),
        (
            "def grow(a,n: int); if n>0; grow([a],n-1); else; a; end; end; def run -> array; grow([],3); end",
            false,
        ),
        (
            "def grow(h,n: int); if n>0; grow({child:h},n-1); else; h; end; end; def run -> hash; grow({},3); end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn recursive_collection_returns_converge() {
    for source in [
        "def grow(n: int); if n==0; []; else; [grow(n-1)]; end; end; def run -> array; grow(3); end",
        "def grow(n: int); if n==0; {}; else; {child:grow(n-1)}; end; end; def run -> hash; grow(3); end",
    ] {
        check(source, false);
    }
}

#[test]
fn mutually_recursive_collection_inputs_and_results_converge() {
    for source in [
        "def a(xs,n: int); if n>0; b(xs.push(1),n-1); else; xs; end; end; def b(xs,n: int); a(xs.push(2),n); end; def run -> array<int>; a([],3); end",
        "def a(n: int); if n==0; []; else; [b(n-1)]; end; end; def b(n: int); [a(n)]; end; def run -> array; a(3); end",
    ] {
        check(source, false);
    }
}

#[test]
fn shared_cached_contexts_close_recursive_return_cycles() {
    let source = "def a(n: int); if n>0; [b(n-1)]; else; []; end; end; def b(n: int); if n>0; [a(n-1)]; else; []; end; end; def run(n: int,choose: bool) -> array; if choose; a(n); else; b(n); end; end";
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.issues.data.is_empty(), "{result:?}");
    assert!(result.incomplete.data.is_empty());
    assert_eq!(result.contexts, 3);
    let script = crate::Engine::new().compile(source).unwrap();
    for choose in [false, true] {
        let actual = script
            .call(
                "run",
                &[crate::Value::int(3), crate::Value::boolean(choose)],
                CallOptions::default(),
            )
            .unwrap();
        let actual = literal_fact(&mut ctx, &mut facts, &actual.value);
        assert_ne!(
            facts.relation(&mut ctx, actual, result.returns).unwrap(),
            Relation::Rejected
        );
    }
    drop((result, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn long_recursive_call_paths_use_metered_iterative_graph_search() {
    let mut source = String::from(
        "def run -> array<int>; f0([],1); end\ndef f0(xs,n: int); if n>0; f1(xs,n); else; xs; end; end\n",
    );
    for i in 1..200 {
        let body = if i == 199 {
            "f0(xs.push(7),n-1)".into()
        } else {
            format!("f{}(xs,n)", i + 1)
        };
        source.push_str(&format!("def f{i}(xs,n: int); {body}; end\n"));
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, &source).unwrap();
    assert!(result.issues.data.is_empty(), "{result:?}");
    assert!(result.incomplete.data.is_empty());
    assert_eq!(result.contexts, 201);
    let actual = crate::Engine::new()
        .compile(&source)
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap();
    assert_eq!(actual.value.to_string(), "[7]");
    let actual = literal_fact(&mut ctx, &mut facts, &actual.value);
    assert_ne!(
        facts.relation(&mut ctx, actual, result.returns).unwrap(),
        Relation::Rejected
    );
    drop((result, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn recursive_defaults_keywords_and_rest_keep_binding_contracts() {
    for (source, rejected) in [
        (
            "def f(a=[7], n: 0); if n>0; f(n:n-1); else; a; end; end; def run -> array<int>; f([8],n:3); end",
            false,
        ),
        (
            "def f(a=[\"bad\"], n: 0); if n>0; f(n:n-1); else; a; end; end; def run -> array<int>; f([8],n:3); end",
            true,
        ),
        (
            "def f(a: array<int> = [\"bad\"], n: int=0); if n>0; f(a.push(7),n-1); else; a; end; end; def run -> array<int>; f([8],3); end",
            false,
        ),
        (
            "def f(n: int, *xs); if n>0; f(n-1,xs); else; xs; end; end; def run -> array; f(3,7); end",
            false,
        ),
        (
            "def f(n: int, **kw); if n>0; f(n-1,child:kw); else; kw; end; end; def run -> hash; f(3,value:7); end",
            false,
        ),
        (
            "def f(n: int, value: array:); if n>0; f(n-1,value:value.push(7)); else; value; end; end; def run -> array<int>; f(3,value:[]); end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn sibling_calls_keep_separate_specializations_after_recursive_widening() {
    for source in [
        "def id(x); x; end; def run -> int; id(\"bad\"); id(7); end",
        "def first(x); x[0]; end; def run -> int; first([\"bad\"]); first([7]); end",
        "def f(n: int,x); if n>0; f(n-1,x); else; x; end; end; def run -> int; f(2,\"bad\"); f(2,7); end",
        "def a(n: int,x); if n>0; b(n-1,x); else; x; end; end; def b(n: int,x); if n>0; a(n-1,x); else; x; end; end; def run -> int; a(2,\"bad\"); b(3,7); end",
    ] {
        check(source, false);
    }
}

#[test]
fn recursive_returns_keep_known_bad_arms_and_unreachable_tails_separate() {
    for source in [
        "def f(n: int); if n>0; [f(n-1)]; else; [\"bad\"]; end; end; def run -> array<array<int>>; f(2); end",
        "def f(xs,n: int); if n>0; f(xs.push(\"bad\"),n-1); else; xs; end; end; def run(x) -> array<int>; f([x],2); end",
        "def f(n: int); if n>0; {child:f(n-1),value:\"bad\"}; else; {}; end; end; def run -> hash<string,int>; f(2); end",
    ] {
        check(source, true);
    }
    for source in [
        "def f(a); f([a]); end; def run -> int; f([]); \"unreachable\"; end",
        "def a(xs); b(xs.push(7)); end; def b(xs); a(xs); end; def run -> int; a([]); \"unreachable\"; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
        assert!(result.issues.data.is_empty(), "{source}: {result:?}");
        assert_eq!(result.returns, super::facts::Atom::Never.fact());
        drop((result, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(
            crate::Engine::new()
                .compile(source)
                .unwrap()
                .call("run", &[], CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Recursion
        );
    }
}

#[test]
fn recursive_unmodeled_paths_stay_incomplete_after_return_widening() {
    for source in [
        "def f(xs,n: int); if n>0; f(xs.push(7),n-1); else; xs.group_by { _1.to_s }; end; end; def run -> int; f([],3); 7; end",
        "def f(n: int); if n>0; [f(n-1)]; else; [1].group_by { _1.to_s }; end; end; def run -> array; f(3); end",
        "def f(n: int); if n>1; [f(n-1)]; elsif n>0; [1].group_by { _1.to_s }; else; []; end; end; def run -> array; f(3); end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!result.incomplete.data.is_empty(), "{source}: {result:?}");
        drop((result, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def f(a,n: int); if n>0; [f(a.push(1),n-1)]; else; a; end; end; def run -> array; f([],2); end";
    let result = analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
    Ok(())
}

#[test]
fn recursive_graph_and_widening_storage_obey_exact_quotas_and_cleanup() {
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
        assert_eq!(result.as_ref().err().map(|error| error.kind), error);
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
fn recursive_analysis_observes_cancellation_and_expired_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        accounting(&mut ctx).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        assert_eq!(
            accounting(&mut ctx).unwrap_err().kind,
            if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            }
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn recursive_reference_decisions_and_unresolved_cases_keep_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-recursion.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    let unresolved = fixture["unresolved_reference"].as_array().unwrap();
    assert_eq!(cases.len(), 21);
    assert_eq!(unresolved.len(), 2);
    let mut differences = 0;
    for case in cases.iter().chain(unresolved) {
        let source = case["source"].as_str().unwrap();
        check(source, case["rust_rejected"].as_bool().unwrap());
        if let Some(go) = case["go_rejected"].as_bool() {
            if go == case["rust_rejected"].as_bool().unwrap() {
                continue;
            }
            differences += 1;
            assert!(!case["difference"].as_str().unwrap().is_empty());
        } else {
            assert_eq!(case["go_status"], "timeout");
            assert!(!case["reason"].as_str().unwrap().is_empty());
        }
        let args: Vec<_> = case["runtime"]["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| crate::Value::int(value.as_i64().unwrap()))
            .collect();
        let actual = crate::Engine::new().compile(source).unwrap().call(
            "run",
            &args,
            CallOptions::default(),
        );
        if let Some(error) = case["runtime"]["error"].as_str() {
            assert_eq!(
                actual.unwrap_err().kind,
                match error {
                    "type" => ErrorKind::Type,
                    "recursion" => ErrorKind::Recursion,
                    _ => panic!("unknown runtime expectation"),
                },
                "{source}"
            );
        } else {
            let json =
                crate::stringify_json(&actual.unwrap().value, CallOptions::default()).unwrap();
            let value: serde_json::Value =
                serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap();
            assert_eq!(value, case["runtime"]["value_json"], "{source}");
        }
    }
    assert_eq!(differences, 6);
}
