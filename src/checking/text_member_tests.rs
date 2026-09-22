use super::{
    arguments::Arguments,
    builtins,
    collection_tests::literal_fact,
    facts::{Atom, Facts, Node},
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, ErrorKind, Limits, Value, arguments,
    bytecode::{CallSite, Method},
};

fn templates() -> Vec<&'static str> {
    vec![
        "",
        "plain {{",
        "{{x}}",
        "a {{ x }} b {{y}}",
        "{{x.y}}",
        "{{x..y}}",
        "{{x.}}",
        "{{missing}}",
        "{{x}}{{x}}",
        "{{1x}} {{ -x }}",
    ]
}

fn contexts() -> Vec<Value> {
    vec![
        Value::hash(vec![]),
        Value::hash(vec![(b"x".to_vec(), Value::int(1))]),
        Value::hash(vec![(b"x".to_vec(), Value::nil())]),
        Value::hash(vec![
            (b"x".to_vec(), Value::bytes("s")),
            (b"y".to_vec(), Value::symbol("sym")),
        ]),
        Value::hash(vec![(b"x".to_vec(), Value::array(vec![Value::int(1)]))]),
        Value::hash(vec![(
            b"x".to_vec(),
            Value::hash(vec![(b"y".to_vec(), Value::int(2))]),
        )]),
        Value::hash(vec![(
            b"x".to_vec(),
            Value::hash(vec![(b"y".to_vec(), Value::hash(vec![]))]),
        )]),
        Value::hash(vec![(b"y".to_vec(), Value::float(1.5))]),
        Value::nil(),
        Value::int(7),
        Value::array(vec![]),
    ]
}

/// Keyword forms: none, `strict: true`, `strict: false`, a non-boolean
/// strict value, an unknown option and two options.
fn keyword_forms() -> Vec<Vec<(&'static str, Value)>> {
    vec![
        vec![],
        vec![("strict", Value::boolean(true))],
        vec![("strict", Value::boolean(false))],
        vec![("strict", Value::int(1))],
        vec![("other", Value::boolean(true))],
        vec![
            ("strict", Value::boolean(true)),
            ("other", Value::boolean(true)),
        ],
    ]
}

#[test]
fn template_contracts_contain_runtime_results() {
    let mut cases = 0;
    for template in templates() {
        for context in contexts() {
            for keywords in keyword_forms() {
                for general in 0..4 {
                    let label =
                        format!("{template:?}.template({context:?}, {keywords:?}), {general}");
                    let mut ctx = CallContext::new(CallOptions::default());
                    let mut facts = Facts::new(&mut ctx).unwrap();
                    let receiver = if general & 1 != 0 {
                        Atom::String.fact()
                    } else {
                        facts.string(&mut ctx, template.as_bytes()).unwrap()
                    };
                    let mut context_fact = literal_fact(&mut ctx, &mut facts, &context);
                    if general & 2 != 0 && matches!(facts.node(context_fact), Node::Shape(..)) {
                        let values = facts.shape_values(&mut ctx, context_fact, false).unwrap();
                        context_fact = facts
                            .hash_kind(&mut ctx, Atom::String.fact(), values, true)
                            .unwrap();
                    }
                    let mut inputs = Arguments::new();
                    inputs.positional.push(&mut ctx, context_fact).unwrap();
                    let mut runtime = CallContext::new(CallOptions::default());
                    let mut actual = arguments::Arguments::empty();
                    let value = runtime.import(&context).unwrap();
                    actual.positional.push(&mut runtime, value).unwrap();
                    for (name, value) in &keywords {
                        let fact = literal_fact(&mut ctx, &mut facts, value);
                        let key = facts.symbol(&mut ctx, name.as_bytes()).unwrap();
                        inputs.keyword(&mut ctx, key, fact).unwrap();
                        let key = runtime.bytes(name.as_bytes()).unwrap();
                        let value = runtime.import(value).unwrap();
                        actual.keywords.insert(&mut runtime, key, value).unwrap();
                    }
                    let site = CallSite {
                        name: 0,
                        method: Method::parse("template"),
                        auto: false,
                        parenthesized: true,
                        scope: false,
                    };
                    let inferred =
                        builtins::member(&mut ctx, &mut facts, receiver, site, "template", &inputs)
                            .unwrap()
                            .unwrap();
                    assert!(!inferred.incomplete, "{label}");
                    let text = runtime.bytes(template.as_bytes()).unwrap();
                    match crate::members::call_keywords(
                        &mut runtime,
                        site,
                        "template",
                        text,
                        &actual,
                    ) {
                        Ok((_, value)) => {
                            // A general context may hold a non-scalar value under any
                            // key; that known-invalid alternative stays a diagnostic.
                            assert!(
                                general & 2 != 0 || inferred.failures.data.is_empty(),
                                "{label}: {value:?}"
                            );
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
                    drop((inputs, inferred, facts, actual));
                    assert_eq!(ctx.stats().retained_memory_bytes, 0, "{label}");
                    assert_eq!(runtime.stats().retained_memory_bytes, 0, "{label}");
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 2_640);
}

#[test]
fn literal_templates_keep_certain_results_and_failures() {
    let mut ctx = CallContext::new(CallOptions::default());
    let mut facts = Facts::new(&mut ctx).unwrap();
    let site = CallSite {
        name: 0,
        method: None,
        auto: false,
        parenthesized: true,
        scope: false,
    };
    let context = literal_fact(
        &mut ctx,
        &mut facts,
        &Value::hash(vec![
            (b"x".to_vec(), Value::int(1)),
            (b"list".to_vec(), Value::array(vec![])),
        ]),
    );
    // A strict miss raises as documented instead of being a contradiction, and
    // rendering stops there, so a later rejected value is unreachable.
    for (template, strict, value, failed, raises) in [
        ("plain", false, Some("plain"), false, false),
        ("{{x}}", false, None, false, false),
        ("{{list}}", false, None, true, true),
        ("{{missing}}", false, None, false, false),
        ("{{missing}}", true, None, false, true),
        ("{{x}} {{missing}} {{list}}", true, None, false, true),
        ("{{list}} {{missing}}", true, None, true, true),
    ] {
        let receiver = facts.string(&mut ctx, template.as_bytes()).unwrap();
        let mut inputs = Arguments::new();
        inputs.positional.push(&mut ctx, context).unwrap();
        if strict {
            let key = facts.symbol(&mut ctx, b"strict").unwrap();
            let yes = facts.boolean(&mut ctx, true).unwrap();
            inputs.keyword(&mut ctx, key, yes).unwrap();
        }
        let inferred = builtins::member(&mut ctx, &mut facts, receiver, site, "template", &inputs)
            .unwrap()
            .unwrap();
        assert_eq!(!inferred.failures.data.is_empty(), failed, "{template}");
        assert_eq!(inferred.throws != 0, raises, "{template}");
        match value {
            Some(text) => assert!(
                matches!(facts.node(inferred.value), Node::String(value) if value.as_bytes() == Some(text.as_bytes())),
                "{template}"
            ),
            None if raises => assert_eq!(inferred.value, Atom::Never.fact(), "{template}"),
            None => assert_eq!(inferred.value, Atom::String.fact(), "{template}"),
        }
    }
}

fn accounting(ctx: &mut CallContext) -> crate::Result<()> {
    let mut facts = Facts::new(ctx)?;
    let source = "def run(id) -> string
      text = \"{{user.name}} #{id} {{user.score}}\".template({ user: { name: \"A\", score: 4 } })
      text.concat(\"!\").include?(\"A\") ? text : \"none\"
    end";
    let result = super::collection_tests::analyze(ctx, &mut facts, source)?;
    assert!(result.incomplete.data.is_empty());
    assert!(result.issues.data.is_empty());
    Ok(())
}

#[test]
fn text_member_analysis_obeys_exact_limits_and_releases_failed_storage() {
    let mut baseline = CallContext::new(CallOptions::default());
    accounting(&mut baseline).unwrap();
    let stats = baseline.stats();
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
    for memory in (0..stats.peak_memory_bytes).step_by(89) {
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
}

#[test]
fn template_scans_observe_latched_cancellation_and_deadlines() {
    for deadline in [false, true] {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let receiver = facts.string(&mut ctx, b"{{a.b.c}}").unwrap();
        let context = literal_fact(&mut ctx, &mut facts, &Value::hash(vec![]));
        let mut inputs = Arguments::new();
        inputs.positional.push(&mut ctx, context).unwrap();
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
        let site = CallSite {
            name: 0,
            method: None,
            auto: false,
            parenthesized: true,
            scope: false,
        };
        let error = builtins::member(&mut ctx, &mut facts, receiver, site, "template", &inputs)
            .err()
            .unwrap();
        assert_eq!(error.kind, expected);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        drop((inputs, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
