use super::{
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value};

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
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

pub(super) fn inferred_runtime(source: &str, args: &[Value], rejected: bool) -> Value {
    let actual = crate::Engine::new()
        .compile(source)
        .unwrap()
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(result.incomplete.data.is_empty(), "{source}: {result:?}");
    assert_eq!(
        !result.issues.data.is_empty(),
        rejected,
        "{source}: {result:?}"
    );
    let concrete = literal_fact(&mut ctx, &mut facts, &actual.value);
    assert_ne!(
        facts.relation(&mut ctx, concrete, result.returns).unwrap(),
        Relation::Rejected,
        "{source}: {result:?}"
    );
    drop((result, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    actual.value
}

fn runtime(source: &str, args: &[Value], expected: &str) {
    let actual = inferred_runtime(source, args, false);
    assert_eq!(actual.to_string(), expected, "{source}");
}

#[test]
fn for_loops_infer_items_and_bindings_without_an_empty_path_for_nonempty_literals() {
    for (source, rejected) in [
        ("def run -> int; for x in [7]; x; end; end", false),
        ("def run -> int; for x in [7,8]; x; end; end", false),
        ("def run -> int; for x in []; x; end; end", true),
        ("def run -> int; for x in [\"bad\"]; x; end; end", true),
        ("def run -> int; for x in [7]; end; x; end", false),
        ("def run -> int; for x in []; end; x; end", true),
        (
            "def run -> string; x=\"kept\"; for x in []; end; x; end",
            false,
        ),
        (
            "def run(xs: array<int>) -> int?; for x in xs; x; end; end",
            false,
        ),
        (
            "def run(xs: array<string>) -> int?; for x in xs; x; end; end",
            true,
        ),
        ("def run; for x in 7; x; end; end", true),
        ("def run; for x in \"abc\"; x; end; end", true),
        ("def run; for x in nil; x; end; end", true),
        ("def run(xs); for x in xs; x; end; end", false),
        (
            "def run(xs, flag: bool); for x in (if flag; xs; else; 7; end); x; end; end",
            true,
        ),
        ("def run; for x in []; x.no_such_method; end; 7; end", false),
    ] {
        check(source, rejected);
    }
}

#[test]
fn for_loop_results_preserve_break_next_return_and_nested_boundaries() {
    for (source, expected) in [
        ("def run; for x in [7]; x; end; end", "7"),
        ("def run; (for x in [7]; x+1; end); end", "[7]"),
        ("def run; for x in [7]; break 9; end; end", "9"),
        ("def run; (for x in [7]; break; end); end", "nil"),
        ("def run; for x in [7]; next 9; end; end", "nil"),
        ("def run; (for x in [7]; next 9; end); end", "[7]"),
        ("def run; for k,v in {a:7}; v; end; end", "7"),
        ("def run; (for pair in {a:7}; break 9; end); end", "9"),
        ("def run; (for pair in {a:7}; break; end); end", "nil"),
        (
            "def run; for x in [7]; for y in [8]; break y; end; x; end; end",
            "7",
        ),
        (
            "def run; for x in [7]; while true; break 9; end; x; end; end",
            "7",
        ),
        (
            "def run -> int; for x in [7]; return x; end; \"unreachable\"; end",
            "7",
        ),
        ("def run; for x in [7]; begin; next 9; end; end; end", "nil"),
        ("def run; for x in [7]; begin; break 9; end; end; end", "9"),
    ] {
        runtime(source, &[], expected);
    }
}

#[test]
fn iteration_uses_value_snapshots_through_source_and_binding_mutations() {
    for (source, expected) in [
        (
            "def run -> array<int>; a=[7]; out=[]; for x in a; a.push(\"bad\"); out.push(x); end; out; end",
            "[7]",
        ),
        (
            "def run -> array<int>; a=[7]; (for x in a; a.clear; end); end",
            "[7]",
        ),
        (
            "def run; a=[[7]]; for x in a; x.push(8); end; a; end",
            "[[7]]",
        ),
        (
            "def run; a=[[7]]; for x in a; a[0].push(8); x; end; end",
            "[7]",
        ),
        (
            "def run; h={a:[7]}; for k,v in h; h[k].push(8); v; end; end",
            "[7]",
        ),
        ("def run; a=[0]; for x in [7]; a[0]=x; end; a; end", "[7]"),
        (
            "def run; a=[0]; a[0]=(for x in [7]; break x; end); a; end",
            "[7]",
        ),
        (
            "def id(x); x; end; def run; id(for x in [7]; break x; end); end",
            "7",
        ),
    ] {
        runtime(source, &[], expected);
    }
}

#[test]
fn destructuring_in_loops_and_assignments_keeps_positions_rest_and_nil_padding() {
    for (source, expected) in [
        ("def run; for a,b in [[7,8]]; [a,b]; end; end", "[7, 8]"),
        ("def run; for a,b in [7]; [a,b]; end; end", "[7, nil]"),
        (
            "def run; for a,*b,c in [[7,8,9,10]]; [a,b,c]; end; end",
            "[7, [8, 9], 10]",
        ),
        (
            "def run; for a,*b,c in [[7]]; [a,b,c]; end; end",
            "[7, [], nil]",
        ),
        (
            "def run; for (a,b),c in [[[7,8],9]]; [a,b,c]; end; end",
            "[7, 8, 9]",
        ),
        ("def run; a,*b,c=[7,8,9]; [a,b,c]; end", "[7, [8], 9]"),
        ("def run; a,*b,c=7; [a,b,c]; end", "[7, [], nil]"),
        ("def run; a,b={x:7}; [a,b]; end", "[{x: 7}, nil]"),
        (
            "def pair(a: int,b: string); [a,b]; end; def run; for a,b in [[7,\"ok\"]]; pair(a,b); end; end",
            "[7, ok]",
        ),
    ] {
        runtime(source, &[], expected);
    }
    for source in [
        "def integer(a: int); a; end; def run; for a in [\"bad\"]; integer(a); end; end",
        "def pair(a: int,b: string); b; end; def run; for a,b in [[7,8]]; pair(a,b); end; end",
        "def run -> int; a,b=[7]; b; end",
    ] {
        check(source, true);
    }
}

#[test]
fn iterable_loops_keep_unsupported_reachable_paths_visible() {
    for source in [
        "def run; for x in [7]; x.no_such_method; end; end",
        "def run(h: hash<string,int>); for k,v in h; v; end; end",
        "def run; for x in [7]; begin; x; ensure; [1].group_by { _1.to_s }; end; end; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let result = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!result.incomplete.data.is_empty(), "{source}: {result:?}");
        drop((result, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn loop_rebinding_reports_type_changes_without_changing_the_source_snapshot() {
    let source = "def run; a=[7]; b=a; for a in a; a+1; end; [a,b]; end";
    let actual = inferred_runtime(source, &[], true);
    assert_eq!(actual.to_string(), "[7, [7]]");
}

#[test]
fn bounded_ranges_keep_integer_items_at_descending_and_integer_endpoints() {
    for (range, expected) in [
        ("1..3", "[1, 2, 3]"),
        ("3..1", "[3, 2, 1]"),
        ("1...3", "[1, 2]"),
        ("3...1", "[3, 2]"),
        ("7...7", "[]"),
        (
            "9223372036854775807..9223372036854775807",
            "[9223372036854775807]",
        ),
        (
            "(-9223372036854775808)..(-9223372036854775808)",
            "[-9223372036854775808]",
        ),
    ] {
        let source =
            format!("def run -> array<int>; out=[]; for x in {range}; out.push(x); end; out; end");
        runtime(&source, &[], expected);
    }
    runtime(
        "def run -> range; (for x in 1..3; x; end); end",
        &[],
        "1..3",
    );
    check("def run -> string?; for x in 1..3; x; end; end", true);
}

#[test]
fn destructuring_facts_cover_runtime_values_and_generalized_array_lengths() {
    let sources = [
        "nil",
        "7",
        "\"raw\"",
        "{x:7}",
        "[]",
        "[7]",
        "[7,8]",
        "[7,8,9]",
        "[7,[8,9],10,11]",
    ];
    let targets = [
        ("a,b", "[a,b]"),
        ("a,*b", "[a,b]"),
        ("*a,b", "[a,b]"),
        ("a,*b,c", "[a,b,c]"),
        ("a,b,*c,d,e", "[a,b,c,d,e]"),
        ("a,(b,c)", "[a,b,c]"),
        ("a,(b,*c),d", "[a,b,c,d]"),
        ("a,*,b", "[a,b]"),
    ];
    let mut cases = 0;
    for value in sources {
        for (target, result) in targets {
            for binding in [
                format!("{target}={value}; {result}"),
                format!("for {target} in [{value}]; {result}; end"),
            ] {
                inferred_runtime(&format!("def run; {binding}; end"), &[], false);
                cases += 1;
            }
        }
    }
    for length in 0..7 {
        let value = Value::array((0..length).map(Value::int).collect());
        for (target, result) in targets {
            inferred_runtime(
                &format!("def run(xs: array<int>); {target}=xs; {result}; end"),
                std::slice::from_ref(&value),
                false,
            );
            cases += 1;
        }
    }
    assert_eq!(cases, 200);
}

#[test]
fn loop_branches_mutations_and_control_results_contain_runtime_outcomes() {
    let mut cases = 0;
    for collection in [
        "[]",
        "[7]",
        "[7,8]",
        "{}",
        "{a:7}",
        "{z:7,a:8}",
        "1..3",
        "3...1",
    ] {
        for body in [
            "x",
            "next",
            "next 9",
            "break",
            "break 9",
            "if flag; break 9; else; next; end",
        ] {
            for expression in [false, true] {
                let loop_source = format!("for x in {collection}; {body}; end");
                let source = format!(
                    "def run(flag: bool); {}; end",
                    if expression {
                        format!("({loop_source})")
                    } else {
                        loop_source
                    }
                );
                for flag in [false, true] {
                    inferred_runtime(&source, &[Value::boolean(flag)], false);
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 192);
}

#[test]
fn optional_hash_entries_and_growing_nested_results_converge() {
    let optional = "def run(flag: bool) -> int?; h={a:7}; if flag; h.delete(:a); end; for k,v in h; v; end; end";
    runtime(optional, &[Value::boolean(false)], "7");
    runtime(optional, &[Value::boolean(true)], "nil");
    runtime(
        "def run(key: string) -> int?; h={}; h[key]=7; for k,v in h; v; end; end",
        &[Value::bytes(b"a".to_vec())],
        "7",
    );
    for (source, expected) in [
        (
            "def run -> array; out=[]; for x in [1,2]; out=[out]; end; out; end",
            "[[[]]]",
        ),
        (
            "def run -> hash; out={}; for x in [1,2]; out={child:out}; end; out; end",
            "{child: {child: {}}}",
        ),
        (
            "def run -> array<int>; a=[7,8]; b=[]; for x in a; a.clear; b.push(x); end; b; end",
            "[7, 8]",
        ),
        (
            "def run -> array<int>; h={z:7,a:8}; out=[]; for k,v in h; h.clear; out.push(v); end; out; end",
            "[7, 8]",
        ),
        ("def run; a=7; for a,a in [[8]]; end; a; end", "nil"),
        ("def run; a=[0]; a[0],b=[7,8]; [a,b]; end", "[[7], 8]"),
    ] {
        runtime(source, &[], expected);
    }
    for (source, rejected) in [
        (
            "def run -> array<int>; out=[]; for x in [1,2]; out.push(\"bad\"); end; out; end",
            true,
        ),
        (
            "def run -> int; for x in {}; x.no_such_method; end; 7; end",
            false,
        ),
        (
            "def run -> string; for x in [7]; return x; end; \"unreachable\"; end",
            true,
        ),
        ("def run -> int; for k,v in {z:7,a:8}; v; end; end", false),
        (
            "def run -> string; for k,v in {z:7,a:8}; k; end; end",
            false,
        ),
        (
            "def run -> int; for k,v in {z:7,a:\"bad\"}; v; end; end",
            true,
        ),
    ] {
        check(source, rejected);
    }
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run -> array<int>; out=[]; for a,*b,c in [[7,8,9],[10,11,12]]; for x in b; out.push(a+x+c); end; end; out; end";
    let result = analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty(), "{result:?}");
    Ok(())
}

#[test]
fn iterable_snapshots_and_extraction_obey_quotas_and_release_failed_analysis() {
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
fn iterable_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn iteration_reference_decisions_and_syntax_differences_keep_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-iteration.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    let syntax = fixture["syntax_differences"].as_array().unwrap();
    assert_eq!(cases.len(), 49);
    assert_eq!(syntax.len(), 1);
    let mut differences = 0;
    for case in cases.iter().chain(syntax) {
        let source = case["source"].as_str().unwrap();
        check(source, case["rust_rejected"].as_bool().unwrap());
        if let Some(go) = case["go_rejected"].as_bool() {
            if go != case["rust_rejected"].as_bool().unwrap() {
                differences += 1;
                assert!(!case["difference"].as_str().unwrap().is_empty());
            }
        } else {
            assert_eq!(case["go_status"], "compile_error");
            assert!(!case["go_compile_error"].as_str().unwrap().is_empty());
            assert!(!case["difference"].as_str().unwrap().is_empty());
        }
        let args = crate::parse_json(
            &serde_json::to_vec(&case["runtime"]["args"]).unwrap(),
            CallOptions::default(),
        )
        .unwrap();
        let actual = crate::Engine::new().compile(source).unwrap().call(
            "run",
            args.value.as_array().unwrap(),
            CallOptions::default(),
        );
        if let Some(error) = case["runtime"]["error"].as_str() {
            assert_eq!(error, "type");
            assert_eq!(actual.unwrap_err().kind, ErrorKind::Type, "{source}");
        } else {
            let json =
                crate::stringify_json(&actual.unwrap().value, CallOptions::default()).unwrap();
            let value: serde_json::Value =
                serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap();
            assert_eq!(value, case["runtime"]["value_json"], "{source}");
        }
    }
    assert_eq!(differences, 22);
}
