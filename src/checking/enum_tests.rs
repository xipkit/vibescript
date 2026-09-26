use super::{
    arguments,
    calls::{self, Analysis, Target, World},
    collection_tests::analyze,
    facts::{Atom, Facts, Node},
    native_tests::witness,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, Limits, Result, Value, budget::Buffer, bytecode,
};

const SOURCE: &str = "enum Status; Draft; Sent; HTTPServer; end; enum Review; Draft; end;";

#[test]
fn source_enums_keep_member_properties_and_aliases_through_calls() {
    for (expression, expected) in [
        ("Status.name", "Status"),
        ("Status::Draft.name", "Draft"),
        ("Status::HTTPServer.symbol", "http_server"),
        ("Status::Draft.enum.name", "Status"),
        ("Status.to_s", "<Enum Status>"),
        ("Status::Draft.inspect", "Status::Draft"),
        ("Status::Draft.string", "Status::Draft"),
        ("id(Status)::Sent.name", "Sent"),
        ("id(Status::Draft).enum::Sent.symbol", "sent"),
        ("a=Status; a::Draft.name", "Draft"),
        ("a=Status::Draft; a.dup.enum.name", "Status"),
        ("[Status,Review][0]::HTTPServer.name", "HTTPServer"),
        ("{status:Status::Draft}.status.enum.name", "Status"),
        ("Status=Review; Status::Draft.enum.name", "Review"),
        ("Status=7; Status", "7"),
        ("[Status,Review].map { |e| e.name }", "[Status, Review]"),
        (
            "[Status::Draft,Status::Sent].map { |v| v.symbol }",
            "[draft, sent]",
        ),
        ("Status::Draft.tap { |v| v.name }.symbol", "draft"),
        ("Status::Draft.yield_self { |v| v.enum.name }", "Status"),
    ] {
        witness(
            &format!("{SOURCE} def id(x); x; end; def run; {expression}; end"),
            Some(expected),
            false,
        );
    }
}

#[test]
fn enum_helpers_and_forwarding_preserve_identity_and_flag_errors() {
    for receiver in ["Status", "Status::Draft"] {
        for (method, expected) in [
            ("nil?", "false"),
            ("frozen?", "true"),
            ("respond_to?(:name)", "true"),
            ("respond_to?(:missing)", "false"),
            ("is_type?(:symbol)", "false"),
        ] {
            witness(
                &format!("{SOURCE} def run; ({receiver}).{method}; end"),
                Some(expected),
                false,
            );
        }
        for method in ["dup", "itself", "clone", "freeze"] {
            witness(
                &format!("{SOURCE} def run; ({receiver}).{method}.name; end"),
                Some(if receiver == "Status" {
                    "Status"
                } else {
                    "Draft"
                }),
                false,
            );
        }
        for method in ["to_s", "string", "inspect"] {
            for call in [
                format!("({receiver}).{method}()"),
                format!("({receiver}).send(:{method})"),
                format!("({receiver}).public_send(:send,:{method})"),
            ] {
                witness(
                    &format!("{SOURCE} def run; {call}; end"),
                    Some(if receiver == "Status" {
                        "<Enum Status>"
                    } else {
                        "Status::Draft"
                    }),
                    false,
                );
            }
            for suffix in ["(1)", "(a:1)", " { raise(\"must not run\") }"] {
                witness(
                    &format!(
                        "{SOURCE} def run; begin; ({receiver}).{method}{suffix}; rescue; 99; end; end"
                    ),
                    Some("99"),
                    true,
                );
            }
            for suffix in [",1", ",a:1"] {
                witness(
                    &format!(
                        "{SOURCE} def run; begin; ({receiver}).send(:{method}{suffix}); rescue; 99; end; end"
                    ),
                    Some("99"),
                    true,
                );
            }
            witness(
                &format!(
                    "{SOURCE} def run; begin; ({receiver}).send(:{method}) {{ raise(\"must not run\") }}; rescue; 99; end; end"
                ),
                Some("99"),
                true,
            );
        }
    }
}

#[test]
fn enum_members_are_data_and_invalid_operations_remain_catchable() {
    for expression in [
        "Status()",
        "Status::Draft()",
        "Status.name()",
        "Status::Draft.name()",
        "Status::Draft.symbol()",
        "Status::Draft.enum()",
        "Status::Missing",
        "Status.Draft",
        "Status::Draft::Draft",
        "Status[0]",
        "Status[:Draft]",
        "Status::Draft[0]",
        "Status::Draft.to_i",
        "Status::Draft.missing",
        "Status.send(:name)",
        "Status::Draft.send(:symbol)",
        "Status::Draft.public_send(:enum)",
        "Status::Draft + 1",
        "Status + \"x\"",
        "-Status::Draft",
        "Status::Draft < Status::Sent",
        "JSON.stringify(Status)",
        "JSON.stringify([Status])",
        "JSON.parse_as(\"1\",Status)",
        "[Status::Draft,Status::Draft].reduce(:==)",
    ] {
        witness(
            &format!("{SOURCE} def run; begin; {expression}; rescue; 99; end; end"),
            Some("99"),
            true,
        );
    }
}

#[test]
fn enum_equality_case_collections_and_serialization_use_member_identity() {
    for (expression, expected) in [
        ("Status == Status", "true"),
        ("Status == Review", "false"),
        ("Status::Draft == Status::Draft", "true"),
        ("Status::Draft == Status::Sent", "false"),
        ("Status::Draft == Review::Draft", "false"),
        ("Status::Draft != :draft", "true"),
        ("Status::Draft == [Status::Draft]", "false"),
        ("Status::Draft === Status::Draft", "true"),
        ("Status::Draft.eql?(Status::Draft)", "true"),
        ("Status::Draft.equal?(Review::Draft)", "false"),
        ("Status::Draft.send(:eql?,Status::Draft)", "true"),
        ("[Status::Draft,Status::Draft].reduce(:eql?)", "true"),
        ("[Status::Draft,Review::Draft].reduce(:equal?)", "false"),
        (
            "case Status::Draft; when Status::Sent; 1 + nil; else; 7; end",
            "7",
        ),
        (
            "case Status::Draft; when Status::Sent; 2; when Status::Draft; 7; end",
            "7",
        ),
        (
            "[Status::Draft,Status::Draft,Status::Sent,Review::Draft,:draft].uniq.length",
            "4",
        ),
        (
            "[Status::Draft,Status::Sent].include?(Status::Draft)",
            "true",
        ),
        ("[Status::Draft,Status::Sent].include?(:draft)", "false"),
        (
            "[Status::Draft,Status::Sent].delete(Status::Draft).name",
            "Draft",
        ),
        ("Status::Draft + \"!\"", "Status::Draft!"),
        ("\"!\" + Status::Draft", "!Status::Draft"),
        ("Status::Draft <=> Status::Sent", "nil"),
        ("JSON.stringify(Status::Draft)", "\"draft\""),
        (
            "JSON.stringify({status:Status::HTTPServer})",
            "{\"status\":\"http_server\"}",
        ),
    ] {
        witness(
            &format!("{SOURCE} def run; {expression}; end"),
            Some(expected),
            false,
        );
    }
}

#[test]
fn enum_facts_preserve_nominal_contracts_and_distinct_singletons() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let status = facts
        .enumeration(&mut ctx, &program.declarations[0])
        .unwrap();
    let again = facts
        .enumeration(&mut ctx, &program.declarations[0])
        .unwrap();
    assert_eq!(status, again);
    let draft = facts.enum_member(&mut ctx, status, 0).unwrap();
    let sent = facts.enum_member(&mut ctx, status, 1).unwrap();
    let Node::Enumeration { nominal, .. } = facts.node(status) else {
        unreachable!()
    };
    let nominal = *nominal;
    for member in [draft, sent] {
        assert_eq!(
            facts.relation(&mut ctx, member, nominal).unwrap(),
            Relation::Accepted
        );
        assert_eq!(
            facts.relation(&mut ctx, nominal, member).unwrap(),
            Relation::Gradual
        );
        assert_eq!(facts.normalized(&mut ctx, member, nominal).unwrap(), member);
        let nullable = facts.union(&mut ctx, &[nominal, Atom::Nil.fact()]).unwrap();
        assert_eq!(
            facts.normalized(&mut ctx, member, nullable).unwrap(),
            member
        );
        assert_eq!(
            facts
                .relation(&mut ctx, member, Atom::Symbol.fact())
                .unwrap(),
            Relation::Rejected
        );
    }
    assert_eq!(
        facts.relation(&mut ctx, status, nominal).unwrap(),
        Relation::Rejected
    );
    assert_eq!(
        facts.relation(&mut ctx, draft, sent).unwrap(),
        Relation::Rejected
    );
    assert_eq!(facts.definitely_equal(status, draft), Some(false));
    assert_eq!(facts.definitely_equal(draft, sent), Some(false));
    let text = facts.symbol(&mut ctx, b"draft").unwrap();
    assert_eq!(facts.definitely_equal(draft, text), Some(false));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn class_initializers_throw_before_the_entry_body() {
    let expression = "class Widget; raise(\"must not run\"); end; def run; Widget; end";
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, expression).unwrap();
    assert!(
        report.incomplete.data.is_empty(),
        "{expression}: {report:?}"
    );
    assert_eq!(report.returns, Atom::Never.fact());
    assert_ne!(report.throws, 0);
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn checked_expression(expression: &str) {
    let source = format!(
        "{SOURCE} def run; begin; value=({expression}); value.nil?; \"ok\"; rescue; \"error\"; end; end"
    );
    let actual = Engine::legacy_unchecked()
        .compile(&source)
        .unwrap_or_else(|e| panic!("{source}: {e}"))
        .call("run", &[], CallOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, &source).unwrap();
    assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
    assert_eq!(
        !report.issues.data.is_empty(),
        actual.value.as_bytes() == Some(b"error"),
        "{source}: {report:?}"
    );
    let concrete = facts
        .string(&mut ctx, actual.value.as_bytes().unwrap())
        .unwrap();
    assert_ne!(
        facts.relation(&mut ctx, concrete, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn enum_member_dispatch_matches_executed_direct_scoped_and_forwarded_calls() {
    let mut cases = 0;
    for receiver in ["Status", "Status::Draft"] {
        for method in [
            "name",
            "symbol",
            "enum",
            "Draft",
            "Missing",
            "nil?",
            "itself",
            "dup",
            "freeze",
            "frozen?",
            "to_s",
            "string",
            "inspect",
            "respond_to?",
        ] {
            checked_expression(&format!("({receiver}).{method}"));
            cases += 1;
            for args in ["", "1", ":name", "key:1"] {
                let extra = if args.is_empty() {
                    String::new()
                } else {
                    format!(",{args}")
                };
                for call in [
                    format!("({receiver}).{method}({args})"),
                    format!("({receiver})::{method}({args})"),
                    format!("({receiver}).send(:{method}{extra})"),
                    format!("({receiver}).public_send(:send,:{method}{extra})"),
                ] {
                    for block in ["", " { 7 }"] {
                        checked_expression(&format!("{call}{block}"));
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 924);
}

#[test]
fn enum_operators_match_executed_native_values_on_both_sides() {
    let mut cases = 0;
    for enumeration in ["Status", "Status::Draft"] {
        for other in [
            "Status",
            "Status::Draft",
            "Status::Sent",
            "Review::Draft",
            "nil",
            "true",
            "0",
            "1.5",
            "\"%s\"",
            ":draft",
            "[]",
            "{}",
        ] {
            for op in [
                "+", "-", "*", "/", "//", "%", "**", "<", "<=", ">", ">=", "<=>", "==", "!=",
                "===", "=~", "!~", "&",
            ] {
                checked_expression(&format!("({enumeration}) {op} ({other})"));
                checked_expression(&format!("({other}) {op} ({enumeration})"));
                cases += 2;
            }
        }
    }
    assert_eq!(cases, 864);
}

fn analyze_program(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    globals: &[(Value, Target)],
) -> Result<Analysis> {
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(ctx, ty, |_, _| Ok(None))?;
        contracts.push(ctx, fact)?;
    }
    let function = program.names["run"];
    let inputs = arguments::general_inputs(
        ctx,
        facts,
        &program.functions[function].params,
        &contracts.data,
    )?;
    calls::analyze(
        ctx,
        facts,
        World {
            loader: None,
            inputs: &[],
            source_owner: 0,
            program,
            contracts: &contracts.data,
            hosts: &[],
            globals,
        },
        function,
        &inputs.data,
    )
}

#[test]
fn host_replacements_do_not_reuse_source_enum_values() {
    let program = bytecode::compile(
        &format!("{SOURCE} def run; Status::Draft.name; end"),
        Vec::new(),
        &(),
    )
    .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze_program(
        &mut ctx,
        &mut facts,
        &program,
        &[(Value::bytes(b"Status"), Target::NonCallable)],
    )
    .unwrap();
    assert!(!report.incomplete.data.is_empty(), "{report:?}");
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn resolved_enum_contracts_keep_members_across_script_boundaries() {
    for ty in ["Status", "Status?", "Status|nil"] {
        let source = format!(
            "{SOURCE} def echo(x:{ty}) -> {ty}; x; end; def run; echo(Status::Draft).symbol; end"
        );
        let program = bytecode::compile(&source, Vec::new(), &()).unwrap();
        let actual = Engine::legacy_unchecked()
            .compile(&source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap();
        assert_eq!(actual.value.as_bytes(), Some(b"draft".as_slice()));
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut bindings = super::type_bindings::Bindings::new();
        let scope = bindings.source(&mut ctx, &mut facts, &program, 0).unwrap();
        let mut contracts = Buffer::empty();
        for ty in &program.types {
            let fact = facts
                .annotation(&mut ctx, ty, |ctx, name| {
                    Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
                })
                .unwrap();
            contracts.push(&mut ctx, fact).unwrap();
        }
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                loader: None,
                inputs: &[],
                source_owner: 0,
                program: &program,
                contracts: &contracts.data,
                hosts: &[],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(report.incomplete.data.is_empty(), "{source}: {report:?}");
        assert!(report.issues.data.is_empty(), "{source}: {report:?}");
        let concrete = facts.symbol(&mut ctx, b"draft").unwrap();
        assert_eq!(report.returns, concrete);
        drop((report, contracts, bindings, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn enum_identities_follow_compiled_definitions_when_reusing_an_arena() {
    let first = bytecode::compile(
        "enum State; First; end; def run; State::First.name; end",
        Vec::new(),
        &(),
    )
    .unwrap();
    let second = bytecode::compile(
        "enum State; Second; end; def run; State::Second.name; end",
        Vec::new(),
        &(),
    )
    .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut enumerations = Vec::new();
    for (program, name) in [
        (&first, b"First".as_slice()),
        (&second, b"Second".as_slice()),
    ] {
        let report = analyze_program(&mut ctx, &mut facts, program, &[]).unwrap();
        assert!(report.incomplete.data.is_empty(), "{report:?}");
        assert!(report.issues.data.is_empty(), "{report:?}");
        let expected = facts.string(&mut ctx, name).unwrap();
        assert_eq!(report.returns, expected);
        enumerations.push(
            facts
                .enumeration(&mut ctx, &program.declarations[0])
                .unwrap(),
        );
    }
    assert_ne!(enumerations[0], enumerations[1]);
    assert!(
        !crate::ops::equal(&mut ctx, &first.declarations[0], &second.declarations[0], 0).unwrap()
    );
    let mut other = CallContext::new(CallOptions::default());
    let imported = other.import(&first.declarations[0]).unwrap();
    assert!(crate::ops::equal(&mut ctx, &first.declarations[0], &imported, 0).unwrap());
    assert_eq!(
        facts.enumeration(&mut ctx, &imported).unwrap(),
        enumerations[0]
    );
    let member = facts.enum_member(&mut ctx, enumerations[0], 0).unwrap();
    let mut bindings = super::type_bindings::Bindings::new();
    for owner in [7, 42] {
        let scope = bindings
            .source(&mut ctx, &mut facts, &first, owner)
            .unwrap();
        let nominal = bindings
            .resolve(&mut ctx, &[scope], "State", false)
            .unwrap()
            .fact()
            .unwrap();
        assert_eq!(
            facts.relation(&mut ctx, member, nominal).unwrap(),
            Relation::Accepted
        );
    }
    drop((bindings, imported, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    assert_eq!(other.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze_program(ctx, &mut facts, program, &[])?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    assert!(report.issues.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn enum_metadata_analysis_obeys_exact_quotas_and_releases_partial_work() {
    let members = (0..128).map(|i| format!("Member{i};")).collect::<String>();
    let program = bytecode::compile(&format!("enum State; {members} end; def echo(x); x; end; def run; values=[State::Member0,echo(State)::Member127]; values.map {{ |v| [v.name,v.symbol,v.enum.name] }}; end"), Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx, &program).unwrap();
    let stats = ctx.stats();
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
        assert_eq!(
            accounting(&mut ctx, &program).err().map(|e| e.kind),
            expected
        );
        if let Some(expected) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..32 {
        for memory in [false, true] {
            let limits = if memory {
                Limits {
                    memory_bytes: Some(stats.peak_memory_bytes * sample / 32),
                    ..Limits::default()
                }
            } else {
                Limits {
                    steps: Some(stats.steps * sample as u64 / 32),
                    ..Limits::default()
                }
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let kind = if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            };
            assert_eq!(accounting(&mut ctx, &program).unwrap_err().kind, kind);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn cached_enum_facts_observe_cancellation_and_deadlines() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let enumeration = facts
            .enumeration(&mut ctx, &program.declarations[0])
            .unwrap();
        let member = facts.enum_member(&mut ctx, enumeration, 0).unwrap();
        if deadline {
            ctx.options.deadline = Some(std::time::Instant::now());
        } else {
            ctx.cancellation().cancel();
        }
        let kind = if deadline {
            ErrorKind::Deadline
        } else {
            ErrorKind::Cancelled
        };
        assert_eq!(
            facts
                .enumeration(&mut ctx, &program.declarations[0])
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(
            facts
                .enum_member(&mut ctx, enumeration, 0)
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(
            facts.set_equal(&mut ctx, member, member).unwrap_err().kind,
            kind
        );
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
