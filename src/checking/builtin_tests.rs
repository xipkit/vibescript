use super::{
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    relation::Relation,
};
use crate::{CallContext, CallOptions, Engine, Value};

fn witness(source: &str, args: &[Value], expected: &str, rejected: bool) {
    let actual = Engine::new()
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"));
    assert_eq!(actual.value.to_string(), expected, "{source}");
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
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
fn json_calls_expose_validated_contracts_and_gradual_unvalidated_data() {
    for (source, expected) in [
        ("def run -> int; JSON.parse_as(\"7\", int); end", "7"),
        (
            "def run -> int; JSON.parse_as(\"{\\\"x\\\":7}\", {x:int}).x; end",
            "7",
        ),
        (
            "def id(x); x; end; def run -> array<int>; t=id(array<int>); JSON.parse_as(\"[1,2]\", t); end",
            "[1, 2]",
        ),
        (
            "def run -> int; JSON.parse(\"{\\\"x\\\":7}\")[\"x\"]; end",
            "7",
        ),
        ("def run -> string; JSON.stringify({x:7}); end", "{\"x\":7}"),
        ("def run -> int; JSON::parse_as(\"7\", int); end", "7"),
        ("def run -> int; JSON.parse_as(*[\"7\"], int); end", "7"),
        ("def run -> int; j=JSON; j.parse_as(\"7\", int); end", "7"),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn core_builtin_calls_keep_scalar_results_and_real_exception_paths() {
    for (source, expected) in [
        ("def run -> int; to_int(\"7\"); end", "7"),
        ("def run -> float; to_float(\"7.5\"); end", "7.5"),
        ("def run -> float; Math.sqrt(9); end", "3"),
        ("def run -> hash; Hash.new(); end", "{}"),
        ("def run -> hash; Hash.new; end", "{}"),
        (
            "def run -> int; begin; assert(false); rescue AssertionError; 7; end; end",
            "7",
        ),
        ("def run -> int; assert(true); 7; end", "7"),
        (
            "def run -> int; begin; JSON.parse(\"bad\"); rescue; 7; end; end",
            "7",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn completed_handler_and_loop_fixtures_keep_runtime_witnesses() {
    for (source, expected) in [
        ("def run; JSON.parse(\"7\"); end", "7"),
        (
            "def run; begin; 1; rescue; 2; ensure; JSON.parse(\"1\"); end; end",
            "1",
        ),
        (
            "def run; for x in [7]; begin; x; ensure; JSON.parse(\"1\"); end; end; end",
            "7",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn type_literals_follow_bindings_and_survive_call_boundaries() {
    for (source, expected) in [
        (
            "def id(x); x; end; def run -> int; t=id(int); JSON.parse_as(\"7\",t); end",
            "7",
        ),
        (
            "def run -> int; int=7; begin; JSON.parse_as(\"7\",int); rescue; 9; end; end",
            "9",
        ),
        ("def run -> int; int=7; t={x:int}; t.x; end", "7"),
        (
            "def run -> int; t={x:int}; JSON.parse_as(\"{\\\"x\\\":7}\",t.dup).x; end",
            "7",
        ),
        ("def run -> bool; t={x:int}; t.nil?; end", "false"),
        (
            "def run -> array<int>; JSON.parse_as(\"[1,2]\",array<int>); end",
            "[1, 2]",
        ),
        (
            "def run -> hash<string,int>; JSON.parse_as(\"{\\\"x\\\":7}\",hash<string,int>); end",
            "{x: 7}",
        ),
        ("def run -> int?; JSON.parse_as(\"null\",int?); end", "nil"),
    ] {
        let rejected = source.contains("int=7; begin");
        witness(source, &[], expected, rejected);
    }
}

#[test]
fn builtin_lookup_and_argument_failures_are_catchable_and_diagnosed() {
    for expression in [
        "JSON.nope",
        "JSON::nope",
        "JSON.parse",
        "JSON.parse()",
        "JSON.parse(7)",
        "JSON.parse(:\"7\")",
        "JSON.parse(\"7\",bad:1)",
        "JSON.parse_as(\"7\",\"int\")",
        "JSON.parse_as(:seven,int)",
        "JSON.stringify(int)",
        "JSON.stringify([1,/x/])",
        "to_int(false)",
        "to_int(2.5)",
        "to_float([])",
        "Math.sqrt(-1)",
        "Math.log(1,-2)",
        "Math.sin(\"bad\")",
        "Math.PI()",
        "Hash.new(7)",
        "assert()",
        "to_int",
        "JSON(7)",
    ] {
        let source = format!("def run; begin; {expression}; rescue; 7; end; end");
        witness(&source, &[], "7", true);
    }
}

#[test]
fn builtin_targets_are_selected_before_argument_effects() {
    for (source, expected) in [
        (
            "def run; x=0; begin; JSON.nope((while true; x=1; break 0; end)); rescue; x; end; end",
            "0",
        ),
        (
            "def run; x=0; begin; (JSON.nope)((while true; x=1; break 0; end)); rescue; x; end; end",
            "0",
        ),
        (
            "def run; j=JSON; t={x:int}; j.parse_as(\"{\\\"x\\\":7}\", (while true; j=nil; break t; end)).x; end",
            "7",
        ),
    ] {
        witness(source, &[], expected, source.contains("nope"));
    }
}

#[test]
fn computed_namespace_calls_keep_attached_selection() {
    for (source, expected) in [
        ("def run -> int; (JSON.parse_as)(\"7\",int); end", "7"),
        ("def run -> int; (JSON::parse_as)(\"7\",int); end", "7"),
        ("def run -> int; JSON[:parse_as](\"7\",int); end", "7"),
        ("def run -> float; Math::PI; end", "3.141592653589793"),
        ("def run -> float; Math::sqrt(*[9]); end", "3"),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn builtin_results_keep_known_bad_alternatives_beside_dynamic_inputs() {
    for source in [
        "def run(x: int | any); to_int(x); end",
        "def run(x: int | any); Math.sin(x); end",
        "def run(raw: string | any); JSON.parse_as(raw,int); end",
    ] {
        check(source, false);
    }
    for source in [
        "def run(x: bool | any); to_int(x); end",
        "def run(x: string | any); Math.sin(x); end",
        "def run(raw: bool | any); JSON.parse_as(raw,int); end",
        "def run(flag: bool) -> int; if flag; t={x:int}; else; t={x:string}; end; JSON.parse_as(\"{}\",t).x; end",
        "def run -> int; JSON.stringify(7); end",
        "def run -> int; to_float(7); end",
    ] {
        check(source, true);
    }
}

pub(super) fn check(source: &str, rejected: bool) {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn source_and_local_shadowing_override_builtin_names() {
    for (source, expected, rejected) in [
        (
            "def to_int(x); \"script\"; end; def run -> string; to_int(7); end",
            "script",
            false,
        ),
        (
            "def run(to_int:int=7); begin; to_int(\"7\"); rescue; 9; end; end",
            "9",
            true,
        ),
        (
            "def run(JSON:{parse:int}={parse:7}); JSON.parse; end",
            "7",
            false,
        ),
        (
            "def run(JSON:{parse:int}={parse:7}); begin; JSON.parse(\"7\"); rescue; 9; end; end",
            "9",
            true,
        ),
    ] {
        witness(source, &[], expected, rejected);
    }
}

#[test]
fn registered_builtin_overrides_use_signatures_without_executing_host_code() {
    use super::{
        calls::{self, Host, World},
        facts::Atom,
    };
    use crate::{HostMethod, Signature, SignatureParam, budget::Buffer};
    let method = HostMethod::new("to_int", |_, _, _| panic!("checker invoked host"))
        .with_contract(
            |_, _, _| panic!("checker invoked validator"),
            |_, _| panic!("checker invoked validator"),
        )
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "text".into(),
                ty: "string".into(),
                optional: false,
            }],
            result: "string".into(),
            accepts_block: false,
        })
        .unwrap();
    let mut engine = Engine::new();
    engine.register_method("to_int", method.clone());
    let script = engine
        .compile("def run -> string; to_int(\"7\"); end")
        .unwrap();
    let program = &script.inner.code.program;
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(&mut ctx, ty, |_, _| Ok(None)).unwrap();
        contracts.push(&mut ctx, fact).unwrap();
    }
    let value = method.value();
    let crate::value::Kind::Host(method) = &value.0 else {
        unreachable!()
    };
    let host = Host::new(&mut ctx, &mut facts, method.signature()).unwrap();
    let report = calls::analyze(
        &mut ctx,
        &mut facts,
        World {
            program,
            contracts: &contracts.data,
            hosts: &[host],
            globals: &[],
        },
        program.names["run"],
        &[],
    )
    .unwrap();
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    assert_eq!(report.returns, Atom::String.fact());
    drop((contracts, report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn root_overrides_do_not_reuse_builtin_implementations_or_namespace_facts() {
    use super::{
        calls::{self, Host, Target, World},
        facts::Atom,
    };
    use crate::bytecode;
    for (name, source, target, incomplete, rejected) in [
        (
            "to_int",
            "def run; to_int(7); end",
            Target::Host(0),
            false,
            false,
        ),
        (
            "to_int",
            "def run; to_int(7); end",
            Target::NonCallable,
            false,
            true,
        ),
        (
            "JSON",
            "def run; JSON.parse_as(\"7\",int); end",
            Target::Dynamic,
            true,
            false,
        ),
        (
            "int",
            "def run; JSON.parse_as(\"7\",int); end",
            Target::Dynamic,
            true,
            false,
        ),
    ] {
        let program = bytecode::compile(source, Vec::new(), &()).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut host = Host::new(&mut ctx, &mut facts, None).unwrap();
        host.result = Atom::String.fact();
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                program: &program,
                contracts: &[],
                hosts: &[host],
                globals: &[(Value::bytes(name), target)],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert_eq!(
            !report.incomplete.data.is_empty(),
            incomplete,
            "{source}: {report:?}"
        );
        assert_eq!(
            !report.issues.data.is_empty(),
            rejected,
            "{source}: {report:?}"
        );
        if name == "to_int" && !rejected {
            assert_eq!(report.returns, Atom::String.fact());
        }
        drop((report, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn unmodeled_builtins_and_shadowed_calls_remain_explicitly_incomplete() {
    for source in [
        "def run; JSON.parse(\"7\") { 1 }; end",
        "def run; require(\"missing\"); end",
        "def run; JSON={parse:7}; JSON.parse; end",
        "def run; to_int=7; to_int(\"7\"); end",
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
fn namespace_alternatives_and_json_members_preserve_known_contracts() {
    for (source, expected) in [
        ("def run; JSON.keys; end", "[parse, parse_as, stringify]"),
        (
            "def run; (JSON.keys)(); end",
            "[parse, parse_as, stringify]",
        ),
        ("def run -> int; JSON.dup.size; end", "3"),
        ("def run -> bool; JSON.nil?; end", "false"),
        (
            "def decode(j,raw); j.parse_as(raw,{x:int}); end; def run -> int; decode(JSON,\"{\\\"x\\\":7}\").x; end",
            "7",
        ),
        (
            "def run -> string; JSON.stringify(JSON.parse_as(\"{}\",{name?:string})); end",
            "{}",
        ),
    ] {
        witness(source, &[], expected, false);
    }
    check(
        "def run(flag: bool); if flag; j=JSON; else; j=Math; end; j.parse(\"7\"); end",
        true,
    );
    check(
        "def run(flag: bool); if flag; j=JSON; else; j=nil; end; j&.parse(\"7\"); end",
        false,
    );
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def decode(raw); JSON.parse_as(raw,{items:array<int>,name?:string}); end; def run -> string; begin; data=decode(\"{\\\"items\\\":[1,2]}\"); JSON.stringify(data); rescue; \"bad\"; ensure; Hash.new; end; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn builtin_analysis_obeys_exact_quotas_and_reclaims_partial_work() {
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
fn cancelled_builtin_analysis_never_enters_script_rescue() {
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
fn json_encoding_checks_shared_fact_graphs_without_serializing_them() {
    use super::{arguments::Arguments, builtins, facts::Atom};
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut value = Atom::Int.fact();
    for _ in 0..4000 {
        value = facts.tuple(&mut ctx, &[value, value]).unwrap();
    }
    let mut args = Arguments::new();
    args.positional.push(&mut ctx, value).unwrap();
    let result = builtins::invoke(
        &mut ctx,
        &mut facts,
        crate::builtin::Builtin::JsonStringify,
        &args,
    )
    .unwrap();
    assert_eq!(result.value, Atom::String.fact());
    assert!(result.failures.data.is_empty());
    assert!(!result.incomplete);
    drop((result, args, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn builtin_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-builtins.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 67);
    let mut differences = 0;
    for case in cases {
        let source = case["source"].as_str().unwrap();
        witness(
            source,
            &[],
            case["runtime"]["display"].as_str().unwrap(),
            case["rust_rejected"].as_bool().unwrap(),
        );
        if case["go_rejected"] != case["rust_rejected"] {
            assert!(!case["difference"].as_str().unwrap().is_empty());
            differences += 1;
        }
    }
    assert_eq!(differences, 14);
}

#[test]
fn known_json_values_do_not_make_unreachable_rescues_reachable() {
    witness(
        "def run -> int; begin; JSON.stringify([1,{x:\"text\",ok:true}]); rescue; missing; end; 7; end",
        &[],
        "7",
        false,
    );
}

#[test]
fn numeric_builtin_facts_include_ieee_results_and_domain_errors() {
    use super::{arguments::Arguments, builtins, facts::Atom};
    use crate::builtin::{Builtin, Math};
    let builtins = [
        Builtin::Math(Math::Sqrt),
        Builtin::Math(Math::Cbrt),
        Builtin::Math(Math::Sin),
        Builtin::Math(Math::Cos),
        Builtin::Math(Math::Tan),
        Builtin::Math(Math::Asin),
        Builtin::Math(Math::Acos),
        Builtin::Math(Math::Atan),
        Builtin::Math(Math::Exp),
        Builtin::Math(Math::Log2),
        Builtin::Math(Math::Log10),
        Builtin::Math(Math::Atan2),
        Builtin::Math(Math::Hypot),
        Builtin::Math(Math::Log),
        Builtin::ToInt,
        Builtin::ToFloat,
    ];
    for builtin in builtins {
        for number in [
            f64::NEG_INFINITY,
            -2.0,
            -1.0,
            -0.0,
            0.0,
            0.5,
            1.0,
            2.0,
            f64::INFINITY,
            f64::NAN,
        ] {
            let count = if matches!(
                builtin,
                Builtin::Math(Math::Atan2 | Math::Hypot | Math::Log)
            ) {
                2
            } else {
                1
            };
            let values = vec![Value::float(number); count];
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let mut args = Arguments::new();
            let value = facts.float(&mut ctx, number).unwrap();
            for _ in 0..count {
                args.positional.push(&mut ctx, value).unwrap();
            }
            let result = builtins::invoke(&mut ctx, &mut facts, builtin, &args).unwrap();
            let actual = builtin.call(
                &mut CallContext::new(CallOptions::default()),
                &values,
                &[],
                false,
            );
            assert!(!result.incomplete, "{builtin:?}({number}): incomplete");
            assert_eq!(
                !result.failures.data.is_empty(),
                actual.is_err(),
                "{builtin:?}({number}): {actual:?}"
            );
            if let Ok(value) = actual {
                let value = literal_fact(&mut ctx, &mut facts, &value);
                assert_ne!(
                    facts.relation(&mut ctx, value, result.value).unwrap(),
                    Relation::Rejected,
                    "{builtin:?}({number})"
                );
                assert_ne!(result.value, Atom::Never.fact());
            } else {
                assert_ne!(result.throws, 0, "{builtin:?}({number})");
            }
            drop((result, args, facts));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
