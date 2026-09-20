use super::{
    facts::{Atom, Fact, Facts},
    flow::{self, IssueKind, Report},
    graph::{Exit, Graph},
    slots::Slots,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, budget::Buffer, bytecode};

fn contracts(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
) -> Result<Buffer<Fact>> {
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(ctx, ty, |_, _| Ok(None))?;
        contracts.push(ctx, fact)?;
    }
    Ok(contracts)
}

#[test]
fn equality_receiver_uncertainty_does_not_become_native_identity() {
    use super::facts::Node;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let class = facts.type_value(&mut ctx, Atom::Int.fact()).unwrap();
    let instance = facts.instance(&mut ctx, class, 0).unwrap();
    let choice = facts
        .choice(&mut ctx, &[instance, Atom::Any.fact()])
        .unwrap();
    assert!(matches!(facts.node(choice), Node::Choice(_)));
    let enumeration = crate::Engine::new()
        .compile("enum E; A; end; E")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let enumeration = facts.enumeration(&mut ctx, &enumeration).unwrap();
    let member = facts.enum_member(&mut ctx, enumeration, 0).unwrap();
    for op in ["==", "!="] {
        for known in [enumeration, member] {
            let (result, guarded) = facts.scalar_binary(&mut ctx, op, choice, known).unwrap();
            assert!(result.unsupported && !result.rejected && !guarded);
            assert_eq!(result.value, Atom::Never.fact());
        }
        for known in [instance, class] {
            for opaque in [Atom::Unknown.fact(), Atom::Any.fact()] {
                let (left, guarded) = facts.scalar_binary(&mut ctx, op, opaque, known).unwrap();
                assert!(!left.unsupported && !left.rejected && !guarded);
                assert_eq!(left.value, Atom::Unknown.fact());
                let (right, guarded) = facts.scalar_binary(&mut ctx, op, known, opaque).unwrap();
                assert!(!right.unsupported && !right.rejected && !guarded);
                assert_eq!(right.value, Atom::Bool.fact());
            }
            let (left, _) = facts.scalar_binary(&mut ctx, op, choice, known).unwrap();
            assert!(left.unsupported);
            let (right, _) = facts.scalar_binary(&mut ctx, op, known, choice).unwrap();
            assert!(!right.unsupported && !right.rejected);
            assert_eq!(right.value, Atom::Bool.fact());
            for (left, right) in [(known, Atom::Never.fact()), (Atom::Never.fact(), known)] {
                let (result, guarded) = facts.scalar_binary(&mut ctx, op, left, right).unwrap();
                assert_eq!(result.value, Atom::Never.fact());
                assert!(!guarded);
            }
        }
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn analyze(ctx: &mut CallContext, facts: &mut Facts, source: &str) -> Result<Report> {
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let contracts = contracts(ctx, facts, &program)?;
    flow::analyze(ctx, facts, &program, program.names["run"], &contracts.data)
}

fn check(source: &str, rejected: usize) -> Option<Atom> {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(report.issues.data.len(), rejected, "{source}: {report:?}");
    facts.atom(report.returns)
}

#[test]
fn scalar_flow_checks_returns_and_independent_branch_assignments() {
    for (source, rejected) in [
        ("def run(x: int) -> int; x + 2; end", 0),
        ("def run(x: int) -> string; x + 2; end", 1),
        (
            "def run(flag: bool) -> int; if flag; x = 1; else; x = 2; end; x; end",
            0,
        ),
        (
            "def run(flag: bool) -> int; if flag; x = 1; else; x = \"x\"; end; x; end",
            1,
        ),
        ("def run(flag: bool) -> int; if flag; x = 1; end; x; end", 1),
        (
            "def run(flag: bool) -> int?; if flag; x = 1; end; x; end",
            0,
        ),
        (
            "def run(flag: bool) -> int; if flag; return 1; else; return \"x\"; end; end",
            1,
        ),
        (
            "def run(flag: bool) -> int; if flag; return 1; end; 2; end",
            0,
        ),
        ("def run(x: int | any) -> int; x; end", 0),
        ("def run(x: string | any) -> int; x; end", 1),
    ] {
        check(source, rejected);
    }
}

#[test]
fn unreachable_scalar_branches_and_statements_produce_no_issues() {
    for source in [
        "def run -> int; if false; \"x\"; else; 7; end; end",
        "def run -> int; if nil; \"x\"; else; 7; end; end",
        "def run -> int; if 0; 7; else; \"x\"; end; end",
        "def run -> int; if \"\"; 7; else; \"x\"; end; end",
        "def run -> int; return 7; 1 - \"x\"; end",
        "def run -> int; if false; [1].push(2); end; 7; end",
    ] {
        assert_eq!(check(source, 0), Some(Atom::Int));
    }
}

#[test]
fn scalar_reassignment_keeps_nil_neutral_and_allows_numeric_changes() {
    for (source, rejected) in [
        ("def run; x = nil; x = 1; x = nil; end", 0),
        ("def run; x = 1; x = 2.5; end", 0),
        ("def run; x = 1; x = \"text\"; end", 1),
        ("def run(x); x = 1; end", 0),
        ("def run(x: int | string); x = 1; end", 0),
        ("def run(x: int | string); x = true; end", 1),
    ] {
        check(source, rejected);
    }
}

#[test]
fn nil_guards_narrow_both_branches_and_early_returns() {
    for source in [
        "def run(x: int?) -> int; if x == nil; 0; else; x; end; end",
        "def run(x: int?) -> int; if nil == x; return 0; end; x; end",
        "def run(x: int?) -> int; if x != nil; x; else; 0; end; end",
        "def run(x: int?) -> int; if x.nil?; return 0; end; x; end",
        "def run(x: int?) -> int; if x.nil?(); return 0; end; x; end",
        "def run(x: int?) -> int; unless !x.nil?; return 0; end; x; end",
        "def run(x: int?) -> int; x || 7; end",
        "def run(x: int?) -> int?; x && (x + 1); end",
        "def run(x: int?) -> int; x ? x : 0; end",
        "def run(x: int?) -> int; if x; x + 1; else; 0; end; end",
        "def run(x: bool?) -> bool; if x; x; else; false; end; end",
        "def run(x: int?) -> bool?; x&.nil?; end",
    ] {
        check(source, 0);
    }
}

#[test]
fn assignments_in_conditions_invalidate_older_predicates() {
    for (source, rejected) in [
        (
            "def run(x: int?) -> int; if x != (while true; x = nil; break nil; end); return \"bad\"; end; 0; end",
            1,
        ),
        (
            "def run(x: int?) -> int; if x && (while true; x = nil; break nil; end); x; else; 0; end; end",
            0,
        ),
        (
            "def run(x: int?) -> int; if (x != nil) && (while true; x = nil; break true; end); x; else; 0; end; end",
            1,
        ),
        (
            "def run(x: int?) -> int; if x || (while true; x = 7; break nil; end); x; else; x; end; end",
            0,
        ),
        (
            "def run(x: int?) -> int; if x; x = nil; return x; end; 0; end",
            1,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn parameter_defaults_join_with_supplied_values_and_check_their_contract() {
    for (source, rejected) in [
        ("def run(x: int = 7) -> int; x; end", 0),
        ("def run(x: int = \"bad\") -> int; x; end", 1),
        ("def run(x: int = 7, y: int = x + 1) -> int; y; end", 0),
        ("def run(x = \"bad\") -> int; x; end", 1),
        ("def run(x = 7) -> int; x; end", 0),
        ("def run(x: int, y: int = x) -> int; y; end", 0),
    ] {
        check(source, rejected);
    }
}

#[test]
fn scalar_loop_flow_joins_zero_iterations_backedges_and_exits() {
    for (source, rejected) in [
        (
            "def run(flag: bool) -> int; x = 0; while flag; x += 1; end; x; end",
            0,
        ),
        (
            "def run(flag: bool) -> int; while flag; x = 1; end; x; end",
            1,
        ),
        (
            "def run(flag: bool) -> int?; while flag; x = 1; end; x; end",
            0,
        ),
        ("def run -> int; while false; 1 - \"x\"; end; 7; end", 0),
        ("def run -> int; while true; break 7; end; end", 0),
        ("def run -> nil; while true; break; end; end", 0),
        ("def run -> int; while true; return 7; end; end", 0),
        (
            "def run -> int; while true; next; end; \"unreachable\"; end",
            0,
        ),
        (
            "def run(flag: bool) -> int; while flag; if flag; break 7; end; next; end; 7; end",
            0,
        ),
        (
            "def run(x: int?) -> int; while x; x += 1; break; end; x || 0; end",
            0,
        ),
        (
            "def run(x: int?) -> int; while x; x = nil; next; end; x; end",
            1,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn nested_loops_keep_separate_control_targets_and_results() {
    for source in [
        "def run -> int; while true; while true; break \"inner\"; end; break 7; end; end",
        "def run(flag: bool) -> int; while flag; while flag; next; end; break; end; 7; end",
        "def run -> int; value = while true; break 7; end; value; end",
        "def run -> nil; value = while true; break; end; value; end",
        "def run -> nil; value = while false; 7; end; value; end",
    ] {
        check(source, 0);
    }
}

#[test]
fn incomplete_analysis_never_looks_like_a_clean_complete_check() {
    for source in [
        "def run; [1, 2].map { _1 }; end",
        "def run; begin; 1; rescue; 2; ensure; [1].map { _1 }; end; end",
        "def run(x); for _ in 1..3; x.no_such_method; end; end",
        "def run; missing; end",
        "def run(x); x.nil?; end",
        "def run(x: hash); x::nil?; end",
        "def run(x: hash); x::nil?(); end",
        "def run; begin; missing; rescue; 7; end; end",
        "def run; [1] <=> [2]; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
    }
    check("def run;for x in 1..3;x.no_such_method;end;end", 1);
}

#[test]
fn block_returns_wait_for_defining_frame_analysis_even_without_captures() {
    let program =
        bytecode::compile("def run; [1].each { return 7 }; end", Vec::new(), &()).unwrap();
    let block = program
        .functions
        .iter()
        .position(|function| function.name == "<block>")
        .unwrap();
    assert!(program.functions[block].captures.is_empty());
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let contracts = contracts(&mut ctx, &mut facts, &program).unwrap();
    let report = flow::analyze(&mut ctx, &mut facts, &program, block, &contracts.data).unwrap();
    assert!(!report.incomplete.data.is_empty());
    assert_eq!(report.returns, Atom::Never.fact());
}

#[test]
fn flow_diagnostics_use_converged_facts_and_original_source_offsets() {
    let source =
        "def run(flag: bool) -> int\n  x = 1\n  while flag\n    x = \"bad\"\n  end\n  x\nend";
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let contracts = contracts(&mut ctx, &mut facts, &program).unwrap();
    let index = program.names["run"];
    let report = flow::analyze(&mut ctx, &mut facts, &program, index, &contracts.data).unwrap();
    assert!(report.incomplete.data.is_empty());
    assert!(
        report
            .issues
            .data
            .iter()
            .any(|issue| matches!(issue.kind, IssueKind::Return { .. }))
    );
    assert!(
        report
            .issues
            .data
            .windows(2)
            .all(|pair| pair[0].pc <= pair[1].pc)
    );
    for issue in &report.issues.data {
        assert!((program.functions[index].locations[issue.pc] as usize) < source.len());
    }
}

#[test]
fn graph_partitions_code_and_places_every_jump_at_a_block_boundary() {
    let source = "def run(flag: bool, x: int = 1); while flag; if x.nil?; break; end; x += 1; next; end; x; end";
    let program = bytecode::compile(source, Vec::new(), &()).unwrap();
    let code = &program.functions[program.names["run"]].code;
    let mut ctx = CallContext::new(CallOptions::default());
    let graph = Graph::new(&mut ctx, code).unwrap();
    let mut next = 0;
    for (index, block) in graph.blocks.data.iter().enumerate() {
        assert_eq!(block.start, next);
        assert!(block.start < block.end);
        assert_eq!(graph.at(&mut ctx, block.start).unwrap(), index);
        if let Exit::Jump(target) | Exit::Branch(target) = block.exit {
            assert_eq!(
                graph.blocks.data[graph.at(&mut ctx, target).unwrap()].start,
                target
            );
        }
        next = block.end;
    }
    assert_eq!(next, code.len());
    drop(graph);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn local_snapshots_isolate_writes_and_join_only_changed_paths() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut initial = Slots::new(100_000, 0u32);
    for i in [0, 15, 16, 255, 256, 65_535, 99_999] {
        initial.set(&mut ctx, i, 1).unwrap();
    }
    let mut left = initial.snapshot(&mut ctx).unwrap();
    let mut right = initial.snapshot(&mut ctx).unwrap();
    left.set(&mut ctx, 256, 2).unwrap();
    right.set(&mut ctx, 99_999, 4).unwrap();
    assert_eq!(initial.get(&mut ctx, 256).unwrap(), 1);
    assert_eq!(initial.get(&mut ctx, 99_999).unwrap(), 1);
    let before = ctx.stats().steps;
    let mut joins = 0;
    assert!(
        left.merge(&mut ctx, &right, |_, a, b| {
            joins += 1;
            Ok(a | b)
        })
        .unwrap()
    );
    assert_eq!(joins, 2);
    assert!(ctx.stats().steps - before < 600);
    assert_eq!(left.get(&mut ctx, 256).unwrap(), 3);
    assert_eq!(left.get(&mut ctx, 99_999).unwrap(), 5);
    assert!(!left.merge(&mut ctx, &right, |_, a, b| Ok(a | b)).unwrap());
    let snapshot = left.snapshot(&mut ctx).unwrap();
    let before = ctx.stats().steps;
    assert!(
        !left
            .merge(&mut ctx, &snapshot, |_, _, _| panic!("identical subtree"))
            .unwrap()
    );
    assert_eq!(ctx.stats().steps - before, 1);
    drop((initial, left, right, snapshot));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn sparse_local_storage_handles_machine_sized_indices_on_the_default_stack() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut values = Slots::new(usize::MAX, 0);
    values.set(&mut ctx, usize::MAX - 1, 7).unwrap();
    let mut copied = values.snapshot(&mut ctx).unwrap();
    copied.set(&mut ctx, 0, 3).unwrap();
    assert_eq!(values.get(&mut ctx, 0).unwrap(), 0);
    assert_eq!(copied.get(&mut ctx, usize::MAX - 1).unwrap(), 7);
    assert!(
        values
            .merge(&mut ctx, &copied, |_, a, b| Ok(a | b))
            .unwrap()
    );
    drop((values, copied));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let contracts = contracts(ctx, &mut facts, program)?;
    let report = flow::analyze(
        ctx,
        &mut facts,
        program,
        program.names["run"],
        &contracts.data,
    )?;
    assert!(report.incomplete.data.is_empty());
    Ok(())
}

#[test]
fn control_flow_obeys_exact_quotas_and_releases_failed_analysis_storage() {
    let program = bytecode::compile("def run(x: int?, flag: bool) -> int; while flag; if x; x += 1; else; x = 7; end; if flag; break; end; end; x || 0; end", Vec::new(), &()).unwrap();
    let mut baseline = CallContext::new(CallOptions::default());
    accounting(&mut baseline, &program).unwrap();
    let stats = baseline.stats();
    assert_eq!(stats.retained_memory_bytes, 0);
    for (memory, steps, expected) in [
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
        let result = accounting(&mut ctx, &program);
        assert_eq!(result.as_ref().err().map(|e| e.kind), expected);
        if let Err(error) = result {
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for memory in (0..stats.peak_memory_bytes).step_by(113) {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            accounting(&mut ctx, &program).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0, "limit {memory}");
    }
}

#[test]
fn scalar_local_analysis_work_and_memory_do_not_grow_quadratically() {
    let measure = |count| {
        let mut source = String::from("def run(x: int)\n");
        for i in 0..count {
            source.push_str(&format!("v{i} = {i}\n"));
        }
        source.push_str("x\nend");
        let program = bytecode::compile(&source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        accounting(&mut ctx, &program).unwrap();
        ctx.stats()
    };
    let small = measure(200);
    let large = measure(400);
    assert!(large.steps < small.steps * 3, "{small:?} -> {large:?}");
    assert!(
        large.peak_memory_bytes < small.peak_memory_bytes * 3,
        "{small:?} -> {large:?}"
    );
}

#[test]
fn cached_local_paths_still_observe_cancellation_and_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut slots = Slots::new(32, 0);
        slots.set(&mut ctx, 0, 7).unwrap();
        let other = slots.snapshot(&mut ctx).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        let expected = if deadline {
            ErrorKind::Deadline
        } else {
            ErrorKind::Cancelled
        };
        assert_eq!(slots.get(&mut ctx, 0).unwrap_err().kind, expected);
        assert_eq!(slots.snapshot(&mut ctx).unwrap_err().kind, expected);
        assert_eq!(slots.set(&mut ctx, 0, 7).unwrap_err().kind, expected);
        assert_eq!(
            slots
                .merge(&mut ctx, &other, |_, a, _| Ok(a))
                .unwrap_err()
                .kind,
            expected
        );
        drop((slots, other));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn flow_reference_decisions_keep_documented_control_semantics() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-flow.json")).unwrap();
    let cases = fixtures["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 60);
    let mut differences = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert_eq!(
            !report.issues.data.is_empty(),
            case["rust_rejected"].as_bool().unwrap(),
            "{source}: {report:?}"
        );
        if case["go_rejected"] != case["rust_rejected"] {
            differences += 1;
            assert!(!case["difference"].as_str().unwrap().is_empty());
            let calls = case["calls"].as_array().unwrap();
            assert!(!calls.is_empty());
            let script = crate::Engine::new().compile(source).unwrap();
            for call in calls {
                let args: Vec<_> = call["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| {
                        if value.is_null() {
                            crate::Value::nil()
                        } else if let Some(value) = value.as_bool() {
                            crate::Value::boolean(value)
                        } else {
                            crate::Value::int(value.as_i64().unwrap())
                        }
                    })
                    .collect();
                let result = script.call("run", &args, CallOptions::default());
                if let Some(expected) = call["value"].as_i64() {
                    assert_eq!(
                        result.unwrap().value.as_int(),
                        Some(expected),
                        "{source}: {call}"
                    );
                } else {
                    let expected = match call["error"].as_str().unwrap() {
                        "type" => ErrorKind::Type,
                        "steps" => ErrorKind::Steps,
                        _ => panic!("unknown runtime expectation"),
                    };
                    assert_eq!(result.unwrap_err().kind, expected, "{source}: {call}");
                }
            }
        }
    }
    assert_eq!(differences, 9);
}

#[test]
fn scalar_operator_facts_cover_runtime_results_and_type_failures() {
    use super::relation::Relation;
    use crate::Value;
    let values = [
        (Atom::Nil, Value::nil()),
        (Atom::Bool, Value::boolean(true)),
        (Atom::Int, Value::int(2)),
        (Atom::Float, Value::float(1.5)),
        (Atom::String, Value::bytes(b"%s")),
        (Atom::Symbol, Value::symbol(b"ready")),
        (Atom::Duration, Value::duration(3)),
        (Atom::Time, Value::time(20, 0).unwrap()),
        (Atom::Money, Value::money(120, "USD").unwrap()),
    ];
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    for op in [
        "+", "-", "*", "/", "%", "**", "==", "!=", "<", "<=", ">", ">=",
    ] {
        for (left, a) in &values {
            for (right, b) in &values {
                let (inferred, _) = facts
                    .scalar_binary(&mut ctx, op, left.fact(), right.fact())
                    .unwrap();
                assert!(!inferred.unsupported);
                let actual = crate::ops::binary(&mut ctx, op, a.clone(), b.clone());
                match actual {
                    Ok(value) => {
                        assert!(!inferred.rejected, "{left:?} {op} {right:?}: {value:?}");
                        let atom = match value.0 {
                            crate::value::Kind::Bool(_) => Atom::Bool,
                            crate::value::Kind::Int(_) | crate::value::Kind::Big(_) => Atom::Int,
                            crate::value::Kind::Float(_) => Atom::Float,
                            crate::value::Kind::Bytes(_) => Atom::String,
                            crate::value::Kind::Duration(_) => Atom::Duration,
                            crate::value::Kind::Time(_) => Atom::Time,
                            crate::value::Kind::Money(_) => Atom::Money,
                            _ => panic!("unexpected scalar result {value:?}"),
                        };
                        assert_ne!(
                            facts
                                .relation(&mut ctx, atom.fact(), inferred.value)
                                .unwrap(),
                            Relation::Rejected,
                            "{left:?} {op} {right:?}: {value:?}"
                        );
                    }
                    Err(error) if error.kind == ErrorKind::Type => {
                        assert!(inferred.rejected, "{left:?} {op} {right:?}: {error}")
                    }
                    Err(error) => assert!(!inferred.rejected, "{left:?} {op} {right:?}: {error}"),
                }
            }
        }
    }
}
