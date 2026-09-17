use super::{
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value};

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result =
        analyze(&mut ctx, &mut facts, source).unwrap_or_else(|error| panic!("{source}: {error}"));
    if !result.incomplete.data.is_empty() {
        let program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
        let pending: Vec<_> = result
            .incomplete
            .data
            .iter()
            .map(|&(function, pc)| (pc, program.functions[function].code[pc]))
            .collect();
        panic!("{source}: {result:?}; unsupported instructions: {pending:?}");
    }
    assert_eq!(
        !result.issues.data.is_empty(),
        rejected,
        "{source}: {result:?}"
    );
    drop((result, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn addressed_mutations_check_receivers_results_and_value_snapshots() {
    for (source, rejected) in [
        ("def run; [1, 2].push(3); end", false),
        ("def run; [7].push(8); end", false),
        ("def run; a=[7]; a[0]=8; a; end", false),
        ("def run(*args); args.push(1); end", false),
        ("def run -> array<int>; a=[1]; a.push(2); a; end", false),
        ("def run -> array<string>; a=[1]; a.push(2); a; end", true),
        (
            "def run -> array<int>; a=[1]; b=a; a.push(\"bad\"); b; end",
            false,
        ),
        ("def run -> int; a=[1]; a.push(2); a[1]; end", false),
        ("def run -> int; a=[1]; a << 2; a[1]; end", false),
        ("def run -> int; a=[1,2]; a.pop; end", false),
        ("def run -> array<int>; a=[1,2]; a.pop(1); end", false),
        ("def run -> array<int>; a=[1,2]; a.pop; a; end", false),
        (
            "def run -> array<int>; a=[1,2]; a.unshift(0); a.append(3); a; end",
            false,
        ),
        (
            "def run -> string; a=[1]; a[0]=\"changed\"; a[0]; end",
            false,
        ),
        ("def run -> int; a=[1]; a[-1]+=2; a[0]; end", false),
        ("def run -> int; a=[nil]; a[0] ||= 7; a[0]; end", false),
        ("def run -> int; a=[7]; a[0] ||= \"bad\"; a[0]; end", false),
        (
            "def run -> array<int>; a=[1]; a.push(*[2,3]); a; end",
            false,
        ),
        (
            "def run -> array<int>; a=[1]; a.push(*[\"bad\"]); a; end",
            true,
        ),
        (
            "def run -> string; a=\"old\"; a.replace(\"new\"); a; end",
            false,
        ),
        ("def run -> string; a=\"old\"; a.clear; end", false),
        (
            "def run -> bool; a=[1]; a.clear.push(2); a.empty?; end",
            false,
        ),
        ("def run; a=[1]; a[2]=7; end", true),
        ("def run; a=[1]; a[0,1]=7; end", true),
        ("def run; a=[1]; a.pop(\"bad\"); end", true),
        ("def run; a=7; a.push(1); end", true),
    ] {
        check(source, rejected);
    }
}

#[test]
fn pending_address_facts_include_runtime_results_through_parent_mutations() {
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
                    "def run; a={root}; b=a; result=a[{selected}].push(begin; {mutation}; 7; end); [a,b,result]; end"
                );
                runtime_fact(&engine, &source, &[]);
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
                "def run; a={{x:[1],y:[1]}}; b=a; result=a.{selected}.push(begin; {mutation}; 7; end); [a,b,result]; end"
            );
            runtime_fact(&engine, &source, &[]);
            cases += 1;
        }
    }
    assert_eq!(cases, 268);
}

fn runtime_fact(engine: &crate::Engine, source: &str, args: &[Value]) {
    let actual = engine
        .compile(source)
        .unwrap()
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
    assert!(result.issues.data.is_empty(), "{source}: {result:?}");
    let concrete = literal_fact(&mut ctx, &mut facts, &actual.value);
    assert_ne!(
        facts.relation(&mut ctx, concrete, result.returns).unwrap(),
        Relation::Rejected,
        "{source}: runtime {}, inferred {:?}",
        actual.value,
        facts.node(result.returns)
    );
    drop((facts, result));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn plain_begin_preserves_binding_timing_call_targets_and_outer_addresses() {
    for (source, rejected) in [
        (
            "def run -> int; begin; x=7; if false; y=8; end; x; end; end",
            false,
        ),
        (
            "def run -> nil; begin; if false; x=7; end; end; x; end",
            false,
        ),
        (
            "def run -> int; a=[1]; a.push(begin; begin; a.push(2); end; 3; end); a[-1]; end",
            false,
        ),
        (
            "def run -> int; a=[1]; a.push(begin; return 7; end); 99; end",
            false,
        ),
        ("def run; (begin; [1]; end)(2); end", true),
        ("def run; (begin; {x:1}; end)(2); end", true),
        ("def f; 7; end; def run; (begin; f; end)(2); end", true),
        (
            "def run -> nil; a=[nil]; (begin; a[0]=a; end).clear; a[0][0]; end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn address_branches_and_assignment_results_keep_runtime_value_boundaries() {
    let engine = crate::Engine::new();
    for source in [
        "def run; a=[nil]; result=(begin; a[0]=a; end).push(2); [a,result]; end",
        "def run; a=[1]; a.push(begin; a=a.push(2); 3; end); a; end",
        "def run(flag: bool); a=[[1]]; result=a[0].push(begin; if flag; a.push([2]); else; a[0]=[3]; end; 7; end); [a,result]; end",
        "def run(flag: bool); a=[[1],[2]]; result=a[-1].push(begin; if flag; a.pop; else; a[0].push(3); end; 7; end); [a,result]; end",
        "def run(flag: bool); a=[[1]]; result=a[0].push(begin; if flag; a=[[1]]; end; 7; end); [a,result]; end",
        "def run(flag: bool); a=[[1],[2]]; result=a[if flag; 0; else; 1; end].push(begin; a[0]=[3]; 7; end); [a,result]; end",
        "def run(flag: bool); a=[1]; result=a.push(begin; while flag; a.push(2); break; end; 3; end); [a,result]; end",
    ] {
        if source.contains("flag: bool") {
            for flag in [false, true] {
                runtime_fact(&engine, source, &[Value::boolean(flag)]);
            }
        } else {
            runtime_fact(&engine, source, &[]);
        }
    }
}

#[test]
fn addressed_flow_reference_differences_have_runtime_witnesses() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-addresses.json")).unwrap();
    let cases = fixtures["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 66);
    let mut differences = 0;
    let engine = crate::Engine::new();
    for case in cases {
        let source = case["source"].as_str().unwrap();
        check(source, case["rust_rejected"].as_bool().unwrap());
        if case["go_rejected"] == case["rust_rejected"] {
            continue;
        }
        differences += 1;
        assert!(!case["difference"].as_str().unwrap().is_empty());
        let args: Vec<_> = case["runtime"]["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| Value::int(value.as_i64().unwrap()))
            .collect();
        let actual = engine
            .compile(source)
            .unwrap()
            .call("run", &args, CallOptions::default());
        if let Some(value) = case["runtime"]["value"].as_str() {
            assert_eq!(actual.unwrap().value.to_string(), value, "{source}");
        } else {
            let kind = match case["runtime"]["error"].as_str().unwrap() {
                "type" => ErrorKind::Type,
                "argument" => ErrorKind::Argument,
                "name" => ErrorKind::Name,
                _ => panic!("unknown runtime expectation"),
            };
            assert_eq!(actual.unwrap_err().kind, kind, "{source}");
        }
    }
    assert_eq!(differences, 17);
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(n: int, flag: bool) -> array; a=[[1]]; a[0].push(begin; if flag; a.push([2]); else; a[0]=[3]; end; 7; end); while n>0; a.push(a); n-=1; end; a; end";
    let result = analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
    Ok(())
}

#[test]
fn addressed_flow_honors_exact_limits_and_reclaims_pending_state() {
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
    for memory in (0..stats.peak_memory_bytes).step_by(509) {
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
fn pending_writes_follow_selected_positions_and_detach_fresh_replacements() {
    for (source, expected) in [
        (
            "def run -> int; a=[[1]]; a[-1].push(begin; a.push([9]); 2; end); a[0][1]; end",
            "2",
        ),
        (
            "def run -> int; a=[1]; a[-1]+=begin; a.push(9); 2; end; a[0]; end",
            "3",
        ),
        (
            "def run -> string; a=[1]; a.push(begin; a=[\"kept\"]; 2; end); a[0]; end",
            "kept",
        ),
        (
            "def run -> string; a=[[1]]; a[0].push(begin; a[0]=[\"kept\"]; 2; end); a[0][0]; end",
            "kept",
        ),
        (
            "def run -> bool; a=[[1]]; a[0].push(begin; a.clear; 2; end); a.empty?; end",
            "true",
        ),
        (
            "def run -> int; a=[[1],[2]]; a[0].push(begin; a.pop; 3; end); a[0][1]; end",
            "3",
        ),
        (
            "def run -> int; a=[[1],[2]]; a[1].push(begin; a.pop; 3; end); a.size; end",
            "1",
        ),
        (
            "def run -> int; a=[[1]]; a.first.push(\"bad\"); a[0][0]; end",
            "1",
        ),
        (
            "def run -> int; a=[[1]]; a[0,1].push(\"bad\"); a[0][0]; end",
            "1",
        ),
        (
            "def run -> array<int>; a=[1]; a.push(a.push(2)[-1]); a; end",
            "[1, 2, 2]",
        ),
    ] {
        check(source, false);
        let result = crate::Engine::new()
            .compile(source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap();
        assert_eq!(result.value.to_string(), expected, "{source}");
    }
}

#[test]
fn hash_fields_and_builtin_names_follow_address_dispatch() {
    for (source, rejected) in [
        (
            "def run -> int; h={items:[1]}; h.items.push(2); h.items[1]; end",
            false,
        ),
        (
            "def run -> string; h={items:[1]}; h.items[0]=\"bad\"; h.items[0]; end",
            false,
        ),
        (
            "def run -> int; h={keys:[1]}; h.keys.push(2); h[:keys][1]; end",
            false,
        ),
        ("def run -> int; h={x:1}; h.x+=2; h.x; end", false),
        ("def run -> string; h={}; h.x=\"new\"; h.x; end", false),
        ("def run -> int; h={}; h.store(:x,7); h.x; end", false),
        ("def run -> int; h={x:7}; h.delete(:x); end", false),
        (
            "def run -> int; h={push:[7]}; x=h.push; x.push(\"bad\"); h[:push][0]; end",
            false,
        ),
        ("def run; h={push:[7]}; h.push(1); end", true),
        ("def run; h={}; h.missing.push(7); end", true),
        (
            "def run -> array<int>; h={x:1}; h.replace({x:[7]}); h.x; end",
            false,
        ),
        (
            "def run -> string; h={x:[1]}; h.x.push(begin; h.store(:x,[\"kept\"]); 2; end); h.x[0]; end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn mutation_branches_safe_navigation_and_loop_cleanup_converge() {
    for (source, rejected) in [
        ("def run -> int; a=nil; a&.push(\"bad\"); 7; end", false),
        ("def run -> int; a=[1]; a&.push(2); a[1]; end", false),
        (
            "def run -> int; a={items:nil}; a.items&.push(\"bad\"); 7; end",
            false,
        ),
        (
            "def run -> array<int>; a=[]; n=2; while n>0; a.push(7); n-=1; end; a; end",
            false,
        ),
        (
            "def run -> array<int>; a=[]; n=2; while n>0; a.push(\"bad\"); n-=1; end; a; end",
            true,
        ),
        (
            "def run -> array; a=[]; n=2; while n>0; a.push(a); n-=1; end; a; end",
            false,
        ),
        (
            "def run -> array; a=[1]; n=2; a.push(while n>0; a.push(2); n-=1; end); a; end",
            false,
        ),
        (
            "def run -> array<int>; a=[1]; n=2; while n>0; a.push(begin; n=0; break; end); end; a; end",
            false,
        ),
        (
            "def run -> array<int>; a=[1]; n=2; while n>0; a.push(begin; n-=1; next; end); end; a; end",
            false,
        ),
        (
            "def make; [1]; end; def run -> array<int>; make.push(2); end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}
