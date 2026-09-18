use super::{
    arguments,
    calls::{self, Analysis, Host, World},
    collection_tests::literal_fact,
    facts::{Atom, Fact, Facts, Field, HashKind, Node},
    relation::Relation,
    type_bindings::Bindings,
};
use crate::{
    CallContext, CallOptions, Engine, ErrorKind, HostMethod, Limits, Result, Signature, Value,
    budget::Buffer, bytecode, hash, syntax, types, value::Kind,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const SOURCE: &str = "enum Status; Draft; Sent; HTTPServer; end; enum Review; Draft; end;";

pub(super) fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
) -> Result<Analysis> {
    analyze_source(ctx, facts, program, 0)
}

pub(super) fn analyze_source(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    source_owner: usize,
) -> Result<Analysis> {
    let mut bindings = Bindings::new();
    let scope = bindings.source(ctx, facts, program, source_owner)?;
    let mut contracts = Buffer::empty();
    for ty in &program.types {
        let fact = facts.annotation(ctx, ty, |ctx, name| {
            Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
        })?;
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
            inputs: &[],
            source_owner,
            program,
            contracts: &contracts.data,
            hosts: &[],
            globals: &[],
        },
        function,
        &inputs.data,
    )
}

pub(super) fn observed(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    value: &Value,
) -> Fact {
    match &value.0 {
        Kind::EnumMember(member) => {
            let original = program
                .declarations
                .iter()
                .find(|value| matches!(&value.0, Kind::Enum(parent) if Arc::ptr_eq(&parent.definition, &member.enumeration.definition)))
                .unwrap();
            let parent = facts.enumeration(ctx, original).unwrap();
            facts.enum_member(ctx, parent, member.index).unwrap()
        }
        Kind::Array(items) => {
            let items: Vec<_> = items
                .buffer
                .data
                .iter()
                .map(|value| observed(ctx, facts, program, value))
                .collect();
            facts.tuple(ctx, &items).unwrap()
        }
        Kind::Hash(hash) => {
            let mut fields = Buffer::empty();
            for (key, value) in &hash.buffer.data {
                let name = ctx.bytes(key.as_bytes().unwrap()).unwrap();
                let value = observed(ctx, facts, program, value);
                fields
                    .push(
                        ctx,
                        Field {
                            name,
                            value,
                            optional: false,
                        },
                    )
                    .unwrap();
            }
            let shape = facts
                .shape_fields(
                    ctx,
                    fields,
                    false,
                    Atom::String.fact(),
                    if hash.object {
                        HashKind::Object
                    } else {
                        HashKind::Plain
                    },
                )
                .unwrap();
            if hash.tag.protected() {
                facts.protected(ctx, shape, hash.tag).unwrap()
            } else {
                shape
            }
        }
        Kind::Enum(_) => facts.enumeration(ctx, value).unwrap(),
        _ => literal_fact(ctx, facts, value),
    }
}

pub(super) fn witness(body: &str, args: &[Value], expected: &str, rejected: bool) {
    let source = format!("{SOURCE} {body}");
    let script = Engine::new().compile(&source).unwrap();
    let program = &script.inner.code.program;
    let actual = script
        .call("run", args, CallOptions::default())
        .unwrap_or_else(|e| panic!("{source}: {e}"));
    assert_eq!(actual.value.to_string(), expected, "{source}");
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let report = analyze(&mut ctx, &mut facts, program).unwrap_or_else(|e| panic!("{source}: {e}"));
    if !report.incomplete.data.is_empty() {
        let ops: Vec<_> = report
            .incomplete
            .data
            .iter()
            .map(|&super::calls::Location { function, pc, .. }| {
                program.functions[function].code[pc]
            })
            .collect();
        panic!("{source}: {report:?}; unsupported: {ops:?}");
    }
    assert_eq!(
        !report.issues.data.is_empty(),
        rejected,
        "{source}: {report:?}"
    );
    let concrete = observed(&mut ctx, &mut facts, program, &actual.value);
    assert_ne!(
        facts.relation(&mut ctx, concrete, report.returns).unwrap(),
        Relation::Rejected,
        "{source}: {report:?}"
    );
    drop((report, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn typed_enum_symbols_normalize_across_calls_defaults_returns_and_blocks() {
    for (source, expected) in [
        (
            "def echo(x:Status); x; end; def run; echo(:draft).symbol; end",
            "draft",
        ),
        (
            "def echo(x:Status?); x; end; def run; [echo(:draft).symbol,echo(nil)]; end",
            "[draft, nil]",
        ),
        (
            "def state -> Status; :draft; end; def run; state().enum.name; end",
            "Status",
        ),
        (
            "def echo(x:Status=:draft); x; end; def run; echo().symbol; end",
            "draft",
        ),
        (
            "def run; [:draft,:sent].map { |x:Status| x.symbol }; end",
            "[draft, sent]",
        ),
        (
            "def run; [nil,:draft].map { |x:Status?| x&.symbol }; end",
            "[nil, draft]",
        ),
        (
            "def echo(x:array<Status>); x; end; def run; echo([:draft,:sent]).map { |x| x.symbol }; end",
            "[draft, sent]",
        ),
        (
            "def echo(x:hash<string,Status>); x; end; def run; echo({a: :draft}).a.symbol; end",
            "draft",
        ),
        (
            "def echo(x:{state:Status,history:array<Status?>}); x; end; def run; echo({state: :sent,history:[:draft,nil]}); end",
            "{state: Status::Sent, history: [Status::Draft, nil]}",
        ),
        (
            "def echo(x:array<array<Status>>); x; end; def run; echo([[:draft],[:sent]])[0][0].symbol; end",
            "draft",
        ),
        (
            "def change(x:array<Status>); x[0]=Status::Sent; end; def run; source=[:draft]; change(source); source.first.is_type?(:symbol); end",
            "true",
        ),
    ] {
        witness(source, &[], expected, false);
    }
}

#[test]
fn ordered_enum_unions_keep_runtime_conversion_precedence() {
    for (ty, symbol) in [
        ("Status|symbol", false),
        ("symbol|Status", true),
        ("Status|any", false),
        ("any|Status", false),
    ] {
        witness(
            &format!("def echo(x:{ty}); x; end; def run; echo(:draft).is_type?(:symbol); end"),
            &[],
            if symbol { "true" } else { "false" },
            false,
        );
    }
    for (ty, symbol) in [
        ("array<Status>|array<symbol>", false),
        ("array<symbol>|array<Status>", true),
        ("array<Status|symbol>", false),
        ("array<symbol|Status>", true),
    ] {
        witness(
            &format!(
                "def echo(x:{ty}); x; end; def run; echo([:draft]).first.is_type?(:symbol); end"
            ),
            &[],
            if symbol { "true" } else { "false" },
            false,
        );
    }
    witness(
        "def echo(x:Status|symbol); x; end; def run; echo(:unknown).is_type?(:symbol); end",
        &[],
        "true",
        false,
    );
    witness(
        "def echo(x:Status|any); x; end; def run; echo(:unknown).is_type?(:symbol); end",
        &[],
        "true",
        false,
    );
}

#[test]
fn general_enum_contracts_support_native_values_and_callbacks() {
    for (expression, expected) in [
        ("x.name", "Draft"),
        ("x.symbol", "draft"),
        ("x.enum.name", "Status"),
        ("x.dup.symbol", "draft"),
        ("x.to_s", "Status::Draft"),
        ("x.respond_to?(:symbol)", "true"),
        ("x.is_type?(:symbol)", "false"),
        ("x.tap { |v| v.name }.symbol", "draft"),
        ("x.yield_self { |v| v.enum.name }", "Status"),
        ("JSON.stringify(x)", "\"draft\""),
        ("x.send(:to_s)", "Status::Draft"),
        ("x == Status::Draft", "true"),
        ("x == Review::Draft", "false"),
        ("x.eql?(Status::Draft)", "true"),
    ] {
        witness(
            &format!("def run(x:Status); {expression}; end"),
            &[Value::symbol(b"draft")],
            expected,
            false,
        );
    }
    witness(
        "def run(xs:array<Status>); xs.map { |x| x.symbol }; end",
        &[Value::array(vec![
            Value::symbol(b"draft"),
            Value::symbol(b"sent"),
        ])],
        "[draft, sent]",
        false,
    );
    witness(
        "def run(x:Status); begin; x[0]; rescue; 99; end; end",
        &[Value::symbol(b"draft")],
        "99",
        true,
    );
}

#[test]
fn enum_hash_key_constraints_validate_without_converting_keys() {
    for (ty, accepted) in [
        ("Status", false),
        ("Status|symbol", false),
        ("symbol|Status", false),
        ("Status|string", true),
        ("string|Status", true),
        ("symbol", true),
    ] {
        witness(
            &format!(
                "def accept(x:hash<{ty},int>); x; end; def run; begin; accept({{draft:1}}).length; rescue; 99; end; end"
            ),
            &[],
            if accepted { "1" } else { "99" },
            !accepted,
        );
    }
    witness(
        "def accept(x:hash<Status,Status>); x; end; def run; accept({}).length; end",
        &[],
        "0",
        false,
    );
}

fn contract(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &bytecode::Program,
    ty: &types::Type,
) -> Fact {
    let mut bindings = Bindings::new();
    let scope = bindings.source(ctx, facts, program, 0).unwrap();
    facts
        .annotation(ctx, ty, |ctx, name| {
            Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
        })
        .unwrap()
}

fn runtime_normalize(
    ctx: &mut CallContext,
    program: &bytecode::Program,
    ty: &types::Type,
    value: Value,
) -> Result<Value> {
    types::prepare(ctx, ty, |ctx, name| {
        let value = program
            .declarations
            .iter()
            .find(|value| value.as_enum_type() == Some(name))
            .unwrap();
        ctx.import(value)
    })?
    .normalize(ctx, value)
}

#[test]
fn general_enum_cases_narrow_each_branch_and_exclude_exhausted_members() {
    for (body, expected) in [
        (
            "case x; when Status::Draft; x.name; when Status::Sent; x.symbol; else; x.name; end",
            "Draft",
        ),
        (
            "case x; when Status::Draft; 1; when Status::Sent; 2; when Status::HTTPServer; 3; else; 1+\"bad\"; end",
            "1",
        ),
        (
            "case x; when *[Status::Draft,Status::Sent]; 4; when Status::HTTPServer; 5; else; 1+\"bad\"; end",
            "4",
        ),
        (
            "case x; when Review::Draft; 1+\"bad\"; else; x.symbol; end",
            "draft",
        ),
    ] {
        witness(
            &format!("def run(x:Status); {body}; end"),
            &[Value::symbol(b"draft")],
            expected,
            false,
        );
    }
    let source = format!(
        "{SOURCE} def run(x:Status); case x; when Status::Sent; 1; when Status::HTTPServer; 2; else; x.name; end; end"
    );
    let program = bytecode::compile(&source, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let result = analyze(&mut ctx, &mut facts, &program).unwrap();
    let draft = facts.string(&mut ctx, b"Draft").unwrap();
    let one = facts.integer(&mut ctx, 1).unwrap();
    let two = facts.integer(&mut ctx, 2).unwrap();
    assert_eq!(
        result.returns,
        facts.union(&mut ctx, &[one, two, draft]).unwrap()
    );
    assert!(result.issues.data.is_empty() && result.incomplete.data.is_empty());
}

#[test]
fn nested_union_fallbacks_preserve_their_own_annotation_order() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let named = |name: &str| types::Type::named(name.into());
    let union = |options| types::Type {
        name: String::new(),
        kind: types::TypeKind::Union(options),
        nullable: false,
    };
    for (ty, converts) in [
        (
            union(vec![
                union(vec![named("int"), named("any")]),
                named("Status"),
            ]),
            false,
        ),
        (
            union(vec![named("int"), named("any"), named("Status")]),
            true,
        ),
        (
            union(vec![
                named("Status"),
                union(vec![named("any"), named("symbol")]),
            ]),
            true,
        ),
        (
            union(vec![
                union(vec![named("symbol"), named("Status")]),
                named("Status"),
            ]),
            false,
        ),
        (
            union(vec![
                union(vec![named("Status"), named("symbol")]),
                named("any"),
            ]),
            true,
        ),
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let expected = contract(&mut ctx, &mut facts, &program, &ty);
        let input = Value::symbol(b"draft");
        let input_fact = observed(&mut ctx, &mut facts, &program, &input);
        let inferred = facts.normalized(&mut ctx, input_fact, expected).unwrap();
        let output = runtime_normalize(&mut ctx, &program, &ty, input).unwrap();
        assert_eq!(output.as_enum_member().is_some(), converts, "{ty:?}");
        assert_eq!(
            inferred,
            observed(&mut ctx, &mut facts, &program, &output),
            "{ty:?}"
        );
        drop((output, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn nested_normalization_matches_runtime_successes_failures_and_exact_values() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let values = [
        Value::nil(),
        Value::int(7),
        Value::symbol(b"draft"),
        Value::symbol(b"sent"),
        Value::symbol(b"unknown"),
        Value::bytes(b"draft"),
        Value::array(vec![]),
        Value::array(vec![Value::symbol(b"draft")]),
        Value::array(vec![Value::symbol(b"draft"), Value::symbol(b"unknown")]),
        Value::hash(vec![]),
        Value::hash(vec![(b"state".to_vec(), Value::symbol(b"draft"))]),
        Value::hash(vec![(b"state".to_vec(), Value::symbol(b"unknown"))]),
        Value::hash(vec![
            (b"state".to_vec(), Value::symbol(b"draft")),
            (b"other".to_vec(), Value::int(7)),
        ]),
        Value::array(vec![Value::hash(vec![(
            b"state".to_vec(),
            Value::symbol(b"draft"),
        )])]),
    ];
    let mut comparisons = 0;
    for spelling in [
        "Status",
        "Status?",
        "Status|symbol",
        "symbol|Status",
        "Status|any",
        "any|Status",
        "array<Status>",
        "array<Status|symbol>",
        "array<symbol|Status>",
        "hash<string,Status>",
        "hash<Status,Status>",
        "{state:Status}",
        "{state?:Status}",
        "{state:Status,...}",
        "array<{state:Status}>",
    ] {
        let ty = syntax::parse_type(spelling).unwrap();
        for value in &values {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut facts = Facts::new(&mut ctx).unwrap();
            let expected = contract(&mut ctx, &mut facts, &program, &ty);
            let actual = observed(&mut ctx, &mut facts, &program, value);
            let inferred = facts.normalized(&mut ctx, actual, expected).unwrap();
            let result = runtime_normalize(&mut ctx, &program, &ty, value.clone());
            match result {
                Ok(output) => {
                    assert_eq!(
                        inferred,
                        observed(&mut ctx, &mut facts, &program, &output),
                        "{spelling}: {value}"
                    );
                }
                Err(error) => {
                    assert_eq!(error.kind, ErrorKind::Type, "{spelling}: {value}");
                    assert_eq!(inferred, Atom::Never.fact(), "{spelling}: {value}");
                }
            }
            drop(facts);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            comparisons += 1;
        }
    }
    assert_eq!(comparisons, 210);
}

#[test]
fn protected_hashes_only_lose_their_tag_when_normalization_replaces_values() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    for tag in [hash::Tag::Match, hash::Tag::Error] {
        for object in [false, true] {
            for changed in [false, true] {
                for spelling in ["{state:Status,...}", "hash<string,Status>"] {
                    let mut ctx = CallContext::new(CallOptions::default());
                    let mut facts = Facts::new(&mut ctx).unwrap();
                    let ty = syntax::parse_type(spelling).unwrap();
                    let expected = contract(&mut ctx, &mut facts, &program, &ty);
                    let mut value = ctx.import(&Value::symbol(b"draft")).unwrap();
                    if !changed {
                        value = runtime_normalize(
                            &mut ctx,
                            &program,
                            &syntax::parse_type("Status").unwrap(),
                            value,
                        )
                        .unwrap();
                    }
                    let mut entries = Buffer::empty();
                    let key = ctx.bytes(b"state").unwrap();
                    entries.push(&mut ctx, (key, value)).unwrap();
                    let mut input = hash::Hash::from_entries(&mut ctx, entries).unwrap();
                    input.tag = tag;
                    input.object = object;
                    let input = Value::from_hash(&mut ctx, input).unwrap();
                    let source = observed(&mut ctx, &mut facts, &program, &input);
                    let inferred = facts.normalized(&mut ctx, source, expected).unwrap();
                    let output = runtime_normalize(&mut ctx, &program, &ty, input.clone()).unwrap();
                    let (Kind::Hash(a), Kind::Hash(b)) = (&input.0, &output.0) else {
                        panic!()
                    };
                    assert_eq!(Arc::ptr_eq(a, b), !changed);
                    assert_eq!(b.tag, if changed { hash::Tag::None } else { tag });
                    assert_eq!(b.object, object);
                    assert_eq!(inferred, observed(&mut ctx, &mut facts, &program, &output));
                    drop((input, output, facts));
                    assert_eq!(ctx.stats().retained_memory_bytes, 0);
                }
            }
        }
    }
}

#[test]
fn host_return_contracts_produce_enum_values_without_executing_callbacks() {
    for (result, value, expression, expected) in [
        ("Status", Value::symbol(b"draft"), "host().symbol", "draft"),
        (
            "array<Status>",
            Value::array(vec![Value::symbol(b"sent")]),
            "x=host().first; if x; x.enum.name; else; \"empty\"; end",
            "Status",
        ),
        (
            "{state:Status}",
            Value::hash(vec![(b"state".to_vec(), Value::symbol(b"draft"))]),
            "host().state.name",
            "Draft",
        ),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let method = HostMethod::new("host", move |_, _, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(value.clone())
        })
        .with_signature(Signature {
            params: vec![],
            result: result.into(),
            accepts_block: false,
        })
        .unwrap();
        let mut engine = Engine::new();
        engine.register_method("host", method.clone());
        let script = engine
            .compile(&format!("{SOURCE} def run; {expression}; end"))
            .unwrap();
        let program = &script.inner.code.program;
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let mut bindings = Bindings::new();
        let scope = bindings.source(&mut ctx, &mut facts, program, 0).unwrap();
        let value = method.value();
        let Kind::Host(method) = &value.0 else {
            panic!()
        };
        let host = Host::resolved(&mut ctx, &mut facts, method.signature(), |ctx, name| {
            Ok(bindings.resolve(ctx, &[scope], name, false)?.fact())
        })
        .unwrap();
        let report = calls::analyze(
            &mut ctx,
            &mut facts,
            World {
                inputs: &[],
                source_owner: 0,
                program,
                contracts: &[],
                hosts: &[host],
                globals: &[],
            },
            program.names["run"],
            &[],
        )
        .unwrap();
        assert!(
            report.incomplete.data.is_empty() && report.issues.data.is_empty(),
            "{expression} -> {result}: {report:?}; ops: {:?}",
            program.functions[program.names["run"]].code
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let result = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(result.value.to_string(), expected);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let value = observed(&mut ctx, &mut facts, program, &result.value);
        assert_eq!(
            facts.relation(&mut ctx, value, report.returns).unwrap(),
            Relation::Accepted
        );
        drop((report, bindings, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn generic_enum_domains_keep_compiled_identity_and_single_member_precision() {
    let a = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let b = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let ty = syntax::parse_type("Status").unwrap();
    let expected = contract(&mut ctx, &mut facts, &a, &ty);
    let foreign = contract(&mut ctx, &mut facts, &b, &ty);
    let domain = facts.value_domain(&mut ctx, expected).unwrap();
    let foreign = facts.value_domain(&mut ctx, foreign).unwrap();
    assert_ne!(domain, expected);
    assert_eq!(
        facts.normalized(&mut ctx, domain, expected).unwrap(),
        domain
    );
    assert_eq!(
        facts.normalized(&mut ctx, foreign, expected).unwrap(),
        Atom::Never.fact()
    );
    assert_eq!(facts.definitely_equal(domain, foreign), Some(false));
    assert_eq!(facts.definitely_equal(domain, domain), None);
    let only = contract(
        &mut ctx,
        &mut facts,
        &a,
        &syntax::parse_type("Review").unwrap(),
    );
    let only = facts.value_domain(&mut ctx, only).unwrap();
    assert!(matches!(
        facts.node(only),
        Node::EnumMember { index: Some(0), .. }
    ));
    assert_eq!(facts.definitely_equal(only, only), Some(true));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn uncertain_conversions_keep_protected_and_replaced_hash_outcomes() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let ty = syntax::parse_type("{state:Status}").unwrap();
    let expected = contract(&mut ctx, &mut facts, &program, &ty);
    let value = runtime_normalize(
        &mut ctx,
        &program,
        &syntax::parse_type("Status").unwrap(),
        Value::symbol(b"draft"),
    )
    .unwrap();
    let member = observed(&mut ctx, &mut facts, &program, &value);
    let symbol = facts.symbol(&mut ctx, b"draft").unwrap();
    let either = facts.union(&mut ctx, &[symbol, member]).unwrap();
    let source = facts
        .shape(&mut ctx, &[(b"state", either, false)], false)
        .unwrap();
    let source = facts.protected(&mut ctx, source, hash::Tag::Match).unwrap();
    let output = facts.normalized(&mut ctx, source, expected).unwrap();
    let plain = facts
        .shape(&mut ctx, &[(b"state", member, false)], false)
        .unwrap();
    let protected = facts.protected(&mut ctx, plain, hash::Tag::Match).unwrap();
    assert_eq!(output, facts.union(&mut ctx, &[plain, protected]).unwrap());
    drop((value, facts));
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn deep_contract_projection_and_shared_normalization_use_the_default_stack() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut expected = contract(
        &mut ctx,
        &mut facts,
        &program,
        &syntax::parse_type("Status").unwrap(),
    );
    for _ in 0..10_000 {
        expected = facts.array(&mut ctx, expected).unwrap();
    }
    let mut projected = facts.value_domain(&mut ctx, expected).unwrap();
    for _ in 0..10_000 {
        let Node::Array(child) = facts.node(projected) else {
            panic!()
        };
        projected = *child;
    }
    assert!(matches!(
        facts.node(projected),
        Node::EnumMember { index: None, .. }
    ));
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);

    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let mut expected = contract(
        &mut ctx,
        &mut facts,
        &program,
        &syntax::parse_type("Status").unwrap(),
    );
    let mut actual = facts.symbol(&mut ctx, b"draft").unwrap();
    let mut converted = facts.normalized(&mut ctx, actual, expected).unwrap();
    for _ in 0..40 {
        actual = facts
            .shape(
                &mut ctx,
                &[(b"a", actual, false), (b"b", actual, false)],
                false,
            )
            .unwrap();
        expected = facts
            .shape(
                &mut ctx,
                &[(b"a", expected, false), (b"b", expected, false)],
                false,
            )
            .unwrap();
        converted = facts
            .shape(
                &mut ctx,
                &[(b"a", converted, false), (b"b", converted, false)],
                false,
            )
            .unwrap();
    }
    assert_eq!(
        facts.normalized(&mut ctx, actual, expected).unwrap(),
        converted
    );
    drop(facts);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn accounting(ctx: &mut CallContext, program: &bytecode::Program) -> Result<()> {
    let mut facts = Facts::new(ctx)?;
    let report = analyze(ctx, &mut facts, program)?;
    assert!(
        report.incomplete.data.is_empty() && report.issues.data.is_empty(),
        "{report:?}"
    );
    Ok(())
}

#[test]
fn normalization_honors_exact_quotas_and_releases_interrupted_work() {
    let program = bytecode::compile(&format!("{SOURCE} def convert(x:array<{{state:Status|symbol,extra?:Status?,...}}>); x; end; def run(input:Status); result=convert([{{state: :draft,extra: :sent}},{{state: :unknown}}]); result.map {{ |x| x.state }}; case input; when Status::Draft; 1; when Status::Sent; 2; else; input.name; end; end"), Vec::new(), &()).unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    accounting(&mut ctx, &program).unwrap();
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
        assert_eq!(accounting(&mut ctx, &program).err().map(|e| e.kind), error);
        if let Some(kind) = error {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
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
fn cached_normalization_paths_observe_cancellation_and_deadlines() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let expected = contract(
            &mut ctx,
            &mut facts,
            &program,
            &syntax::parse_type("Status").unwrap(),
        );
        let domain = facts.value_domain(&mut ctx, expected).unwrap();
        let actual = facts.symbol(&mut ctx, b"draft").unwrap();
        facts.normalized(&mut ctx, actual, expected).unwrap();
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
        for (actual, expected) in [
            (domain, domain),
            (actual, expected),
            (actual, Atom::Any.fact()),
            (Atom::Never.fact(), expected),
        ] {
            assert_eq!(
                facts
                    .normalized(&mut ctx, actual, expected)
                    .unwrap_err()
                    .kind,
                kind
            );
        }
        for expected in [expected, domain, Atom::Never.fact(), Atom::Any.fact()] {
            assert_eq!(
                facts.value_domain(&mut ctx, expected).unwrap_err().kind,
                kind
            );
        }
        assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        drop(facts);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn nullable_enum_properties_keep_ordinary_lookup_errors_catchable() {
    for name in ["name", "symbol", "enum"] {
        witness(
            &format!("def run(x:Status?); begin; x.{name}; rescue; 99; end; end"),
            &[Value::nil()],
            "99",
            true,
        );
    }
}

#[test]
fn general_hash_contracts_describe_stored_string_keys() {
    let program = bytecode::compile(SOURCE, Vec::new(), &()).unwrap();
    for ty in [
        "hash<symbol,int>",
        "hash<symbol|int,int>",
        "hash<any,int>",
        "{state:int}",
    ] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let ty = syntax::parse_type(ty).unwrap();
        let expected = contract(&mut ctx, &mut facts, &program, &ty);
        let projected = facts.value_domain(&mut ctx, expected).unwrap();
        let keys = match facts.node(projected) {
            Node::Hash(keys, _, _) | Node::Shape(_, _, keys, _) => *keys,
            _ => panic!(),
        };
        let input = Value::hash(vec![(b"state".to_vec(), Value::int(7))]);
        let output = runtime_normalize(&mut ctx, &program, &ty, input.clone()).unwrap();
        let Kind::Hash(hash) = &output.0 else {
            panic!()
        };
        for (key, _) in &hash.buffer.data {
            let key = observed(&mut ctx, &mut facts, &program, key);
            assert_eq!(
                facts.relation(&mut ctx, key, keys).unwrap(),
                Relation::Accepted,
                "{ty:?}"
            );
        }
        assert_eq!(keys, Atom::String.fact());
        let actual = observed(&mut ctx, &mut facts, &program, &input);
        assert_eq!(
            facts.normalized(&mut ctx, actual, expected).unwrap(),
            actual
        );
        drop((output, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
