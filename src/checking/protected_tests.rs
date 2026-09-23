use super::{builtin_tests::check, native_tests::witness};

fn matched(body: &str, expected: &str, rejected: bool) {
    witness(
        &format!("def run; m=/(?<first>a)(b)?/.match(\"xa!\"); if m; {body}; end; end"),
        Some(expected),
        rejected,
    );
}

#[test]
fn match_data_reads_captures_names_and_offsets() {
    for (body, expected) in [
        ("m.to_s", "a"),
        ("m[0]", "a"),
        ("m[1]", "a"),
        ("m[2]", "nil"),
        ("m[-1]", "nil"),
        ("m[-3]", "a"),
        ("m[99]", "nil"),
        ("m[:first]", "a"),
        ("m[:missing]", "nil"),
        ("m.captures", "[a, nil]"),
        ("m.named_captures[:first]", "a"),
        ("[m.pre_match,m.post_match]", "[x, !]"),
        ("m.begin(0)", "1"),
        ("m.end(0)", "2"),
        ("m.begin(1)", "1"),
        ("m.end(2)", "nil"),
        ("m.begin(-1)", "nil"),
        ("m[:begin](0)", "1"),
        ("(m[:end])(0)", "2"),
        ("f=m[:begin]; f(0)", "1"),
        ("m.dup.to_s", "a"),
        ("m.clone.to_s", "a"),
        ("m.freeze.to_s", "a"),
        ("m.itself.to_s", "a"),
        ("m.frozen?", "true"),
        ("m.nil?", "false"),
        ("format(\"%s!\",m)", "a!"),
        ("format(\"!%s\",m)", "!a"),
        ("format(\"%s\",m)", "a"),
    ] {
        matched(body, expected, false);
    }
}

#[test]
fn protected_ancestry_blocks_writes_and_keeps_previous_argument_effects() {
    for operation in [
        "m.clear",
        "m.replace({})",
        "m.delete(:missing)",
        "m.store(:x,1)",
        "m[:to_s]=7",
        "m.to_s=7",
        "m.captures.push(7)",
        "m.captures.clear",
        "m.captures[0]=7",
        "m.named_captures[:first]=7",
        "m.dup.captures.push(7)",
        "m.clone.captures[0]=7",
        "m.itself.named_captures.clear",
        "[m][0].captures.push(7)",
    ] {
        matched(
            &format!("begin; {operation}; rescue; m.to_s; end"),
            "a",
            true,
        );
    }
    matched(
        "x=0; begin; m.captures.push((while true; x=7; break 9; end)); rescue; x; end",
        "7",
        true,
    );
}

#[test]
fn detached_children_are_independent_and_root_duplicates_keep_protection() {
    for (body, expected) in [
        (
            "captures=m.captures; captures.push(7); [captures,m.captures]",
            "[[a, nil, 7], [a, nil]]",
        ),
        (
            "names=m.named_captures; names[:first]=7; [names[:first],m[:first]]",
            "[7, a]",
        ),
        ("m.captures.dup.push(7); m.captures", "[a, nil]"),
        (
            "copy=m.dup; begin; copy.captures.push(7); rescue; copy.to_s; end",
            "a",
        ),
    ] {
        matched(body, expected, body.contains("rescue"));
    }
}

#[test]
fn offset_calls_reject_invalid_arguments_and_bare_reads() {
    for body in [
        "m.begin",
        "m.begin(3)",
        "m.end(-4)",
        "m.begin(nil)",
        "m.end(0,bad:true)",
        "m.begin()",
        "f=m[:begin]; f",
        "f=m[:begin]; f.call(0)",
        "m.to_s()",
        "m.captures()",
    ] {
        matched(&format!("begin; {body}; rescue; 7; end"), "7", true);
    }
}

#[test]
fn string_matches_preserve_protected_results_and_offset_contracts() {
    for (expression, expected) in [
        ("\"xa\".match(/a/)", "a"),
        ("\"xa\".match(\"a\")", "a"),
        ("\"xa\".match(/a/,-1)", "a"),
        ("\"xa\".match(/a/,-99)", "nil"),
        ("\"xa\".match(/a/,99)", "nil"),
        ("\"xa\".match?(/a/)", "true"),
        ("\"xa\".match?(\"a\",1)", "true"),
        ("\"xa\".match?(/a/,-0.5)", "true"),
    ] {
        witness(
            &format!("def run; {expression}; end"),
            Some(expected),
            false,
        );
    }
    for expression in [
        "\"xa\".match?(/a/,-1)",
        "\"xa\".match(/a/,nil)",
        "\"xa\".match(7)",
        "\"xa\".match(/a/,bad:true)",
    ] {
        witness(
            &format!("def run; begin; {expression}; rescue; 7; end; end"),
            Some("7"),
            true,
        );
    }
}

#[test]
fn protected_error_values_keep_read_only_paths_and_independent_copies() {
    for (body, expected, rejected) in [
        ("error.message", "bad", false),
        ("error.dup.message", "bad", false),
        ("format(\"%s!\",error)", "bad!", false),
        (
            "begin; error.backtrace.clear; rescue; error.message; end",
            "bad",
            true,
        ),
        (
            "trace=error.backtrace; trace.clear; error.message",
            "bad",
            false,
        ),
    ] {
        witness(
            &format!("def run; begin; raise \"bad\"; rescue => error; {body}; end; end"),
            Some(expected),
            rejected,
        );
    }
}

#[test]
fn protected_values_preserve_typed_fields_through_script_calls() {
    check(
        "def capture(x:{captures:array<string?>,...}) -> string?; x.captures[0]; end; def run; m=/(a)/.match(\"a\"); if m; capture(m); end; end",
        false,
    );
    witness(
        "def copy(x); x; end; def run; m=/a/.match(\"a\"); if m; n=copy(m); begin; n.clear; rescue; n.to_s; end; end; end",
        Some("a"),
        true,
    );
}

#[test]
fn protected_errors_keep_evaluation_order_and_original_selected_objects() {
    matched(
        "x=0; begin; m.nope((while true; x=7; break 9; end)); rescue; x; end",
        "0",
        true,
    );
    matched(
        "x=0; begin; m.captures((while true; x=7; break 9; end)); rescue; x; end",
        "7",
        true,
    );
    matched(
        "x=0; begin; m.captures.push(bad:(while true; x=7; break 9; end)); rescue; x; end",
        "7",
        true,
    );
    matched(
        "begin; m.captures.push((while true; m=nil; break 9; end)); rescue; m; end",
        "nil",
        true,
    );
    matched("begin; m+\"!\"; rescue; 7; end", "7", true);
    matched("begin; JSON.stringify(m); rescue; 7; end", "7", true);
    witness("def run; /a/.match(\"a\").captures; end", Some("[]"), true);
}

#[test]
fn match_fields_participate_in_plain_iteration_and_keyword_copying() {
    matched(
        "names=[]; for key,value in m; names.push(key); end; names.length",
        "7",
        false,
    );
    witness(
        "def read(**data); data[:pre_match]; end; def run; m=/a/.match(\"xa\"); if m; read(**m); end; end",
        Some("x"),
        false,
    );
    matched("unless m.nil?; m[:first]; end", "a", false);
}

#[test]
fn protected_and_mutable_branch_results_keep_both_mutation_outcomes() {
    for flag in ["false", "true"] {
        witness(
            &format!(
                "def run(flag:bool={flag}); m=/(a)/.match(\"a\"); if m; value=if flag; m; else; {{captures:[\"x\"]}}; end; begin; value.captures.push(7); rescue; value.captures; end; end; end"
            ),
            Some(if flag == "true" { "[a]" } else { "[x, 7]" }),
            true,
        );
    }
    witness(
        "def copy(x:hash) -> hash; x; end; def run; m=/(a)/.match(\"a\"); if m; n=copy(m); begin; n.captures.clear; rescue; n.to_s; end; end; end",
        Some("a"),
        true,
    );
}

fn accounting(ctx: &mut crate::CallContext) -> crate::Result<()> {
    let mut facts = super::facts::Facts::new(ctx)?;
    let source = "def run; m=/(?<first>a)(b)?/.match(\"xa!\"); if m; begin; f=m[:begin]; start=f(0); copy=m.dup; copy.captures.push(start); rescue => error; trace=error.backtrace; trace.clear; format(\"%s\",copy); ensure; m.end(0); end; end; end";
    let report = super::collection_tests::analyze(ctx, &mut facts, source)?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(!report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn protected_fact_and_address_storage_obey_exact_quotas_and_reclaim_failures() {
    use crate::{CallContext, CallOptions, ErrorKind, Limits};
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
fn protected_analysis_preserves_latched_cancellation_and_deadlines() {
    use crate::{CallContext, CallOptions, ErrorKind};
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
fn bare_hash_contracts_preserve_known_fields_without_inventing_error_paths() {
    witness(
        "def copy(x:hash) -> hash; x; end; def run -> int; begin; copy({count:7}).count; rescue; missing; end; end",
        Some("7"),
        false,
    );
    witness(
        "def copy(x:hash) -> hash; x; end; def run; value=copy({items:[1]}); value.items.push(2); value.items; end",
        Some("[1, 2]"),
        false,
    );
    witness(
        "def copy(x:hash) -> hash; x; end; def run; begin; copy(7); rescue; 7; end; end",
        Some("7"),
        true,
    );
}

#[test]
fn match_summaries_inspect_metadata_without_running_searches_or_patterns() {
    use super::{
        arguments::Arguments,
        builtins,
        facts::{Atom, Facts},
        relation::Relation,
    };
    use crate::{CallContext, CallOptions, Value};
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let regex = crate::regex::value::Regex::compile(
        &mut ctx,
        Value::bytes(b"(?<x>a)(b)?"),
        0,
        "Regexp.new",
    )
    .unwrap();
    let regex = facts.regex(&mut ctx, regex).unwrap();
    let string = facts.string(&mut ctx, &vec![b'['; 20_000]).unwrap();
    let mut args = Arguments::new();
    args.positional.push(&mut ctx, string).unwrap();
    let site = crate::bytecode::CallSite {
        name: 0,
        method: None,
        auto: false,
        parenthesized: true,
        scope: false,
    };
    let before = ctx.stats().steps;
    let result = builtins::member(&mut ctx, &mut facts, regex, site, "match", &args)
        .unwrap()
        .unwrap();
    assert!(!result.incomplete);
    assert!(ctx.stats().steps - before < 1000);
    assert_ne!(
        facts
            .relation(&mut ctx, Atom::Nil.fact(), result.value)
            .unwrap(),
        Relation::Rejected
    );
    let receiver = facts.string(&mut ctx, b"a").unwrap();
    let before = ctx.stats().steps;
    let result = builtins::member(&mut ctx, &mut facts, receiver, site, "match", &args)
        .unwrap()
        .unwrap();
    assert!(!result.incomplete);
    assert!(ctx.stats().steps - before < 1000);
    drop((result, args, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn specimens() -> Vec<crate::Value> {
    [
        "def run; /a/.match(\"xa!\"); end",
        "def run; /(?<first>a)(b)?/.match(\"xa!\"); end",
        "def run; /(?<same>水)|(?<same>b)/.match(\"é水!\"); end",
        "def run; begin; raise \"bad\"; rescue => error; error; end; end",
    ]
    .into_iter()
    .map(|source| {
        crate::Engine::new()
            .compile(source)
            .unwrap()
            .call("run", &[], crate::CallOptions::default())
            .unwrap()
            .value
    })
    .collect()
}

fn input_fact(
    ctx: &mut crate::CallContext,
    facts: &mut super::facts::Facts,
    value: &crate::Value,
) -> super::facts::Fact {
    if let crate::value::Kind::Float(n) = value.0 {
        facts.float(ctx, n).unwrap()
    } else {
        super::collection_tests::literal_fact(ctx, facts, value)
    }
}

#[test]
fn protected_member_facts_contain_runtime_values_and_ordinary_errors() {
    use super::{
        arguments::Arguments, builtins, collection_tests::literal_fact, facts::Facts,
        relation::Relation,
    };
    use crate::{CallContext, CallOptions, Value, bytecode::CallSite};
    let arguments = [
        vec![],
        vec![Value::nil()],
        vec![Value::boolean(true)],
        vec![Value::int(-4)],
        vec![Value::int(-1)],
        vec![Value::int(0)],
        vec![Value::int(1)],
        vec![Value::int(3)],
        vec![Value::float(f64::NAN)],
        vec![Value::float(f64::NEG_INFINITY)],
        vec![Value::float(1.5)],
        vec![Value::bytes(b"missing")],
        vec![Value::symbol(b"first")],
        vec![Value::array(vec![])],
        vec![Value::int(0), Value::int(1)],
    ];
    let names = [
        "begin",
        "end",
        "captures",
        "named_captures",
        "pre_match",
        "post_match",
        "to_s",
        "message",
        "backtrace",
        "code_frame",
        "type",
        "class",
        "length",
        "size",
        "empty?",
        "keys",
        "values",
        "itself",
        "dup",
        "clone",
        "freeze",
        "frozen?",
        "nil?",
        "eql?",
        "equal?",
        "inspect",
        "clear",
        "store",
        "delete",
        "replace",
        "missing",
    ];
    let mut cases = 0;
    for receiver in specimens() {
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
                        for scope in [false, true] {
                            let mut ctx = CallContext::new(CallOptions::default());
                            let mut facts = Facts::new(&mut ctx).unwrap();
                            let root = literal_fact(&mut ctx, &mut facts, &receiver);
                            let mut inputs = Arguments::new();
                            for arg in args {
                                let value = input_fact(&mut ctx, &mut facts, arg);
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
                                scope,
                            };
                            let inferred =
                                builtins::member(&mut ctx, &mut facts, root, site, name, &inputs)
                                    .unwrap()
                                    .unwrap();
                            let label = format!(
                                "{receiver:?}.{name}({args:?}), auto={auto}, keywords={keywords}, scope={scope}"
                            );
                            assert!(!inferred.incomplete, "{label}");
                            let mut runtime = CallContext::new(CallOptions::default());
                            let mut actual_args =
                                crate::arguments::Arguments::from_values(&mut runtime, args)
                                    .unwrap();
                            if keywords {
                                actual_args
                                    .keywords
                                    .insert(
                                        &mut runtime,
                                        Value::bytes(b"extra"),
                                        Value::boolean(true),
                                    )
                                    .unwrap();
                            }
                            match crate::members::call_keywords(
                                &mut runtime,
                                site,
                                name,
                                receiver.clone(),
                                &actual_args,
                            ) {
                                Ok((_, value)) => {
                                    assert!(inferred.failures.data.is_empty(), "{label}");
                                    let concrete = literal_fact(&mut ctx, &mut facts, &value);
                                    assert_ne!(
                                        facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                                        Relation::Rejected,
                                        "{label}: {:?}",
                                        facts.node(inferred.value)
                                    );
                                }
                                Err(error) => assert_ne!(
                                    inferred.throws & (1 << error.class().unwrap() as u8),
                                    0,
                                    "{label}: {error}"
                                ),
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
    }
    assert_eq!(cases, 7688);
}

#[test]
fn protected_index_facts_contain_runtime_values() {
    use super::{collection_tests::literal_fact, facts::Facts, relation::Relation};
    use crate::{CallContext, CallOptions, Value};
    let selectors = [
        Value::nil(),
        Value::boolean(true),
        Value::int(i64::MIN),
        Value::int(-4),
        Value::int(-1),
        Value::int(0),
        Value::int(1),
        Value::int(3),
        Value::int(i64::MAX),
        Value::float(1.5),
        Value::float(f64::NAN),
        Value::float(f64::INFINITY),
        Value::bytes(b"first"),
        Value::symbol(b"same"),
        Value::bytes(b"captures"),
        Value::symbol(b"begin"),
        Value::bytes(b"message"),
        Value::symbol(b"backtrace"),
        Value::bytes(b"missing"),
        Value::array(vec![]),
    ];
    let mut cases = 0;
    for receiver in specimens() {
        for selector in &selectors {
            for length in [
                None,
                Some(Value::int(-1)),
                Some(Value::int(1)),
                Some(Value::nil()),
            ] {
                let mut ctx = CallContext::new(CallOptions::default());
                let mut facts = Facts::new(&mut ctx).unwrap();
                let root = literal_fact(&mut ctx, &mut facts, &receiver);
                let mut args = vec![selector.clone()];
                args.extend(length);
                let inputs: Vec<_> = args
                    .iter()
                    .map(|arg| input_fact(&mut ctx, &mut facts, arg))
                    .collect();
                let inferred = facts.collection_index(&mut ctx, root, &inputs).unwrap();
                assert!(!inferred.unsupported, "{receiver:?}[{args:?}]");
                let mut runtime = CallContext::new(CallOptions::default());
                let actual = if args.len() == 1 {
                    crate::ops::index(&mut runtime, &receiver, selector)
                } else {
                    crate::sequence::slice(&mut runtime, &receiver, &args, false, None)
                };
                if let Ok(value) = actual {
                    assert!(!inferred.rejected, "{receiver:?}[{args:?}]");
                    let concrete = literal_fact(&mut ctx, &mut facts, &value);
                    assert_ne!(
                        facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                        Relation::Rejected,
                        "{receiver:?}[{args:?}]: {:?}",
                        facts.node(inferred.value)
                    );
                }
                drop(facts);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
                assert_eq!(runtime.stats().retained_memory_bytes, 0);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 320);
}

#[test]
fn matching_loops_and_calls_preserve_protected_metadata_and_value_copies() {
    for (source, expected, rejected) in [
        (
            "def run; m=/(?<same>水)|(?<same>b)/.match(\"é水!\"); if m; [m[:same],m.named_captures[:same],m.begin(1),m.end(1),m.end(2)]; end; end",
            "[水, 水, 1, 2, nil]",
            false,
        ),
        (
            "def read(m:{captures:array<string?>,...}) -> string?; m.captures[-1]; end; def run; m=/(a)(b)?/.match(\"a\"); if m; read(m); end; end",
            "nil",
            false,
        ),
        (
            "def run; m=nil; n=0; while n<3; m=/(a)/.match(\"a\"); n+=1; end; if m; begin; m.captures.clear; rescue; m.to_s; end; end; end",
            "a",
            true,
        ),
        (
            "def match(n); if n>0; match(n-1); else; /(a)/.match(\"a\"); end; end; def run; m=match(3); if m; m.captures; end; end",
            "[a]",
            false,
        ),
        (
            "def make; m=/(a)/.match(\"xa\"); if m; m[:begin]; end; end; def run; f=make(); f(0); end",
            "1",
            true,
        ),
        (
            "def run; begin; \"a\".match(\"[\"); rescue; 7; end; end",
            "7",
            false,
        ),
        (
            "def run; begin; /a/.match((\"x\"*1024)*1025); rescue LimitError; 7; end; end",
            "7",
            false,
        ),
    ] {
        witness(source, Some(expected), rejected);
    }
}

#[test]
fn protected_reference_decisions_have_runtime_witnesses() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/checker-protected.json")).unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 108);
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
    assert_eq!(differences, 51);
}

#[test]
fn joined_protected_receivers_keep_their_addressed_ancestry() {
    for (flag, expected) in [("true", "a"), ("false", "ab")] {
        witness(
            &format!(
                "def run(flag:bool={flag}); m=if flag; /(a)/.match(\"a\"); else; /(a)(b)/.match(\"ab\"); end; if m; begin; m.captures.push(7); rescue; m.to_s; end; end; end"
            ),
            Some(expected),
            true,
        );
    }
    witness(
        "def run; begin; \"a\".match(\"[\"); rescue => error; begin; error.backtrace.clear; rescue; 7; end; end; end",
        Some("7"),
        true,
    );
    witness(
        "def run(flag:bool=true); m=if flag; /(a)/.match(\"a\"); else; /(a)(b)/.match(\"ab\"); end; if m; x=0; begin; m.begin.push((while true; x=7; break 9; end)); rescue; x; end; end; end",
        Some("7"),
        true,
    );
}

#[test]
fn unknown_patterns_still_produce_plain_named_capture_hashes() {
    for expression in [
        "\"a\".match(\"(?<x>a)\")",
        "Regexp.new(\"(?<x>a)\").match(\"a\")",
    ] {
        witness(
            &format!("def run; m={expression}; if m; m.named_captures.keys; end; end"),
            Some("[x]"),
            false,
        );
        witness(
            &format!(
                "def run; m={expression}; if m; names=m.named_captures; names[:x]=7; names[:x]; end; end"
            ),
            Some("7"),
            false,
        );
    }
}
