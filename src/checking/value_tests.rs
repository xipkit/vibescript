use super::{
    arguments::Arguments,
    builtin_tests::check,
    builtins,
    collection_tests::{analyze, literal_fact},
    facts::Facts,
    native_tests::witness,
    relation::Relation,
};
use crate::{CallContext, CallOptions, ErrorKind, Value, bytecode::CallSite};

const TIME: &str = "Time.at(0.125,in:\"UTC\")";
const DURATION: &str = "Duration.build(93784)";
const MONEY: &str = "money_cents(125,\"USD\")";

#[test]
fn time_properties_preserve_field_types_and_reject_calls() {
    for (name, ty) in [
        ("year", "int"),
        ("month", "int"),
        ("mon", "int"),
        ("day", "int"),
        ("mday", "int"),
        ("hour", "int"),
        ("min", "int"),
        ("sec", "int"),
        ("wday", "int"),
        ("yday", "int"),
        ("nsec", "int"),
        ("tv_nsec", "int"),
        ("usec", "int"),
        ("tv_usec", "int"),
        ("hash", "int"),
        ("to_i", "int"),
        ("tv_sec", "int"),
        ("utc_offset", "int"),
        ("gmt_offset", "int"),
        ("gmtoff", "int"),
        ("subsec", "float"),
        ("to_f", "float"),
        ("to_r", "float"),
        ("zone", "string"),
        ("utc?", "bool"),
        ("gmt?", "bool"),
        ("dst?", "bool"),
        ("isdst", "bool"),
        ("sunday?", "bool"),
        ("monday?", "bool"),
        ("tuesday?", "bool"),
        ("wednesday?", "bool"),
        ("thursday?", "bool"),
        ("friday?", "bool"),
        ("saturday?", "bool"),
        ("getutc", "time"),
        ("getgm", "time"),
        ("utc", "time"),
        ("gmtime", "time"),
    ] {
        witness(&format!("def run -> {ty}; {TIME}.{name}; end"), None, false);
        witness(
            &format!("def run; begin; {TIME}.{name}(); rescue; 7; end; end"),
            Some("7"),
            true,
        );
    }
    for (suffix, ty) in [
        ("to_a[0]", "int"),
        ("to_a[8]", "bool"),
        ("to_a[9]", "string"),
    ] {
        witness(
            &format!("def run -> {ty}; {TIME}.{suffix}; end"),
            None,
            false,
        );
    }
}

#[test]
fn time_methods_keep_formatting_rounding_zones_and_nullable_comparison() {
    witness(
        "def run -> string; (Time.at(0,in:\"UTC\").to_s)(); end",
        Some("1970-01-01T00:00:00Z"),
        false,
    );
    for (suffix, ty) in [
        ("to_s", "string"),
        ("to_s()", "string"),
        ("string()", "string"),
        ("inspect", "string"),
        ("iso8601", "string"),
        ("iso8601(3)", "string"),
        ("xmlschema(2)", "string"),
        ("rfc3339(9)", "string"),
        ("httpdate", "string"),
        ("rfc2822()", "string"),
        ("rfc822", "string"),
        ("format(\"2006-01-02\")", "string"),
        ("strftime(\"%Y-%m-%d\")", "string"),
        ("getlocal(\"+02:00\")", "time"),
        ("localtime(\"UTC\")", "time"),
        ("getlocal(nil)", "time"),
        ("round", "time"),
        ("round(1)", "time"),
        ("ceil()", "time"),
        ("floor", "time"),
        ("round(1000)", "time"),
        ("between?(Time.at(-1),Time.at(1))", "bool"),
    ] {
        witness(
            &format!("def run -> {ty}; {TIME}.{suffix}; end"),
            None,
            false,
        );
    }
    witness(
        &format!("def run -> int; {TIME} <=> Time.at(1); end"),
        Some("-1"),
        false,
    );
    witness(
        &format!("def run -> nil; {TIME} <=> 7; end"),
        Some("nil"),
        false,
    );
}

#[test]
fn duration_units_parts_and_anchors_keep_constructed_result_types() {
    for unit in [
        "second", "seconds", "minute", "minutes", "hour", "hours", "day", "days", "week", "weeks",
    ] {
        witness(&format!("def run -> duration; 7.{unit}; end"), None, false);
        witness(
            &format!("def run -> int; {DURATION}.{unit}; end"),
            None,
            false,
        );
        witness(
            &format!("def run; begin; 7.{unit}(); rescue; 7; end; end"),
            Some("7"),
            true,
        );
    }
    for (name, ty) in [
        ("in_seconds", "float"),
        ("in_minutes", "float"),
        ("in_hours", "float"),
        ("in_days", "float"),
        ("in_weeks", "float"),
        ("in_months", "float"),
        ("in_years", "float"),
        ("to_i", "int"),
        ("iso8601", "string"),
        ("format", "string"),
        ("parts.days", "int"),
        ("parts.hours", "int"),
        ("parts.minutes", "int"),
        ("parts.seconds", "int"),
        ("to_s()", "string"),
        ("inspect", "string"),
    ] {
        witness(
            &format!("def run -> {ty}; {DURATION}.{name}; end"),
            None,
            false,
        );
    }
    for name in ["after", "since", "from_now", "ago", "before", "until"] {
        for argument in ["", "Time.at(0,in:\"UTC\")", "\"2020-01-02T03:04:05Z\""] {
            witness(
                &format!("def run -> time; {DURATION}.{name}({argument}); end"),
                None,
                false,
            );
        }
        witness(
            &format!("def run; begin; {DURATION}.{name}; rescue; 7; end; end"),
            Some("7"),
            true,
        );
    }
}

#[test]
fn money_properties_and_rendering_follow_the_runtime_call_contracts() {
    for (suffix, ty, expected) in [
        ("currency", "string", "USD"),
        ("cents", "int", "125"),
        ("amount", "string", "1.25 USD"),
        ("format", "string", "1.25 USD"),
        ("format()", "string", "1.25 USD"),
        ("format(7,bad:true)", "string", "1.25 USD"),
        ("to_s()", "string", "1.25 USD"),
        ("string", "string", "1.25 USD"),
        ("inspect()", "string", "1.25 USD"),
        (
            "between?(money_cents(0,\"USD\"),money_cents(200,\"USD\"))",
            "bool",
            "true",
        ),
    ] {
        witness(
            &format!("def run -> {ty}; {MONEY}.{suffix}; end"),
            Some(expected),
            false,
        );
    }
}

#[test]
fn native_value_identity_methods_do_not_invent_mutability_or_host_effects() {
    for receiver in [TIME, DURATION, MONEY, "/x/"] {
        for suffix in [
            "itself", "dup", "clone", "freeze", "itself()", "dup()", "clone()", "freeze()",
        ] {
            witness(&format!("def run; ({receiver}).{suffix}; end"), None, false);
        }
        for (suffix, expected) in [
            ("nil?", "false"),
            ("frozen?", "true"),
            ("nil?()", "false"),
            ("frozen?()", "true"),
        ] {
            witness(
                &format!("def run -> bool; ({receiver}).{suffix}; end"),
                Some(expected),
                false,
            );
        }
        for method in ["eql?", "equal?"] {
            witness(
                &format!("def run -> bool; ({receiver}).{method}(nil); end"),
                Some("false"),
                false,
            );
        }
    }
}

#[test]
fn regex_value_metadata_predicates_and_operators_preserve_types_and_anchors() {
    for (expression, ty, expected) in [
        ("/a+/im.source", "string", "a+"),
        ("/a+/im.flags", "string", "im"),
        ("/a/.source()", "string", "a"),
        ("/a/.flags(ignored:true)", "string", ""),
        ("/a/.string", "string", "/a/"),
        ("(/a/.string)()", "string", "/a/"),
        ("/a/.inspect", "string", "/a/"),
        ("Regexp.new(\"a\").source", "string", "a"),
        ("/^a$/.match?(\"ab\")", "bool", "false"),
        ("/a/.match?(\"ba\")", "bool", "true"),
        ("/a/ =~ \"ba\"", "int?", "1"),
        ("\"x\" =~ /a/", "int?", "nil"),
        ("/a/ !~ \"x\"", "bool", "true"),
        ("\"ba\" !~ /a/", "bool", "false"),
        ("/a/ + \"x\"", "string", "/a/x"),
    ] {
        witness(
            &format!("def run -> {ty}; {expression}; end"),
            Some(expected),
            false,
        );
    }
}

#[test]
fn native_value_failures_keep_prior_argument_effects_and_are_catchable() {
    for expression in [
        "Time.at(0).year()",
        "Time.at(0).to_a()",
        "Time.at(0).format",
        "Time.at(0).format(7)",
        "Time.at(0).strftime(:x)",
        "Time.at(0).iso8601(-1)",
        "Time.at(0).round(1.5)",
        "Time.at(0).ceil(1)",
        "Time.at(0).getlocal(7)",
        "Time.at(0).iso8601(ignored:true)",
        "Duration.build(7).parts()",
        "Duration.build(7).to_i()",
        "Duration.build(7).after(nil)",
        "Duration.build(7).after(bad:1)",
        "Duration.build(7).between?(nil,7)",
        "money_cents(7,\"USD\").currency()",
        "money_cents(7,\"USD\").cents()",
        "money_cents(7,\"USD\").to_s(7)",
        "money_cents(7,\"USD\").between?(nil,nil)",
        "7.0.days",
        "false.days",
        "/a/.match?(7)",
        "/a/.match?",
        "/a/.match?(\"a\",bad:true)",
        "/a/.inspect(bad:true)",
        "/a/.string(bad:true)",
        "/a/.nil?(bad:true)",
        "/a/.itself(bad:true)",
        "/a/.dup(bad:true)",
        "/a/.to_s",
        "Time.at(0).nope",
        "Time.at(0).clear",
        "Time.at(0)::year",
        "7 =~ /a/",
    ] {
        witness(
            &format!("def run; begin; {expression}; rescue; 7; end; end"),
            Some("7"),
            true,
        );
    }
    for method in ["year", "nope"] {
        witness(
            &format!(
                "def run; t=Time.at(0); x=0; begin; t.{method}((while true; x=1; break 7; end)); rescue; x; end; end"
            ),
            Some("1"),
            true,
        );
    }
}

#[test]
fn native_value_call_results_and_temporary_parts_do_not_replace_the_receiver() {
    for (source, expected) in [
        (
            "def run; t=Time.at(0,in:\"UTC\"); s=t.strftime((while true; t=nil; break \"%Y\"; end)); [s,t]; end",
            "[1970, nil]",
        ),
        (
            "def run; d=Duration.build(7); parts=d.parts; parts[:seconds]=9; [parts.seconds,d.seconds]; end",
            "[9, 7]",
        ),
        (
            "def run; d=Duration.build(7); d.parts[:seconds]=9; d.seconds; end",
            "7",
        ),
        (
            "def run; d=Duration.build(7); d.parts[:seconds]+=1; d.seconds; end",
            "7",
        ),
        (
            "def run; r=/a/; matched=r.match?((while true; r=nil; break \"a\"; end)); [matched,r]; end",
            "[true, nil]",
        ),
    ] {
        witness(source, Some(expected), false);
    }
}

#[test]
fn skipped_upper_comparisons_keep_normal_results_beside_diagnostics() {
    for (receiver, low) in [
        (TIME, "Time.at(10)"),
        (DURATION, "Duration.build(200000)"),
        (MONEY, "money_cents(999,\"USD\")"),
    ] {
        witness(
            &format!("def run; {receiver}.between?({low},nil); end"),
            Some("false"),
            true,
        );
    }
}

#[test]
fn temporal_arithmetic_retains_actual_exception_classes_without_spurious_handlers() {
    for (setup, expr) in [
        ("a=Time.at(0);b=Time.at(1);", "a<b"),
        ("a=Duration.build(7);b=Duration.build(8);", "a<b"),
        ("a=money_cents(7,\"USD\");b=money_cents(8,\"EUR\");", "a==b"),
        ("a=Time.at(0);b=Time.at(1);", "a<=>b"),
        (
            "a=money_cents(7,\"USD\");b=money_cents(8,\"EUR\");",
            "a<=>b",
        ),
    ] {
        witness(
            &format!("def run -> int; {setup} begin; {expr}; rescue; missing; end; 7; end"),
            Some("7"),
            false,
        );
    }
    for source in [
        "def run; a=money_cents(7,\"USD\"); begin; a/0; rescue ZeroDivisionError; missing; rescue; 7; end; end",
        "def run; a=money_cents(7,\"USD\"); b=money_cents(8,\"EUR\"); begin; a<b; rescue ArgumentError; 7; end; end",
        "def run; d=Duration.build(7); begin; d/0; rescue ZeroDivisionError; 7; end; end",
        "def run; d=Duration.build(7); begin; d/0.0; rescue ZeroDivisionError; 7; end; end",
        "def run; t=Time.at(0); begin; t+9223372036854775807; rescue; 7; end; end",
    ] {
        witness(source, Some("7"), false);
    }
}

#[test]
fn value_input_limits_remain_ordinary_errors_without_running_formatters_in_analysis() {
    for expression in [
        "Time.at(0).iso8601(101)",
        "Time.at(0).strftime(\"%1048577Y\")",
        "/a/.match?((\"x\"*1024)*1025)",
        "/a/ =~ (\"x\"*1024)*1025",
    ] {
        witness(
            &format!("def run; begin; {expression}; rescue LimitError; \"caught\"; end; end"),
            Some("caught"),
            false,
        );
    }
}

#[test]
fn value_methods_keep_known_bad_inputs_beside_gradual_alternatives() {
    for source in [
        "def run(p:int|any); Time.at(0).round(p); end",
        "def run(p:string|any); Time.at(0).format(p); end",
        "def run(p:time|any); Duration.build(7).after(p); end",
        "def run(p:time|duration); p.to_s; end",
    ] {
        check(source, false);
    }
    for source in [
        "def run(p:bool|any); Time.at(0).round(p); end",
        "def run(p:bool|any); Time.at(0).format(p); end",
        "def run(p:bool|any); Duration.build(7).after(p); end",
        "def run(p:bool|any); /a/.match?(p); end",
        "def run -> int; Time.at(0).zone; end",
        "def run -> int; Duration.build(7).in_seconds; end",
    ] {
        check(source, true);
    }
}

#[test]
fn match_data_blocks_forwarding_and_introspection_remain_explicitly_incomplete() {
    for source in [
        "def run; /a/.match(\"a\").captures; end",
        "def run; Time.at(0).respond_to?(:year); end",
        "def run; Time.at(0).send(:strftime,\"%Y\"); end",
        "def run; Duration.build(7).after {7}; end",
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
fn primitive_operator_facts_contain_temporal_regex_and_comparison_results() {
    let mut ctx = CallContext::new(CallOptions::default());
    let regex = crate::regex::value::Regex::compile(&mut ctx, Value::bytes(b"a"), 0).unwrap();
    let values = [
        Value::nil(),
        Value::boolean(false),
        Value::int(-1),
        Value::int(0),
        Value::int(2),
        Value::float(0.5),
        Value::bytes(b"abc"),
        Value::symbol(b"a"),
        Value::duration(7),
        Value::time(0, 0).unwrap(),
        Value::money(7, "USD").unwrap(),
        Value::money(8, "EUR").unwrap(),
        regex,
    ];
    let mut count = 0;
    for left in &values {
        for right in &values {
            for op in [
                "+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">=", "<=>", "=~", "!~",
            ] {
                let mut facts = Facts::new(&mut ctx).unwrap();
                let a = literal_fact(&mut ctx, &mut facts, left);
                let b = literal_fact(&mut ctx, &mut facts, right);
                let inferred = facts.scalar_binary(&mut ctx, op, a, b).unwrap();
                assert!(!inferred.unsupported, "{left:?} {op} {right:?}");
                let actual = crate::ops::binary(
                    &mut CallContext::new(CallOptions::default()),
                    op,
                    left.clone(),
                    right.clone(),
                );
                if let Ok(value) = actual {
                    assert!(!inferred.rejected, "{left:?} {op} {right:?}");
                    let concrete = literal_fact(&mut ctx, &mut facts, &value);
                    assert_ne!(
                        facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                        Relation::Rejected,
                        "{left:?} {op} {right:?}"
                    );
                }
                count += 1;
            }
        }
    }
    assert_eq!(count, 2366);
    drop(values);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn native_member_facts_contain_runtime_values_and_ordinary_errors() {
    let mut owner = CallContext::new(CallOptions::default());
    let regex = crate::regex::value::Regex::compile(&mut owner, Value::bytes(b"a"), 0).unwrap();
    let values = [
        Value::time(0, 125_000_000).unwrap(),
        Value::duration(93784),
        Value::money(125, "USD").unwrap(),
        regex,
        Value::int(7),
        Value::float(0.5),
    ];
    let arguments = [
        vec![],
        vec![Value::nil()],
        vec![Value::boolean(true)],
        vec![Value::int(-1)],
        vec![Value::int(2)],
        vec![Value::int(101)],
        vec![Value::float(0.5)],
        vec![Value::bytes(b"UTC")],
        vec![Value::bytes(b"%Y")],
        vec![Value::bytes(b"1970-01-01T00:00:00Z")],
        vec![values[0].clone(), values[0].clone()],
        vec![values[1].clone(), values[1].clone()],
        vec![values[2].clone(), values[2].clone()],
    ];
    let names = [
        "nil?",
        "itself",
        "dup",
        "clone",
        "freeze",
        "frozen?",
        "eql?",
        "equal?",
        "to_s",
        "string",
        "inspect",
        "between?",
        "seconds",
        "minutes",
        "hours",
        "days",
        "weeks",
        "to_i",
        "in_seconds",
        "in_minutes",
        "in_hours",
        "in_days",
        "in_weeks",
        "in_months",
        "in_years",
        "parts",
        "after",
        "since",
        "from_now",
        "ago",
        "before",
        "until",
        "currency",
        "cents",
        "amount",
        "format",
        "strftime",
        "iso8601",
        "xmlschema",
        "rfc3339",
        "httpdate",
        "rfc2822",
        "rfc822",
        "getlocal",
        "localtime",
        "round",
        "ceil",
        "floor",
        "getutc",
        "getgm",
        "utc",
        "gmtime",
        "year",
        "month",
        "mon",
        "day",
        "mday",
        "hour",
        "min",
        "sec",
        "wday",
        "yday",
        "nsec",
        "tv_nsec",
        "usec",
        "tv_usec",
        "hash",
        "tv_sec",
        "utc_offset",
        "gmt_offset",
        "gmtoff",
        "subsec",
        "to_f",
        "to_r",
        "zone",
        "utc?",
        "gmt?",
        "dst?",
        "isdst",
        "sunday?",
        "monday?",
        "tuesday?",
        "wednesday?",
        "thursday?",
        "friday?",
        "saturday?",
        "to_a",
        "<=>",
        "source",
        "flags",
        "match?",
        "nope",
    ];
    let mut cases = 0;
    for receiver in &values {
        for name in names {
            for args in &arguments {
                for auto in [false, true] {
                    if auto && !args.is_empty() {
                        continue;
                    }
                    for keywords in [false, true] {
                        if auto && keywords {
                            continue;
                        }
                        let mut ctx = CallContext::new(CallOptions::default());
                        let mut facts = Facts::new(&mut ctx).unwrap();
                        let root = literal_fact(&mut ctx, &mut facts, receiver);
                        let mut inputs = Arguments::new();
                        for arg in args {
                            let value = literal_fact(&mut ctx, &mut facts, arg);
                            inputs.positional.push(&mut ctx, value).unwrap();
                        }
                        if keywords {
                            let key = facts.symbol(&mut ctx, b"extra").unwrap();
                            let value = facts.boolean(&mut ctx, true).unwrap();
                            inputs.keyword(&mut ctx, key, value).unwrap();
                        }
                        let site = CallSite {
                            name: 0,
                            method: crate::bytecode::Method::parse(name),
                            auto,
                            parenthesized: !auto,
                            scope: false,
                        };
                        let Some(inferred) =
                            builtins::member(&mut ctx, &mut facts, root, site, name, &inputs)
                                .unwrap()
                        else {
                            continue;
                        };
                        assert!(!inferred.incomplete, "{receiver:?}.{name}({args:?})");
                        let mut runtime = CallContext::new(CallOptions::default());
                        let mut actual_args =
                            crate::arguments::Arguments::from_values(&mut runtime, args).unwrap();
                        if keywords {
                            actual_args
                                .keywords
                                .insert(&mut runtime, Value::bytes(b"extra"), Value::boolean(true))
                                .unwrap();
                        }
                        let actual = crate::members::call_keywords(
                            &mut runtime,
                            site,
                            name,
                            receiver.clone(),
                            &actual_args,
                        );
                        let label = format!(
                            "{receiver:?}.{name}({args:?}), auto={auto}, keywords={keywords}"
                        );
                        match actual {
                            Ok((_, value)) => {
                                assert!(
                                    inferred.failures.data.is_empty(),
                                    "{label}: {:?}",
                                    facts.node(inferred.value)
                                );
                                let concrete = literal_fact(&mut ctx, &mut facts, &value);
                                assert_ne!(
                                    facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                                    Relation::Rejected,
                                    "{label}: {:?}",
                                    facts.node(inferred.value)
                                );
                            }
                            Err(error) => {
                                let class = error.class().unwrap();
                                assert_ne!(
                                    inferred.throws & (1 << class as u8),
                                    0,
                                    "{label}: {error}; throws={}",
                                    inferred.throws
                                );
                            }
                        }
                        drop((inferred, inputs, facts, actual_args));
                        assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
                        assert_eq!(runtime.stats().retained_memory_bytes, 0, "{label}");
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 10_314);
    drop((arguments, values));
    assert_eq!(owner.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run -> string; t=Time.at(0,in:\"UTC\"); d=Duration.build(7); begin; s=t.strftime(\"%Y\"); t.iso8601(2); m=money_cents(7,\"USD\"); r=/a/; r.match?(s); parts=d.parts; parts[:seconds]+=1; m.format; rescue; \"failed\"; ensure; t.year; end; end";
    let report = analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn value_method_analysis_obeys_exact_quotas_and_reclaims_failed_allocations() {
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
fn value_method_analysis_preserves_latched_cancellation_and_deadlines() {
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
fn value_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-values.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 113);
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
    assert_eq!(differences, 35);
}
