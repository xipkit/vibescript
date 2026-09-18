use super::{
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts},
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Result, Value};

fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    if !report.incomplete.data.is_empty() {
        let program = crate::bytecode::compile(source, Vec::new(), &()).unwrap();
        let pending: Vec<_> = report
            .incomplete
            .data
            .iter()
            .map(
                |&super::calls::Location {
                     function: f, pc, ..
                 }| program.functions[f].code[pc],
            )
            .collect();
        panic!("{source}: {report:?}; pending {pending:?}");
    }
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn witness(source: &str, args: &[Value], expected: &str) {
    let actual = crate::Engine::new()
        .compile(source)
        .unwrap()
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert_eq!(actual.value.to_string(), expected, "{source}");
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert!(report.issues.data.is_empty(), "{source}: {report:?}");
    let concrete = literal_fact(&mut ctx, &mut facts, &actual.value);
    assert_ne!(
        facts.relation(&mut ctx, concrete, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn case_branches_keep_first_match_order_and_skip_unreachable_effects() {
    for (source, expected) in [
        (
            "def run -> int; case 7; when 7; 9; else; \"bad\"; end; end",
            "9",
        ),
        (
            "def run -> int; case 7; when 8; missing; when 7; 9; else; \"bad\"; end; end",
            "9",
        ),
        (
            "def run -> int; case 7; when 7,missing; 9; else; \"bad\"; end; end",
            "9",
        ),
        (
            "def run -> int; case 7; when 7; 9; when missing; \"bad\"; end; end",
            "9",
        ),
        ("def run; case 7; when 8; missing; end; end", "nil"),
        (
            "def run; case; when false,nil; missing; when 0; 7; else; missing; end; end",
            "7",
        ),
        (
            "def run; case true; when false; missing; else; 7; end; end",
            "7",
        ),
        (
            "def run; case [7]; when [7]; 9; else; missing; end; end",
            "9",
        ),
        (
            "def run; case {a:7,b:8}; when {b:8,a:7}; 9; else; missing; end; end",
            "9",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn case_patterns_narrow_only_the_binding_that_supplied_the_target() {
    for (source, rejected) in [
        (
            "def run(x: int | string) -> int; case x; when 7; x+1; else; 9; end; end",
            false,
        ),
        (
            "def run(x: int | string) -> string; case x; when \"yes\"; x; else; \"no\"; end; end",
            false,
        ),
        (
            "def run(x: int?) -> int; case x; when nil; 7; else; x; end; end",
            false,
        ),
        (
            "def run(x: bool) -> int; case x; when false; 7; when true; 9; else; missing; end; end",
            false,
        ),
        (
            "def run(x: int | string) -> int; case x; when 7; x+\"bad\"; else; 9; end; end",
            true,
        ),
        (
            "def run(x) -> int; case x; when 7; x; else; 9; end; end",
            true,
        ),
        (
            "def run(x) -> number; case x; when 7; x; else; 9; end; end",
            false,
        ),
        (
            "def run(x: int | string) -> int; y=x; case x; when 7; y; else; 9; end; end",
            true,
        ),
        (
            "def run(x: bool) -> int; case; when x; 7; when !x; 9; else; missing; end; end",
            false,
        ),
    ] {
        check(source, rejected);
    }
}

#[test]
fn range_matchers_keep_bounds_direction_and_numeric_domains() {
    for (matcher, value, expected) in [
        ("1..3", "2", "true"),
        ("1...3", "3", "false"),
        ("3..1", "2", "true"),
        ("3...1", "1", "false"),
        ("..3", "2", "true"),
        ("3..", "2", "false"),
        ("1..3", "\"2\"", "false"),
        ("1..3", "[2]", "false"),
        ("7...7", "7", "false"),
        ("9223372036854775807..", "9223372036854775807", "true"),
    ] {
        witness(
            &format!("def run -> bool; ({matcher}) === {value}; end"),
            &[],
            expected,
        );
    }
    for (source, rejected) in [
        (
            "def run(x: int | string) -> int; case x; when 1..3; x; else; 7; end; end",
            false,
        ),
        (
            "def run(x) -> int; case x; when 1..3; x; else; 7; end; end",
            true,
        ),
        (
            "def run(x) -> number; case x; when 1..3; x; else; 7; end; end",
            false,
        ),
        ("def run -> int; for x in 7..7; x; end; end", false),
        (
            "def run -> int; for x in 7...7; missing; end; 9; end",
            false,
        ),
        ("def run; for x in 7..; x; end; end", true),
    ] {
        check(source, rejected);
    }
}

#[test]
fn regex_patterns_match_raw_strings_and_narrow_their_domain() {
    for (source, expected) in [
        (
            "def run -> int; case \"hello\"; when /el+/; 7; else; missing; end; end",
            "7",
        ),
        (
            "def run -> int; case \"ab\"; when /^a$/; missing; else; 7; end; end",
            "7",
        ),
        ("def run -> bool; /a/i === \"A\"; end", "true"),
        ("def run -> bool; /./ === 7; end", "false"),
        ("def run -> bool; /./ === :a; end", "false"),
    ] {
        witness(source, &[], expected);
    }
    check(
        "def run(x: int | string) -> string; case x; when /./; x; else; \"no\"; end; end",
        false,
    );
    check(
        "def run(x) -> string; case x; when /./; x; else; \"no\"; end; end",
        false,
    );
    check("def run; /(/; end", true);
}

#[test]
fn splatted_patterns_keep_empty_and_short_circuit_matching_rules() {
    for (source, expected) in [
        (
            "def run -> int; case 7; when *[8,7]; 9; else; missing; end; end",
            "9",
        ),
        (
            "def run -> int; case 7; when *[]; missing; else; 9; end; end",
            "9",
        ),
        (
            "def run -> int; case; when *[false,nil]; missing; when *[0]; 9; end; end",
            "9",
        ),
        (
            "def run -> int; case 7; when *[[7],1..8]; 9; else; missing; end; end",
            "9",
        ),
    ] {
        witness(source, &[], expected);
    }
    check("def run; case 7; when *7; 9; end; end", true);
    check(
        "def run(xs: array<int>) -> int?; case 7; when *xs; 9; end; end",
        false,
    );
}

#[test]
fn matching_cannot_narrow_reassigned_targets_or_value_copies() {
    witness(
        "def run; x=7; case x; when (begin; x=8; 7; end); x; else; missing; end; end",
        &[],
        "8",
    );
    witness(
        "def run; x=[7]; case x; when x.push(8); missing; else; x; end; end",
        &[],
        "[7, 8]",
    );
    witness(
        "def run; x=[7]; case x; when (begin; x[0]=8; [7]; end); x; else; missing; end; end",
        &[],
        "[8]",
    );
    witness(
        "def run; x=7; case x; when 8; missing; when (begin; x=9; 7; end); x; end; end",
        &[],
        "9",
    );
}

#[test]
fn numeric_matchers_preserve_float_kinds_and_exact_integer_comparisons() {
    for (source, expected) in [
        ("def run; 7 === 7.0; end", "true"),
        (
            "def run; 9007199254740993 === 9007199254740992.0; end",
            "false",
        ),
        (
            "def run; 9007199254740992.0 === 9007199254740992; end",
            "true",
        ),
        ("def run; (-0.0) === 0; end", "true"),
        ("def run; (1..3) === 2.5; end", "true"),
        ("def run; (1...3) === 3.0; end", "false"),
        ("def run; (3...1) === 1.0; end", "false"),
        (
            "def run -> int; case 7.0; when 7; 9; else; missing; end; end",
            "9",
        ),
    ] {
        witness(source, &[], expected);
    }
    for source in [
        "def run(x) -> float; case x; when 0.5; x; else; 9.0; end; end",
        "def run(x) -> int; case x; when 9007199254740993; x; else; 9; end; end",
        "def run(x: float) -> int; case x; when 9007199254740993; missing; else; 9; end; end",
        "def run(x: int) -> int; case x; when 0.5; missing; else; 9; end; end",
        "def run(x) -> int; case x; when 7...7; missing; else; 9; end; end",
    ] {
        check(source, false);
    }
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let nan = facts.float(&mut ctx, f64::NAN).unwrap();
    let result = facts.case_result(&mut ctx, Some(nan), nan, false).unwrap();
    let no = facts.boolean(&mut ctx, false).unwrap();
    assert_eq!(result.value, no);
    let unknown = facts
        .case_filter(&mut ctx, Atom::Unknown.fact(), nan, false, true)
        .unwrap();
    assert_eq!(unknown, Atom::Never.fact());
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn literal(ctx: &mut CallContext, facts: &mut Facts, value: &Value) -> super::facts::Fact {
    use crate::value::Kind;
    match &value.0 {
        Kind::Float(value) => facts.float(ctx, *value).unwrap(),
        Kind::Range(range) => facts
            .range(ctx, range.start, range.end, range.exclusive)
            .unwrap(),
        Kind::Regex(_) => {
            let value = ctx.import(value).unwrap();
            facts.regex(ctx, value).unwrap()
        }
        _ => literal_fact(ctx, facts, value),
    }
}

#[test]
fn matcher_facts_and_both_narrowed_branches_include_runtime_outcomes() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let values = [
        Value::nil(),
        Value::boolean(false),
        Value::boolean(true),
        Value::int(0),
        Value::int(7),
        Value::int(9007199254740993),
        Value::float(0.0),
        Value::float(-0.0),
        Value::float(7.0),
        Value::float(0.5),
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
        Value::bytes(b"a"),
        Value::bytes(b"\xffa"),
        Value::symbol("a"),
        Value::array(vec![]),
        Value::array(vec![Value::int(7)]),
    ];
    let mut matchers = values.to_vec();
    for (start, end, exclusive) in [
        (Some(1), Some(7), false),
        (Some(7), Some(1), true),
        (Some(7), Some(7), true),
        (None, Some(7), false),
        (Some(7), None, true),
    ] {
        matchers.push(Value(crate::value::Kind::Range(
            crate::range::Range::untracked(start, end, exclusive),
        )));
    }
    matchers.push(Value::regex(b"^a$", "").unwrap());
    matchers.push(Value::regex(b"a", "i").unwrap());
    let mut cases = 0;
    for target in &values {
        let target_fact = literal(&mut ctx, &mut facts, target);
        for matcher in &matchers {
            let matcher_fact = literal(&mut ctx, &mut facts, matcher);
            for splat in [false, true] {
                let actual_matcher = if splat {
                    Value::array(vec![Value::nil(), matcher.clone()])
                } else {
                    matcher.clone()
                };
                let matcher_fact = if splat {
                    facts
                        .tuple(&mut ctx, &[Atom::Nil.fact(), matcher_fact])
                        .unwrap()
                } else {
                    matcher_fact
                };
                let actual =
                    crate::ops::case_matches(&mut ctx, Some(target), &actual_matcher, splat)
                        .unwrap();
                let result = facts
                    .case_result(&mut ctx, Some(target_fact), matcher_fact, splat)
                    .unwrap();
                let expected = facts.boolean(&mut ctx, actual).unwrap();
                assert_ne!(
                    facts.relation(&mut ctx, expected, result.value).unwrap(),
                    Relation::Rejected,
                    "target {target}, matcher {actual_matcher}"
                );
                let broad = facts.atom(target_fact).map_or(target_fact, Atom::fact);
                for input in [target_fact, broad, Atom::Unknown.fact(), Atom::Any.fact()] {
                    let filtered = facts
                        .case_filter(&mut ctx, input, matcher_fact, splat, actual)
                        .unwrap();
                    assert_ne!(
                        facts.relation(&mut ctx, target_fact, filtered).unwrap(),
                        Relation::Rejected,
                        "target {target}, matcher {actual_matcher}, input {input:?}, matched {actual}"
                    );
                }
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 816);
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(x: string | int | nil) -> int; case x; when nil; 1; when /a+/; 2; when *[1..7,9]; 3; else; 4; end; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty());
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn matcher_compilation_and_narrowing_obey_exact_quotas_and_release_failures() {
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
fn matcher_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn matcher_predicates_survive_negation_and_nested_control_without_running_skipped_patterns() {
    for source in [
        "def run(x: int | string) -> int; if 7 === x; x+1; else; 9; end; end",
        "def run(x: int | string) -> int; if !(7 === x); 9; else; x+1; end; end",
        "def run(x: int | string) -> int; if (7 === x) && (x+1>0); x; else; 9; end; end",
        "def run(x: int?) -> int; case x; when nil; 7; else; x; end; end",
        "def run -> int; case 7; when 7,/(/; 9; else; missing; end; end",
        "def run -> int; case 7; when 7; 9; when /(/; missing; end; end",
    ] {
        check(source, false);
    }
    check("def run; case 7; when *[7,/(/]; 9; end; end", true);
    for (source, expected) in [
        (
            "def run; for x in [7]; case x; when 7; begin; break 9; end; else; missing; end; end; end",
            "9",
        ),
        (
            "def run; for x in [7]; case x; when 7; begin; next 9; end; else; missing; end; end; end",
            "nil",
        ),
        (
            "def run; while true; case 7; when 7; begin; return 9; end; else; missing; end; end; end",
            "9",
        ),
        (
            "def run; a=[7]; a[0]=case a[0]; when 7; 9; else; missing; end; a; end",
            "[9]",
        ),
        (
            "def id(x); x; end; def run; id(case 7; when 7; 9; else; missing; end); end",
            "9",
        ),
        (
            "def run; x=7; case; when (begin; x=8; false; end); missing; when x; x; end; end",
            "8",
        ),
    ] {
        witness(source, &[], expected);
    }
}

#[test]
fn deep_shared_case_values_and_reused_matcher_facts_use_the_default_stack() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut a = facts.integer(&mut ctx, 7).unwrap();
    let mut b = facts.integer(&mut ctx, 8).unwrap();
    for _ in 0..2000 {
        a = facts.tuple(&mut ctx, &[a, a]).unwrap();
        b = facts.tuple(&mut ctx, &[b, b]).unwrap();
    }
    let yes = facts.boolean(&mut ctx, true).unwrap();
    let no = facts.boolean(&mut ctx, false).unwrap();
    assert_eq!(
        facts
            .case_result(&mut ctx, Some(a), a, false)
            .unwrap()
            .value,
        yes
    );
    assert_eq!(
        facts
            .case_result(&mut ctx, Some(a), b, false)
            .unwrap()
            .value,
        no
    );
    let source = facts.union(&mut ctx, &[a, b]).unwrap();
    assert_eq!(
        facts.case_filter(&mut ctx, source, a, false, true).unwrap(),
        a
    );
    assert_eq!(
        facts
            .case_filter(&mut ctx, source, a, false, false)
            .unwrap(),
        b
    );
    let regex = crate::regex::value::Regex::compile(&mut ctx, Value::bytes(b"a+"), 0).unwrap();
    let regex = facts.regex(&mut ctx, regex).unwrap();
    let widened = facts.union(&mut ctx, &[regex, Atom::Regex.fact()]).unwrap();
    assert_eq!(widened, Atom::Regex.fact());
    let range = facts.range(&mut ctx, Some(1), Some(3), false).unwrap();
    assert_eq!(
        facts.union(&mut ctx, &[range, Atom::Range.fact()]).unwrap(),
        Atom::Range.fact()
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn short_circuit_partitions_preserve_guards_without_equating_unrelated_values() {
    let and = "def run(x: int | string) -> int; if (7 === x) && (x+1>0); x; else; 9; end; end";
    let or = "def run(x: int | string) -> int; if (7 === x) || (8 === x); x+1; else; 9; end; end";
    for (value, a, b) in [
        (Value::int(7), "7", "8"),
        (Value::int(8), "9", "9"),
        (Value::bytes(b"bad"), "9", "9"),
    ] {
        witness(and, std::slice::from_ref(&value), a);
        witness(or, std::slice::from_ref(&value), b);
    }
    for a in [false, true] {
        witness(
            "def run(a: bool) -> int; if a && (begin; a=false; a; end); missing; else; 7; end; end",
            &[Value::boolean(a)],
            "7",
        );
        for b in [false, true] {
            for (source, expected) in [
                (
                    "def run(a: bool,b: bool) -> bool; (a && b) || !a; end",
                    (a && b) || !a,
                ),
                (
                    "def run(a: bool,b: bool) -> bool; if a; b; else; b; end; end",
                    b,
                ),
                (
                    "def pair(a: bool,b: bool) -> bool; b; end; def run(a: bool,b: bool) -> bool; pair(a && false,b); end",
                    b,
                ),
            ] {
                witness(
                    source,
                    &[Value::boolean(a), Value::boolean(b)],
                    if expected { "true" } else { "false" },
                );
            }
        }
    }
    check(
        "def run(x: int | string) -> int; if (7 === x) && (begin; x=\"bad\"; true; end); x; else; 9; end; end",
        true,
    );
}

#[test]
fn case_reference_decisions_keep_runtime_witnesses_and_precision_limits() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-case.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 53);
    let mut differences = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        check(source, case["rust_rejected"].as_bool().unwrap());
        if case["go_rejected"] != case["rust_rejected"] {
            differences += 1;
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
            assert_eq!(
                actual.unwrap_err().kind,
                match error {
                    "type" => ErrorKind::Type,
                    "argument" => ErrorKind::Argument,
                    _ => panic!("unknown runtime error"),
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
    assert_eq!(differences, 29);
}
