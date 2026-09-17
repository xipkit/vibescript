use super::{
    collection_tests::analyze, facts::Facts, iteration_tests::inferred_runtime,
    native_tests::witness,
};
use crate::{CallContext, CallOptions, ErrorKind, Limits, Value};

fn forms(receiver: &str, name: &str, args: &str) -> [String; 3] {
    let suffix = if args.is_empty() {
        String::new()
    } else {
        format!(",{args}")
    };
    [
        format!("({receiver}).{name}({args})"),
        format!("({receiver}).send(:{name}{suffix})"),
        format!("({receiver}).public_send(:send,:{name}{suffix})"),
    ]
}

#[test]
fn native_responding_checks_lookup_without_entering_the_method() {
    for (receiver, name, expected) in [
        ("7", "odd?", true),
        ("7", "seconds", true),
        ("10**30", "seconds", false),
        ("7.5", "nan?", true),
        ("7", "nan?", false),
        ("nil", "to_s", true),
        (":name", "id2name", true),
        (":name", "to_sym", true),
        ("[1]", "map", true),
        ("[1]", "missing", false),
        ("1..3", "step", true),
        ("Time.at(0)", "year", true),
        ("1.seconds", "parts", true),
        ("/a/", "match?", true),
        ("money(\"1 USD\")", "cents", true),
        ("{size:JSON::parse}", "size", true),
        ("{tap:7}", "tap", false),
        ("{run:JSON::parse}", "run", true),
        ("{data:7}", "data", false),
        ("JSON", "parse", true),
        ("JSON", "keys", true),
        ("JSON::parse", "call", false),
        ("JSON::parse", "equal?", true),
        ("Regexp.new(\"(a)\").match(\"a\")", "begin", true),
        ("Regexp.new(\"(a)\").match(\"a\")", "captures", false),
    ] {
        for call in forms(receiver, "respond_to?", &format!(":{name}")) {
            witness(
                &format!("def run -> bool; {call}; end"),
                Some(if expected { "true" } else { "false" }),
                false,
            );
        }
    }
    witness(
        "def run -> int; if 7.respond_to?(:missing); missing; else; 7; end; end",
        Some("7"),
        false,
    );
}

#[test]
fn primitive_type_atoms_use_native_kinds_and_nullable_rules() {
    for (receiver, atom, expected) in [
        ("nil", "nil", true),
        ("nil", "int?", true),
        ("nil", "int", false),
        ("false", "bool", true),
        ("7", "int", true),
        ("10**30", "int", true),
        ("7.5", "float", true),
        ("7", "number", true),
        ("7.5", "number", true),
        ("\"x\"", "string", true),
        (":x", "symbol", true),
        ("[]", "array", true),
        ("{}", "hash", true),
        ("{}", "object", true),
        ("JSON", "hash", true),
        ("1..3", "range", true),
        ("1.seconds", "duration", true),
        ("Time.at(0)", "time", true),
        ("money(\"1 USD\")", "money", true),
        ("/a/", "string", false),
        ("JSON::parse", "object", false),
    ] {
        for call in forms(receiver, "is_type?", &format!("\"{atom}\"")) {
            witness(
                &format!("def run -> bool; {call}; end"),
                Some(if expected { "true" } else { "false" }),
                false,
            );
        }
    }
    witness(
        "def run -> bool; 7.is_type?(\"int\".to_sym); end",
        Some("true"),
        false,
    );
    witness(
        "def run -> bool; 7.respond_to?(:odd?.id2name); end",
        Some("true"),
        false,
    );
}

#[test]
fn introspection_validates_arguments_and_leaves_blocks_unexecuted() {
    for method in [
        "respond_to?",
        "is_type?",
        "is_a?",
        "kind_of?",
        "instance_of?",
    ] {
        for args in ["", "7,8,9", "extra:7"] {
            for call in forms("7", method, args) {
                witness(
                    &format!(
                        "def run; seen=[]; begin; {call} {{seen.push(9); return 999}}; rescue RuntimeError; [7,seen]; end; end"
                    ),
                    Some("[7, []]"),
                    true,
                );
            }
        }
    }
    for (method, args) in [
        ("respond_to?", "7"),
        ("respond_to?", ":odd?,7"),
        ("is_type?", "7"),
        ("is_type?", "\"regex\""),
        ("is_type?", "\"int[]\""),
        ("is_type?", "\"lowercase\""),
        ("is_type?", "\"A.B.C\""),
        ("is_a?", "JSON"),
        ("kind_of?", ":int"),
        ("instance_of?", "{}"),
    ] {
        for call in forms("7", method, args) {
            witness(
                &format!("def run; begin; {call}; rescue RuntimeError; 7; end; end"),
                Some("7"),
                true,
            );
        }
    }
}

#[test]
fn native_queries_accept_general_arguments_and_keep_union_results() {
    for source in [
        "def run(name:string) -> bool; 7.respond_to?(name); end",
        "def run(name:string) -> bool; 7.send(:respond_to?,name,true); end",
    ] {
        for name in ["odd?", "seconds", "missing"] {
            inferred_runtime(source, &[Value::bytes(name)], false);
        }
    }
    for flag in [false, true] {
        inferred_runtime(
            "def run(flag:bool) -> bool; name=if flag; :int; else; :string; end; 7.is_type?(name); end",
            &[Value::boolean(flag)],
            false,
        );
        inferred_runtime(
            "def run(flag:bool) -> bool; x=if flag; 7; else; \"x\"; end; x.respond_to?(:odd?); end",
            &[Value::boolean(flag)],
            false,
        );
    }
    inferred_runtime(
        "def run(value:int) -> bool; value.respond_to?(:seconds); end",
        &[Value::int(7)],
        false,
    );
    for source in [
        "def run(kind); begin; 7.is_a?(kind); rescue RuntimeError; false; end; end",
        "def run(kind); begin; [].instance_of?(kind); rescue RuntimeError; false; end; end",
    ] {
        inferred_runtime(source, &[Value::int(7)], false);
    }
}

#[test]
fn named_reducers_and_symbol_roundtrips_use_the_same_native_contracts() {
    for (source, expected) in [
        (
            "def run -> bool; [7,:odd?].reduce(:respond_to?); end",
            "true",
        ),
        (
            "def run -> bool; [nil,\"int?\"].reduce(:is_type?); end",
            "true",
        ),
        (
            "def run -> bool; :name.id2name.to_sym == :name; end",
            "true",
        ),
        ("def run -> string; :name.id2name; end", "name"),
        ("def run -> symbol; :name.to_sym; end", "name"),
    ] {
        witness(source, Some(expected), false);
    }
}

#[test]
fn unresolved_nominal_queries_stay_explicitly_incomplete() {
    for source in [
        "def run; 7.is_type?(:User); end",
        "def run; nil.is_type?(\"User?\"); end",
        "def run(name:string); 7.is_type?(name); end",
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
fn optional_and_open_hash_queries_preserve_present_and_absent_results() {
    use super::{arguments::Arguments, builtins, facts::Atom, relation::Relation};
    use crate::{builtin::Builtin, bytecode::CallSite};

    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let callable = facts.builtin(&mut ctx, Builtin::JsonParse).unwrap();
    for name in [b"run".as_slice(), b"tap", b"size", b"raw\xff\0"] {
        for field in [callable, Atom::Int.fact(), Atom::Unknown.fact()] {
            for optional in [false, true] {
                for open in [false, true] {
                    let receiver = facts
                        .shape(&mut ctx, &[(name, field, optional)], open)
                        .unwrap();
                    for query in [name, b"absent"] {
                        let mut args = Arguments::new();
                        let query_fact = facts.string(&mut ctx, query).unwrap();
                        args.positional.push(&mut ctx, query_fact).unwrap();
                        let inferred = builtins::member(
                            &mut ctx,
                            &mut facts,
                            receiver,
                            CallSite {
                                name: 0,
                                method: None,
                                auto: false,
                                parenthesized: true,
                                scope: false,
                            },
                            "respond_to?",
                            &args,
                        )
                        .unwrap()
                        .unwrap();
                        assert!(!inferred.incomplete && inferred.throws == 0);
                        assert!(inferred.failures.data.is_empty());
                        let label = format!(
                            "{name:?}/{query:?} field={field:?} optional={optional} open={open}"
                        );
                        // These alternatives include both a present callable/data
                        // field and an absent optional field, independently of
                        // the checker lookup implementation.
                        let possible = if query != name {
                            if open { [true, true] } else { [true, false] }
                        } else if name == b"size" {
                            [false, true]
                        } else if field == Atom::Unknown.fact() {
                            [true, true]
                        } else if field == callable {
                            [optional && name != b"tap", true]
                        } else {
                            [true, optional && name == b"tap"]
                        };
                        for (value, admitted) in possible.into_iter().enumerate() {
                            let value = facts.boolean(&mut ctx, value != 0).unwrap();
                            assert_eq!(
                                facts.relation(&mut ctx, value, inferred.value).unwrap()
                                    != Relation::Rejected,
                                admitted,
                                "{label}"
                            );
                        }
                    }
                }
            }
        }
    }
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn native_introspection_contracts_contain_runtime_results_and_errors() {
    use super::{
        arguments::Arguments, builtins, collection_tests::literal_fact, facts::Atom,
        relation::Relation,
    };
    use crate::{
        builtin::Builtin,
        bytecode::CallSite,
        members::introspection::{Predicate, Query},
        value::Kind,
    };

    let mut owner = CallContext::new(CallOptions::default());
    let regex = crate::regex::value::Regex::compile(&mut owner, Value::bytes("a"), 0).unwrap();
    let big = crate::Engine::new()
        .compile("10**30")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let values = [
        Value::nil(),
        Value::boolean(false),
        Value::int(7),
        big,
        Value::float(1.5),
        Value::bytes(b"raw\xff".as_slice()),
        Value::symbol(b"raw\xff".as_slice()),
        Value::range(Some(1), Some(3), true),
        Value::array(vec![Value::int(7)]),
        Value::hash(vec![(
            b"run".to_vec(),
            Value(Kind::Builtin(Builtin::JsonParse)),
        )]),
        Value::time(0, 0).unwrap(),
        Value::duration(1),
        Value::money(125, "USD").unwrap(),
        regex,
        Value(Kind::Builtin(Builtin::JsonParse)),
    ];
    let mut cases = 0;
    for receiver in &values {
        for name in [
            "respond_to?",
            "is_type?",
            "is_a?",
            "kind_of?",
            "instance_of?",
        ] {
            let predicate = Predicate::parse(name).unwrap();
            for args in [
                vec![],
                vec![Value::int(7)],
                vec![Value::nil()],
                vec![Value::symbol("odd?")],
                vec![Value::bytes("seconds")],
                vec![Value::symbol("id2name")],
                vec![Value::bytes("to_sym")],
                vec![Value::bytes("tap")],
                vec![Value::symbol("run")],
                vec![Value::bytes("size")],
                vec![Value::bytes(b"raw\xff\0".as_slice())],
                vec![Value::bytes(vec![b'x'; 257])],
                vec![Value::bytes("int")],
                vec![Value::symbol("int?")],
                vec![Value::bytes("number")],
                vec![Value::bytes("array")],
                vec![Value::symbol("object")],
                vec![Value::bytes("regex")],
                vec![Value::bytes("int[]")],
                vec![Value::bytes("A.B.C")],
                vec![Value::symbol("odd?"), Value::boolean(true)],
                vec![Value::symbol("odd?"), Value::int(7)],
            ] {
                for general in 0..4 {
                    for keywords in [false, true] {
                        let label = format!(
                            "{receiver:?}.{name}({args:?}), general={general}, keywords={keywords}"
                        );
                        let mut ctx = CallContext::new(CallOptions::default());
                        let mut facts = Facts::new(&mut ctx).unwrap();
                        let mut root = literal_fact(&mut ctx, &mut facts, receiver);
                        if general & 1 != 0 {
                            root = facts.atom(root).map_or(root, Atom::fact);
                        }
                        let mut inputs = Arguments::new();
                        for arg in &args {
                            let mut value = literal_fact(&mut ctx, &mut facts, arg);
                            if general & 2 != 0 {
                                value = facts.atom(value).map_or(value, Atom::fact);
                            }
                            inputs.positional.push(&mut ctx, value).unwrap();
                        }
                        if keywords {
                            let key = facts.symbol(&mut ctx, b"extra").unwrap();
                            inputs.keyword(&mut ctx, key, Atom::Int.fact()).unwrap();
                        }
                        let inferred = builtins::member(
                            &mut ctx,
                            &mut facts,
                            root,
                            CallSite {
                                name: 0,
                                method: None,
                                auto: false,
                                parenthesized: true,
                                scope: false,
                            },
                            name,
                            &inputs,
                        )
                        .unwrap()
                        .unwrap();
                        let mut runtime = CallContext::new(CallOptions::default());
                        let actual = predicate
                            .validate(&mut runtime, &args, keywords, false)
                            .and_then(|query| match query {
                                Query::Respond(query, _) => {
                                    crate::members::introspection::responds(
                                        &mut runtime,
                                        receiver,
                                        query,
                                    )
                                }
                                Query::Type(atom) => atom.matches(&mut runtime, receiver, None),
                                Query::Class(class) => {
                                    Ok(crate::members::introspection::belongs(receiver, class))
                                }
                            });
                        if !inferred.incomplete {
                            match actual {
                                Ok(value) => {
                                    let concrete = facts.boolean(&mut ctx, value).unwrap();
                                    assert_ne!(inferred.value, Atom::Never.fact(), "{label}");
                                    assert_ne!(
                                        facts.relation(&mut ctx, concrete, inferred.value).unwrap(),
                                        Relation::Rejected,
                                        "{label}"
                                    );
                                }
                                Err(error) => assert_ne!(
                                    inferred.throws & (1 << error.class().unwrap() as u8),
                                    0,
                                    "{label}: {error}"
                                ),
                            }
                        } else {
                            assert!(name == "is_type?" && general & 2 != 0, "{label}");
                        }
                        drop((inputs, inferred, facts));
                        assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
                        assert_eq!(runtime.stats().retained_memory_bytes, 0, "{label}");
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, 13_200);
    drop(values);
    assert_eq!(owner.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(
        ctx,
        &mut facts,
        "def run(flag:bool); name=if flag; :tap; else; :run; end; h={run:JSON::parse,tap:7}; [h.send(:respond_to?,name),7.is_type?(:int?.id2name),[7,:odd?].reduce(:respond_to?),:raw.id2name.to_sym,begin; 7.is_type?(\"int[]\"); rescue RuntimeError; false; end]; end",
    )?;
    assert!(report.incomplete.data.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn introspection_analysis_charges_exact_budgets_and_reclaims_failed_allocations() {
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx).unwrap();
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
        assert_eq!(accounting(&mut ctx).err().map(|error| error.kind), expected);
        if let Some(expected) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..64 {
        for memory in [false, true] {
            let mut ctx = CallContext::new(CallOptions {
                limits: if memory {
                    Limits {
                        memory_bytes: Some(stats.peak_memory_bytes * sample / 64),
                        ..Limits::default()
                    }
                } else {
                    Limits {
                        steps: Some(stats.steps * sample as u64 / 64),
                        ..Limits::default()
                    }
                },
                ..CallOptions::default()
            });
            let expected = if memory {
                ErrorKind::Memory
            } else {
                ErrorKind::Steps
            };
            assert_eq!(accounting(&mut ctx).unwrap_err().kind, expected);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn introspection_analysis_preserves_latched_cancellation_and_deadlines() {
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
