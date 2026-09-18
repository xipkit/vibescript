use super::{
    arguments::Arguments,
    builtin_tests::check,
    builtins,
    collection_tests::{analyze, literal_fact},
    facts::{Atom, Facts},
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, Error, ErrorClass, ErrorKind, Value, builtin::Builtin,
};
use std::sync::Arc;

fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_output_writer(|_, _| Ok(()));
    engine.set_error_writer(|_, _| Ok(()));
    engine.set_random_source(|_, bytes| {
        bytes.fill(0);
        Ok(bytes.len())
    });
    engine
}

pub(super) fn witness(source: &str, expected: Option<&str>, rejected: bool) {
    witness_with(&engine(), source, expected, rejected);
}

fn witness_with(engine: &Engine, source: &str, expected: Option<&str>, rejected: bool) {
    let actual = engine
        .compile(source)
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    if let Some(expected) = expected {
        assert_eq!(actual.value.to_string(), expected, "{source}");
    }
    let mut ctx = CallContext::new(CallOptions::default());
    ctx.output_writer = Some(Arc::new(|_, _| panic!("checker executed output writer")));
    ctx.error_writer = Some(Arc::new(|_, _| panic!("checker executed error writer")));
    ctx.random_source = Some(Arc::new(|_, _| panic!("checker executed entropy provider")));
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
    drop((facts, report));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn regex_helpers_keep_constructor_and_optional_match_results() {
    for (expression, ty, expected) in [
        ("Regexp.new(\"x\")", "regex", None),
        ("Regexp.union()", "regex", None),
        ("Regexp.union(\"a+\",\"b\")", "regex", None),
        ("Regexp.escape(\"a+b\")", "string", Some("a\\+b")),
        ("Regexp.quote(\"a+b\")", "string", Some("a\\+b")),
        ("Regexp.last_match", "nil", Some("nil")),
        ("Regexp.last_match()", "nil", Some("nil")),
        ("Regex.match(\"a\",\"ab\")", "string?", Some("a")),
        ("Regex.match(\"(?:^a$)\",\"ab\")", "string?", Some("nil")),
        ("Regex.replace(\"aba\",\"a\",\"x\")", "string", Some("xba")),
        (
            "Regex.replace_all(\"aba\",\"a\",\"x\")",
            "string",
            Some("xbx"),
        ),
        ("Regexp[:new](\"x\")", "regex", None),
        ("(Regexp::new)(\"x\")", "regex", None),
        ("Regexp.new(*[\"x\"])", "regex", None),
    ] {
        let source = if ty == "regex" {
            format!("def run; {expression}; end")
        } else {
            format!("def run -> {ty}; {expression}; end")
        };
        witness(&source, expected, false);
    }
    witness(
        "def construct(r); r.new(\"x\"); end; def run; construct(Regexp); end",
        None,
        false,
    );
}

#[test]
fn duration_and_money_helpers_preserve_numeric_and_keyword_contracts() {
    for (expression, ty) in [
        ("Duration.build(7)", "duration"),
        ("Duration.build(7.9)", "duration"),
        ("Duration.build(seconds:7,minutes:2)", "duration"),
        ("Duration.build(**{weeks:1,days:2,hours:3})", "duration"),
        ("Duration.build(seconds:1,seconds:2)", "duration"),
        ("Duration.parse(\"7s\")", "duration"),
        ("Duration.parse(\"7s\",ignored:true)", "duration"),
        ("Duration[:build](seconds:7)", "duration"),
        ("money(\"1.25 USD\")", "money"),
        ("money(\"1.25 USD\",ignored:true)", "money"),
        ("money_cents(125,\"USD\")", "money"),
        ("money_cents(-125.5,\"USD\",ignored:true)", "money"),
    ] {
        witness(&format!("def run -> {ty}; {expression}; end"), None, false);
    }
}

#[test]
fn time_helpers_distinguish_text_clock_calendar_and_epoch_inputs() {
    for (expression, ty) in [
        ("now", "string"),
        ("now()", "string"),
        ("now(ignored:true)", "string"),
        ("Time.now", "time"),
        ("Time.now(in:\"UTC\")", "time"),
        ("Time.now(ignored:true)", "time"),
        ("Time.at(0,in:\"UTC\")", "time"),
        ("Time.at(0,7,:nsec,in:\"UTC\")", "time"),
        ("Time.new(2020,1,2,3,4,5,\"UTC\",ignored:true)", "time"),
        ("Time.new(2020,1,2,3,4,5,\"UTC\",7,8)", "time"),
        ("Time.utc(2020,1,2,3,4,5,7)", "time"),
        ("Time.local(2020)", "time"),
        ("Time.mktime(2020)", "time"),
        ("Time.gm(2020)", "time"),
        (
            "Time.utc(2020,\"ignored\",nil,false,[],{},nil,in:7)",
            "time",
        ),
        ("Time.parse(\"2020-01-02T03:04:05Z\")", "time"),
        (
            "Time.parse(\"2020-01-02\",\"2006-01-02\",in:\"UTC\")",
            "time",
        ),
        ("Time.parse(\"2020-01-02T03:04:05Z\",nil)", "time"),
    ] {
        witness(&format!("def run -> {ty}; {expression}; end"), None, false);
    }
}

#[test]
fn random_helpers_keep_bound_dependent_results_without_running_entropy() {
    for (expression, ty) in [
        ("rand", "float"),
        ("rand()", "float"),
        ("rand(nil)", "float"),
        ("rand(10)", "int"),
        ("rand(2..5)", "int"),
        ("rand(5...2)", "int"),
        ("rand(2..2)", "int"),
        ("srand(7)", "int?"),
        ("srand()", "int?"),
        ("uuid", "string"),
        ("uuid()", "string"),
        ("random_id()", "string"),
        ("random_id(7)", "string"),
    ] {
        witness(&format!("def run -> {ty}; {expression}; end"), None, false);
    }
    witness("def run -> int?; srand(7); srand(9); end", Some("7"), false);
}

#[test]
fn formatting_and_output_keep_return_values_without_writing_during_analysis() {
    for (expression, ty, expected) in [
        ("format(\"%s %d\",\"x\",7)", "string", "x 7"),
        ("sprintf(\"%.2f\",1.25)", "string", "1.25"),
        ("format(\"%.2s\",10**30)", "string", "10"),
        ("puts(\"hello\")", "nil", "nil"),
        ("puts()", "nil", "nil"),
        ("print(1,[2,3])", "nil", "nil"),
        ("print()", "nil", "nil"),
        ("warn(\"hello\")", "nil", "nil"),
        ("warn()", "nil", "nil"),
        ("p()", "nil", "nil"),
        ("p(7)", "int", "7"),
        ("p(7,\"x\")", "array<int|string>", "[7, x]"),
        ("p({x:7}).x", "int", "7"),
    ] {
        witness(
            &format!("def run -> {ty}; {expression}; end"),
            Some(expected),
            false,
        );
    }
    witness(
        "def run; a=[7]; b=p(a); b.push(8); a; end",
        Some("[7]"),
        false,
    );
}

#[test]
fn native_argument_contradictions_are_diagnosed_even_when_rescued() {
    for expression in [
        "Regexp.new()",
        "Regexp.new(:x)",
        "Regexp.new(\"x\",bad:1)",
        "Regexp.union(\"x\",7)",
        "Regexp.last_match(7)",
        "Regexp.escape(7)",
        "Regex.match(\"x\")",
        "Regex.match(:x,\"x\")",
        "Regex.replace(\"x\",/x/,\"y\")",
        "Duration.build()",
        "Duration.build(1,seconds:7)",
        "Duration.build(\"7\")",
        "Duration.build(bad:7)",
        "Duration.build(seconds:false)",
        "Duration.parse(7)",
        "money(7)",
        "money_cents(false,\"USD\")",
        "money_cents(7,:USD)",
        "Time.now(1)",
        "Time.now(in:7)",
        "Time.at()",
        "Time.at(\"7\")",
        "Time.at(0,nil)",
        "Time.at(0,0,\"nsec\")",
        "Time.at(0,0,:bad)",
        "Time.at(0,bad:7)",
        "Time.parse(7)",
        "Time.parse(\"x\",7)",
        "Time.parse(\"x\",bad:7)",
        "Time.utc()",
        "Time.utc(false)",
        "Time.utc(2020,1,1,0,0,0,-1)",
        "Time.utc(2020,1,1,0,0,0,1000000)",
        "Time.new(2020,1,1,0,0,0,7)",
        "Time.new(2020,1,1,0,0,0,\"UTC\",in:7)",
        "Time.gm(2020,1,1,0,0,0,0,0)",
        "now(7)",
        "now.call()",
        "rand.call(10)",
        "rand(0)",
        "rand(-1)",
        "rand(1.5)",
        "rand(2...2)",
        "rand(2..)",
        "rand(bad:7)",
        "srand(false)",
        "uuid(7)",
        "random_id(0)",
        "random_id(nil)",
        "format()",
        "sprintf(7)",
        "format(\"x\",bad:7)",
        "puts(bad:7)",
        "p(bad:7)",
    ] {
        witness(
            &format!("def run; begin; {expression}; rescue; 7; end; end"),
            Some("7"),
            true,
        );
    }
}

#[test]
fn native_parse_errors_are_runtime_effects_not_checker_execution() {
    for expression in [
        "Regexp.new(\"[\")",
        "Regex.match(\"[\",\"x\")",
        "Duration.parse(\"bad\")",
        "Time.parse(\"bad\")",
        "Time.at(0,in:\"../bad\")",
        "money(\"bad\")",
        "money_cents(7,\"BAD!\")",
        "format(\"%d\",\"x\")",
    ] {
        witness(
            &format!("def run; begin; {expression}; rescue; 7; end; end"),
            Some("7"),
            false,
        );
    }
}

#[test]
fn addressed_namespace_calls_preserve_selection_and_argument_effects() {
    for (source, expected, rejected) in [
        (
            "def run; r=Regex; r.replace(*[\"aba\",\"a\",\"x\"]); end",
            "xba",
            false,
        ),
        (
            "def run; r=Regex; text=r.replace((while true; r=nil; break \"aba\"; end),\"a\",\"x\"); [text,r]; end",
            "[xba, nil]",
            false,
        ),
        (
            "def run; r={regex:Regex}; r.regex.replace(\"aba\",\"a\",\"x\"); end",
            "xba",
            false,
        ),
        (
            "def run; begin; Regex.replace(\"aba\",\"a\",\"x\",bad:7); rescue; 7; end; end",
            "7",
            true,
        ),
        (
            "def run; x=0; begin; rand.call((while true; x=1; break 7; end)); rescue; x; end; end",
            "0",
            true,
        ),
    ] {
        witness(source, Some(expected), rejected);
    }
    witness(
        "def run; JSON[:parse]=7; begin; JSON.parse(\"7\"); rescue; 9; end; end",
        Some("9"),
        true,
    );
    for source in [
        "def run; Regex[:replace]=7; Regex.replace(\"x\",\"x\",\"y\"); end",
        "def run; JSON.clear; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((facts, report));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    check(
        "def run(x:int); begin; srand(x); rescue AssertionError; missing; end; 7; end",
        false,
    );
}

#[test]
fn native_numeric_boundaries_match_runtime_successes_and_error_classes() {
    use crate::{random::Method as Random, time::Constructor as Time, value::Kind};
    let mut inputs = vec![
        Value::nil(),
        Value::boolean(false),
        Value::bytes(b"7"),
        Value::symbol(b"nsec"),
        Value::array(vec![]),
    ];
    for n in [i64::MIN, -1, 0, 1, 1024, 1025, 999999, 1000000, i64::MAX] {
        inputs.push(Value::int(n));
    }
    for n in [
        f64::NEG_INFINITY,
        i64::MIN as f64,
        -1.5,
        -0.0,
        0.5,
        999999.9,
        1000000.0,
        i64::MAX as f64,
        f64::INFINITY,
        f64::NAN,
    ] {
        inputs.push(Value::float(n));
    }
    let mut count = 0;
    for variant in 0..10 {
        for input in &inputs {
            let builtin = match variant {
                0 | 1 => Builtin::DurationBuild,
                2 => Builtin::MoneyCents,
                3 => Builtin::Time(Time::At),
                4 => Builtin::Time(Time::Utc),
                5 => Builtin::Time(Time::New),
                6 => Builtin::Time(Time::Gm),
                7 => Builtin::Random(Random::Seed),
                8 => Builtin::Random(Random::Rand),
                _ => Builtin::Random(Random::Id),
            };
            let mut values = match variant {
                1 => vec![],
                6 => vec![
                    Value::int(2020),
                    Value::int(1),
                    Value::int(1),
                    Value::int(0),
                    Value::int(0),
                    Value::int(0),
                    input.clone(),
                ],
                _ => vec![input.clone()],
            };
            if variant == 2 {
                values.push(Value::bytes(b"USD"));
            }
            let keywords = match variant {
                1 => vec![(Value::symbol(b"seconds"), input.clone())],
                3 | 5 => vec![(Value::symbol(b"in"), Value::bytes(b"UTC"))],
                _ => vec![],
            };
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let mut args = Arguments::new();
            for value in &values {
                let fact = match value.0 {
                    Kind::Float(n) => facts.float(&mut ctx, n).unwrap(),
                    _ => literal_fact(&mut ctx, &mut facts, value),
                };
                args.positional.push(&mut ctx, fact).unwrap();
            }
            for (key, value) in &keywords {
                let name = literal_fact(&mut ctx, &mut facts, key);
                let fact = match value.0 {
                    Kind::Float(n) => facts.float(&mut ctx, n).unwrap(),
                    _ => literal_fact(&mut ctx, &mut facts, value),
                };
                args.keyword(&mut ctx, name, fact).unwrap();
            }
            let result = builtins::invoke(&mut ctx, &mut facts, builtin, &args).unwrap();
            let mut runtime = CallContext::new(CallOptions::default());
            runtime.random_source = Some(Arc::new(|_, bytes| {
                bytes.fill(0);
                Ok(bytes.len())
            }));
            let actual = builtin.call(&mut runtime, &values, &keywords, false);
            assert!(!result.incomplete, "{builtin:?} {input:?}");
            match actual {
                Ok(value) => {
                    assert!(
                        result.failures.data.is_empty(),
                        "{builtin:?} {input:?}: {:?}",
                        result.failures
                    );
                    let fact = literal_fact(&mut ctx, &mut facts, &value);
                    assert_ne!(
                        facts.relation(&mut ctx, fact, result.value).unwrap(),
                        Relation::Rejected,
                        "{builtin:?} {input:?}"
                    );
                }
                Err(error) => {
                    if error.class() == Some(ErrorClass::Limit) {
                        assert_eq!(error.kind, ErrorKind::OutputLimit);
                        assert!(runtime.checkpoint().is_ok());
                        assert_eq!(result.throws, 1 << ErrorClass::Limit as u8);
                        assert_eq!(result.value, Atom::Never.fact());
                    } else {
                        assert_ne!(
                            result.throws & (1 << error.class().unwrap() as u8),
                            0,
                            "{builtin:?} {input:?}: {error}"
                        );
                        if variant != 3 {
                            assert!(
                                !result.failures.data.is_empty(),
                                "{builtin:?} {input:?}: {error}"
                            );
                        }
                    }
                }
            }
            drop((result, args, facts));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            count += 1;
        }
    }
    assert_eq!(count, 240);
}

#[test]
fn native_facts_keep_known_bad_alternatives_beside_gradual_inputs() {
    for source in [
        "def run(x:string|any); Regexp.new(x); end",
        "def run(x:int|any); Duration.build(seconds:x); end",
        "def run(x:int|any); rand(x); end",
        "def run(x:string|any); Time.parse(x); end",
    ] {
        check(source, false);
    }
    for source in [
        "def run(x:bool|any); Regexp.new(x); end",
        "def run(x:bool|any); Duration.build(seconds:x); end",
        "def run(x:bool|any); rand(x); end",
        "def run(x:bool|any); Time.now(in:x); end",
        "def run -> int; now; end",
        "def run -> string; Time.now; end",
        "def run -> int; rand; end",
        "def run -> float; rand(7); end",
        "def run -> string; Regexp.new(\"x\"); end",
        "def run -> int; p(\"x\"); end",
    ] {
        check(source, true);
    }
}

#[test]
fn native_host_effects_retain_every_ordinary_exception_class() {
    for class in [
        ErrorClass::Runtime,
        ErrorClass::Argument,
        ErrorClass::Type,
        ErrorClass::Standard,
        ErrorClass::ZeroDivision,
        ErrorClass::Assertion,
        ErrorClass::Limit,
        ErrorClass::LocalJump,
    ] {
        let mut engine = engine();
        engine.set_random_source(move |_, _| {
            Err(Error::new(ErrorKind::Host, "entropy").with_class(class))
        });
        engine.set_output_writer(move |_, _| {
            Err(Error::new(ErrorKind::Host, "writer").with_class(class))
        });
        engine.set_error_writer(move |_, _| {
            Err(Error::new(ErrorKind::Host, "writer").with_class(class))
        });
        for expression in [
            "rand(7)",
            "srand()",
            "uuid",
            "random_id(7)",
            "puts()",
            "p(7)",
            "print(7)",
            "warn(7)",
        ] {
            witness_with(
                &engine,
                &format!(
                    "def run; begin; {expression}; rescue {}; \"caught\"; end; end",
                    class.name()
                ),
                Some("caught"),
                false,
            );
        }
    }
}

#[test]
fn infallible_native_calls_do_not_make_dead_rescues_reachable() {
    for expression in [
        "now",
        "Time.now",
        "Regexp.last_match",
        "Regexp.escape(\"x\")",
        "Duration.build(7)",
        "Duration.build(seconds:7)",
        "srand(7)",
    ] {
        witness(
            &format!("def run -> int; begin; {expression}; rescue; missing; end; 7; end"),
            Some("7"),
            false,
        );
    }
    let source = "def run; begin; random_id(1025); rescue; missing; end; end";
    let error = engine()
        .compile(source)
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::OutputLimit);
    check(source, false);
}

#[test]
fn custom_string_conversions_and_remaining_dynamic_work_are_explicitly_incomplete() {
    for source in [
        "def run(x:any); format(\"%s\",x); end",
        "def run(x:any); puts(x); end",
        "def run(x:{name:string}); sprintf(\"%s\",x); end",
        "def run; require(\"missing\"); end",
        "def run; Regexp.new(\"x\") { 7 }; end",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let report = analyze(&mut ctx, &mut facts, source).unwrap();
        assert!(!report.incomplete.data.is_empty(), "{source}: {report:?}");
        drop((facts, report));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    check("def run(x:any); p(x); end", false);
}

#[test]
fn native_analysis_does_not_compile_patterns_or_allocate_requested_output() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let value = facts.string(&mut ctx, &vec![b'['; 20_000]).unwrap();
    let mut args = Arguments::new();
    args.positional.push(&mut ctx, value).unwrap();
    let before = ctx.stats().steps;
    let result = builtins::invoke(
        &mut ctx,
        &mut facts,
        Builtin::Regexp(crate::regex::value::Constructor::New),
        &args,
    )
    .unwrap();
    assert_eq!(result.value, Atom::Regex.fact());
    assert!(!result.incomplete);
    assert!(ctx.stats().steps - before < 100);
    drop((result, args, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run -> int; begin; Regexp.union(\"a\",\"b\"); Regex.match(\"x\",\"x\"); Time.at(0,1,:nsec,in:\"UTC\"); Duration.build(days:1,seconds:2); money_cents(7,\"USD\"); srand(7); rand(10); p(format(\"%s\",now)); rescue; 7; ensure; Regexp.last_match; end; 7; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn native_analysis_obeys_exact_quotas_and_reclaims_partial_work() {
    use crate::Limits;
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
fn native_analysis_cannot_catch_cancellation_or_deadline_expiry() {
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
fn native_input_limits_remain_catchable_without_latching_budget_exhaustion() {
    for expression in [
        "random_id(1025)",
        "Regexp.new(\"x\"*16385)",
        "Regexp.union(\"x\"*16385)",
        "Regex.match(\"x\",(\"x\"*1024)*1025)",
        "format(\"%1048577d\",1)",
    ] {
        witness(
            &format!("def run; begin; {expression}; rescue LimitError; \"caught\"; end; end"),
            Some("caught"),
            false,
        );
    }
}

#[test]
fn native_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-native.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 90);
    let mut differences = 0;
    for case in cases {
        witness(
            case["source"].as_str().unwrap(),
            case["runtime"]["display"].as_str(),
            case["rust_rejected"].as_bool().unwrap(),
        );
        if case["go_rejected"] != case["rust_rejected"] {
            assert!(!case["difference"].as_str().unwrap().is_empty());
            differences += 1;
        }
    }
    assert_eq!(differences, 23);
}
