use super::{
    calls::Analysis,
    entry::{self, Call, Check},
    facts::{Atom, Node},
    flow::IssueKind,
    normalization_tests::observed,
    relation::Relation,
};
use crate::{
    CallContext, CallOptions, Capability, Engine, ErrorKind, HostMethod, Limits, Result, Script,
    Signature, SignatureParam, Value,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn value(source: &str) -> Value {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value
}

fn signature(parameter: Option<&str>, result: &str, block: bool) -> Signature {
    Signature {
        params: parameter
            .map(|ty| SignatureParam {
                name: "value".into(),
                ty: ty.into(),
                optional: false,
            })
            .into_iter()
            .collect(),
        result: result.into(),
        accepts_block: block,
    }
}

fn check(
    ctx: &mut CallContext,
    script: &Script,
    arguments: &[Value],
    keywords: &[(String, Value)],
    options: &CallOptions,
) -> Result<Check> {
    entry::check(
        ctx,
        Call {
            script,
            name: "run",
            arguments,
            keywords,
            options,
        },
    )
}

fn success(
    script: &Script,
    arguments: &[Value],
    keywords: &[(String, Value)],
    options: &CallOptions,
    expected: &Value,
) {
    let mut ctx = CallContext::new(options.clone());
    let mut checked = check(&mut ctx, script, arguments, keywords, options).unwrap();
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert!(checked.analysis.issues.data.is_empty(), "{checked:?}");
    let actual = script
        .call_with_keywords("run", arguments, keywords, options.clone())
        .unwrap()
        .value;
    assert_eq!(actual.to_string(), expected.to_string());
    let actual = observed(
        &mut ctx,
        &mut checked.facts,
        &script.inner.code.program,
        &actual,
    );
    assert_ne!(
        checked
            .facts
            .relation(&mut ctx, actual, checked.analysis.returns)
            .unwrap(),
        Relation::Rejected,
        "{checked:?}"
    );
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn rejected(
    script: &Script,
    arguments: &[Value],
    keywords: &[(String, Value)],
    options: &CallOptions,
    kind: ErrorKind,
) {
    let mut ctx = CallContext::new(options.clone());
    let checked = check(&mut ctx, script, arguments, keywords, options).unwrap();
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert!(!checked.analysis.issues.data.is_empty(), "{checked:?}");
    assert_eq!(
        script
            .call_with_keywords("run", arguments, keywords, options.clone())
            .unwrap_err()
            .kind,
        kind
    );
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn deep() -> Value {
    let mut result = Value::int(1);
    for _ in 0..=crate::budget::MAX_VALUE_DEPTH {
        result = Value::array(vec![result]);
    }
    result
}

#[test]
fn concrete_positionals_defaults_and_rest_bind_like_execution() {
    let script = Engine::new()
        .compile("def run(a:int,b=2,*rest);[a,b,rest];end")
        .unwrap();
    for (arguments, expected) in [
        (vec![Value::int(1)], "[1,2,[]]"),
        (
            vec![Value::int(1), Value::int(3), Value::int(4), Value::int(5)],
            "[1,3,[4,5]]",
        ),
    ] {
        success(
            &script,
            &arguments,
            &[],
            &CallOptions::default(),
            &value(expected),
        );
    }
    rejected(
        &script,
        &[Value::bytes(b"bad".to_vec())],
        &[],
        &CallOptions::default(),
        ErrorKind::Type,
    );
}

#[test]
fn host_keywords_bind_by_name_with_duplicates_and_keyword_rest() {
    let script = Engine::new()
        .compile("def run(a,b:,c:3,**rest);[a,b,c,rest[:x],rest[:z],rest.keys];end")
        .unwrap();
    let keywords = [
        ("b".into(), Value::int(2)),
        ("z".into(), Value::int(7)),
        ("c".into(), Value::int(4)),
        ("b".into(), Value::int(5)),
        ("x".into(), Value::int(6)),
    ];
    success(
        &script,
        &[Value::int(1)],
        &keywords,
        &CallOptions::default(),
        &value("[1,5,4,6,7,[\"x\",\"z\"]]"),
    );
    let script = Engine::new()
        .compile("def run(a,**rest);[a,rest[:a]];end")
        .unwrap();
    success(
        &script,
        &[Value::int(1)],
        &[("a".into(), Value::int(2))],
        &CallOptions::default(),
        &value("[1,2]"),
    );
    let script = Engine::new().compile("def run(a=7);a;end").unwrap();
    success(
        &script,
        &[],
        &[("a".into(), Value::int(3))],
        &CallOptions::default(),
        &Value::int(3),
    );
}

#[test]
fn host_keywords_never_become_a_positional_options_hash() {
    let script = Engine::new()
        .compile("def run(options);options;end")
        .unwrap();
    rejected(
        &script,
        &[],
        &[("different".into(), Value::int(1))],
        &CallOptions::default(),
        ErrorKind::Argument,
    );
    let script = Engine::new()
        .compile("def accept(options);options[:different];end;def run;accept(different:7);end")
        .unwrap();
    success(&script, &[], &[], &CallOptions::default(), &Value::int(7));
}

#[test]
fn shape_failures_precede_defaults_parameter_types_and_body_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    for (source, arguments, keywords) in [
        ("def run(x=tick(),needed:);tick();end", vec![], vec![]),
        (
            "def run(x=tick());tick();end",
            vec![Value::int(1), Value::int(2)],
            vec![],
        ),
        (
            "def run(x=tick());tick();end",
            vec![],
            vec![("extra".into(), Value::int(1))],
        ),
        (
            "def run(x:int,needed:);tick();end",
            vec![Value::bytes(b"bad".to_vec())],
            vec![],
        ),
        (
            "def run(x);tick();end",
            vec![Value::int(1)],
            vec![("x".into(), Value::int(2))],
        ),
    ] {
        let script = engine.compile(source).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let checked = check(
            &mut ctx,
            &script,
            &arguments,
            &keywords,
            &CallOptions::default(),
        )
        .unwrap();
        assert_eq!(checked.analysis.contexts, 0);
        assert_eq!(checked.analysis.returns, Atom::Never.fact());
        assert!(!checked.analysis.issues.data.is_empty());
        assert!(checked.analysis.incomplete.data.is_empty());
        assert_eq!(
            script
                .call_with_keywords("run", &arguments, &keywords, CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(checked);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn defaults_and_reachable_functions_follow_the_supplied_call() {
    let script = Engine::new()
        .compile(
            "def bad->int;\"bad\";end;def run(flag=false,x=7)->int;if flag;bad();else;x;end;end",
        )
        .unwrap();
    success(&script, &[], &[], &CallOptions::default(), &Value::int(7));
    success(
        &script,
        &[Value::boolean(false), Value::int(9)],
        &[],
        &CallOptions::default(),
        &Value::int(9),
    );
    rejected(
        &script,
        &[Value::boolean(true)],
        &[],
        &CallOptions::default(),
        ErrorKind::Type,
    );
    let script = Engine::new().compile("def run(x=missing);x;end").unwrap();
    success(
        &script,
        &[Value::int(7)],
        &[],
        &CallOptions::default(),
        &Value::int(7),
    );
    rejected(&script, &[], &[], &CallOptions::default(), ErrorKind::Name);
}

#[test]
fn scalar_collection_and_protected_arguments_keep_their_facts() {
    for (source, input, expected) in [
        (
            "def run(x);x;end",
            Value::bytes(vec![0, b'a', 0xff]),
            Value::bytes(vec![0, b'a', 0xff]),
        ),
        (
            "def run(x);[x[:missing],x[:a]];end",
            value("{a:7}"),
            value("[nil,7]"),
        ),
        (
            "def run(x);sum=0;for n in x;sum+=n;end;sum;end",
            value("1..3"),
            Value::int(6),
        ),
        (
            "def run(x);x.match?(\"abc\");end",
            Value::regex(b"a", "").unwrap(),
            Value::boolean(true),
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        success(&script, &[input], &[], &CallOptions::default(), &expected);
    }
    let input = value("\"abc\".match(\"(b)\")");
    let script = Engine::new()
        .compile("def run(m);begin;m.captures.push(\"bad\");rescue;nil;end;m[0];end")
        .unwrap();
    // Protected mutation is a known diagnostic even when the script rescues it.
    let mut ctx = CallContext::new(CallOptions::default());
    let checked = check(
        &mut ctx,
        &script,
        std::slice::from_ref(&input),
        &[],
        &CallOptions::default(),
    )
    .unwrap();
    assert!(!checked.analysis.issues.data.is_empty());
    assert!(checked.analysis.incomplete.data.is_empty());
    assert_eq!(
        script
            .call("run", &[input], CallOptions::default())
            .unwrap()
            .value
            .as_bytes(),
        Some(b"b".as_slice())
    );
}

#[test]
fn arguments_keywords_and_globals_keep_independent_value_copies() {
    let original = Value::array(vec![Value::int(1)]);
    let script = Engine::new()
        .compile("def run(a,b:);a.push(2);b.push(3);shared.push(4);[a,b,shared];end")
        .unwrap();
    let options = CallOptions {
        globals: [("shared".into(), original.clone())].into(),
        ..CallOptions::default()
    };
    for _ in 0..2 {
        success(
            &script,
            std::slice::from_ref(&original),
            &[("b".into(), original.clone())],
            &options,
            &value("[[1,2],[1,3],[1,4]]"),
        );
        assert_eq!(original.as_array().unwrap().len(), 1);
        assert_eq!(options.globals["shared"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn enum_inputs_rebind_to_their_compiled_source_and_normalize_symbols() {
    let script = Engine::new()
        .compile(
            "enum State;Ready;end;def produce;State::Ready;end;def run(x:State)->string;x.name;end",
        )
        .unwrap();
    let member = script
        .call("produce", &[], CallOptions::default())
        .unwrap()
        .value;
    success(
        &script,
        &[member],
        &[],
        &CallOptions::default(),
        &Value::bytes(b"Ready".to_vec()),
    );
    let symbol = value(":ready");
    success(
        &script,
        std::slice::from_ref(&symbol),
        &[],
        &CallOptions::default(),
        &Value::bytes(b"Ready".to_vec()),
    );
    assert_eq!(symbol.to_string(), value(":ready").to_string());
    let foreign = value("enum State;Ready;end;State::Ready");
    rejected(
        &script,
        &[foreign],
        &[],
        &CallOptions::default(),
        ErrorKind::Type,
    );
}

#[test]
fn capability_arguments_share_metadata_with_lazily_loaded_globals() {
    for strict in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let method = HostMethod::new("send", move |_, args, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(args[0].clone())
        })
        .with_signature(signature(Some("int"), "int", false))
        .unwrap();
        let capability = Value::object(vec![(b"send".to_vec(), method.value())]);
        let mut engine = Engine::new();
        engine.set_strict_effects(strict);
        engine.register("unused", |_, _| panic!("unused registration called"));
        let source = if strict {
            "def relay(sms);sms.send(7);end;def run(sms);relay(sms);end"
        } else {
            "def relay(sms);sms.send(7);end;def run(sms);[relay(sms),other.send(8)];end"
        };
        let script = engine.compile(source).unwrap();
        let options = if strict {
            CallOptions::default()
        } else {
            CallOptions {
                globals: [("other".into(), capability.clone())].into(),
                ..CallOptions::default()
            }
        };
        let mut ctx = CallContext::new(options.clone());
        let checked = check(
            &mut ctx,
            &script,
            std::slice::from_ref(&capability),
            &[],
            &options,
        )
        .unwrap();
        assert!(checked.analysis.issues.data.is_empty(), "{checked:?}");
        assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let actual = script.call("run", &[capability], options).unwrap().value;
        assert_eq!(
            actual.to_string(),
            if strict {
                "7".into()
            } else {
                value("[7,8]").to_string()
            }
        );
        assert_eq!(calls.load(Ordering::Relaxed), if strict { 1 } else { 2 });
        drop(checked);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn block_drivers_supplied_as_arguments_preserve_control_transfers() {
    let method = HostMethod::new_with_block("visit", |call, _, _| {
        for _ in 0..3 {
            let _ = call.call_block(&[]);
        }
        Ok(Value::int(99))
    })
    .with_signature(signature(None, "int", true))
    .unwrap();
    let capability = Value::object(vec![(b"visit".to_vec(), method.value())]);
    for (transfer, expected) in [("break 7", "[7,1]"), ("return 9", "9")] {
        let script=Engine::new().compile(&format!("def run(driver);count=0;result=driver.visit{{count+=1;{transfer}}};[result,count];end")).unwrap();
        success(
            &script,
            std::slice::from_ref(&capability),
            &[],
            &CallOptions::default(),
            &value(expected),
        );
    }
}

#[test]
fn old_capability_arguments_keep_their_expired_grants() {
    let method = HostMethod::new("send", |_, _, _| panic!("old grant revived"));
    let fresh = Value::object(vec![(b"send".to_vec(), method.value())]);
    let producer = Engine::new().compile("def run(x);x;end").unwrap();
    let old = producer
        .call("run", &[fresh], CallOptions::default())
        .unwrap()
        .value;
    let script = Engine::new()
        .compile("def run(driver);begin;driver.send();rescue RuntimeError;9;end;end")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let checked = check(
        &mut ctx,
        &script,
        std::slice::from_ref(&old),
        &[],
        &CallOptions::default(),
    )
    .unwrap();
    assert!(!checked.analysis.issues.data.is_empty());
    assert!(checked.analysis.incomplete.data.is_empty());
    assert_eq!(
        script
            .call("run", &[old], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(9)
    );
}

#[test]
fn detached_arguments_fail_before_later_inputs_and_initializers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile("class C;tick();end;def run(x,y=tick());tick();end")
        .unwrap();
    let method = HostMethod::new("send", |_, _, _| panic!("detached callback ran"));
    for input in [method.value(), Value::array(vec![method.value()])] {
        let args = [input, deep()];
        let mut ctx = CallContext::new(CallOptions::default());
        let checked = check(&mut ctx, &script, &args, &[], &CallOptions::default()).unwrap();
        assert_eq!(checked.analysis.contexts, 0);
        assert!(checked.analysis.incomplete.data.is_empty());
        assert!(matches!(
            checked.analysis.issues.data[0].issue.kind,
            IssueKind::DetachedValue(_)
        ));
        let error = script
            .call("run", &args, CallOptions::default())
            .unwrap_err();
        assert!(
            error.message.contains("cannot be used as a value"),
            "{error}"
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        drop(checked);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}

#[test]
fn required_function_arguments_stay_attached_to_their_module() {
    let code = crate::code::Code::compile_file("def helper;7;end", &Default::default()).unwrap();
    let mut producer = CallContext::new(CallOptions::default());
    let environment = crate::objects::environment(&mut producer).unwrap();
    let index = code.program.names["helper"];
    let function = Value(crate::value::Kind::Function(
        crate::exports::Function::new(&mut producer, code, environment, index).unwrap(),
    ));
    let script = Engine::new().compile("def run(x);7;end").unwrap();
    for input in [function.clone(), Value::array(vec![function.clone()])] {
        let mut ctx = CallContext::new(CallOptions::default());
        let checked = check(
            &mut ctx,
            &script,
            std::slice::from_ref(&input),
            &[],
            &CallOptions::default(),
        )
        .unwrap();
        assert!(matches!(
            checked.analysis.issues.data[0].issue.kind,
            IssueKind::DetachedValue(_)
        ));
        assert!(checked.analysis.incomplete.data.is_empty());
        assert_eq!(checked.analysis.contexts, 0);
        let error = script
            .call("run", &[input], CallOptions::default())
            .unwrap_err();
        assert!(error.message.contains("cannot be used as a value"));
        drop(checked);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    let module = Value::object(vec![(b"helper".to_vec(), function)]);
    let script = Engine::new()
        .compile("def run(lib);lib.helper();end")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let checked = check(
        &mut ctx,
        &script,
        std::slice::from_ref(&module),
        &[],
        &CallOptions::default(),
    )
    .unwrap();
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert!(matches!(
        checked.facts.node(checked.analysis.returns),
        Node::Integer(7)
    ));
    assert!(checked.analysis.issues.data.is_empty());
    assert_eq!(
        script
            .call("run", &[module], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn duplicate_keyword_values_are_imported_before_replacement() {
    let script = Engine::new().compile("def run(item:);item;end").unwrap();
    let method = HostMethod::new("send", |_, _, _| panic!("keyword callback called"));
    let keywords = [
        ("item".into(), method.value()),
        ("item".into(), Value::int(7)),
    ];
    let mut ctx = CallContext::new(CallOptions::default());
    let checked = check(&mut ctx, &script, &[], &keywords, &CallOptions::default()).unwrap();
    assert!(!checked.analysis.issues.data.is_empty());
    assert_eq!(checked.analysis.contexts, 0);
    assert!(
        script
            .call_with_keywords("run", &[], &keywords, CallOptions::default())
            .unwrap_err()
            .message
            .contains("cannot be used as a value")
    );
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let keywords = [("item".into(), deep()), ("item".into(), Value::int(7))];
    assert_eq!(
        check(&mut ctx, &script, &[], &keywords, &CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert_eq!(
        script
            .call_with_keywords("run", &[], &keywords, CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert!(ctx.checkpoint().is_ok());
}

#[test]
fn all_host_arguments_are_eager_even_when_unused_or_excess() {
    for (source, keywords) in [
        ("def run(unused);7;end", false),
        ("def run;7;end", false),
        ("def run;7;end", true),
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(48 << 10),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let huge = Value::bytes(vec![b'x'; 128 << 10]);
        let (args, kwargs) = if keywords {
            (vec![], vec![("unused".into(), huge)])
        } else {
            (vec![huge], vec![])
        };
        let mut ctx = CallContext::new(options.clone());
        assert_eq!(
            check(&mut ctx, &script, &args, &kwargs, &options)
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(
            script
                .call_with_keywords("run", &args, &kwargs, options)
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
    }
    let script = Engine::new()
        .compile("def run(x);begin;x;rescue LimitError;9;end;end")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    assert_eq!(
        check(&mut ctx, &script, &[deep()], &[], &CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert_eq!(
        script
            .call("run", &[deep()], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert!(ctx.checkpoint().is_ok());
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn function_lookup_and_strict_validation_precede_argument_imports() {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("def run(x);x;end").unwrap();
    let poison = HostMethod::new("send", |_, _, _| panic!("poison called")).value();
    let options = CallOptions {
        globals: [("unused".into(), poison)].into(),
        ..CallOptions::default()
    };
    let args = [deep()];
    let mut ctx = CallContext::new(options.clone());
    let error = entry::check(
        &mut ctx,
        Call {
            script: &script,
            name: "unknown",
            arguments: &args,
            keywords: &[],
            options: &options,
        },
    )
    .unwrap_err();
    let actual = script.call("unknown", &args, options.clone()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Name);
    assert_eq!(error.message, actual.message);
    let error = check(&mut ctx, &script, &args, &[], &options).unwrap_err();
    let actual = script.call("run", &args, options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert_eq!(error.message, actual.message);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
    let options = CallOptions {
        limits: Limits {
            memory_bytes: Some(0),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let mut ctx = CallContext::new(options.clone());
    assert_eq!(
        entry::check(
            &mut ctx,
            Call {
                script: &script,
                name: "unknown",
                arguments: &args,
                keywords: &[],
                options: &options
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Name
    );
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

#[test]
fn opaque_factories_remain_explicit_while_nominal_arguments_are_analyzed() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = Engine::new();
    engine.register("tick", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine
        .compile("class C;tick();end;def run(x);7;end")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    let checked = check(
        &mut ctx,
        &script,
        &[Value::int(1)],
        &[],
        &CallOptions::default(),
    )
    .unwrap();
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert!(checked.analysis.contexts > 0);
    drop(checked);
    assert_eq!(
        check(&mut ctx, &script, &[deep()], &[], &CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Recursion
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let script = engine.compile("def run(x);7;end").unwrap();
    let count = effects.clone();
    let options = CallOptions {
        capabilities: vec![Capability::new("opaque", move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(Value::nil())
        })],
        ..CallOptions::default()
    };
    let checked = check(&mut ctx, &script, &[deep()], &[], &options).unwrap();
    assert!(!checked.analysis.incomplete.data.is_empty());
    assert_eq!(checked.analysis.contexts, 0);
    drop(checked);
    let instance = value("class C;end;C.new");
    let checked = check(&mut ctx, &script, &[instance], &[], &CallOptions::default()).unwrap();
    assert!(checked.analysis.incomplete.data.is_empty(), "{checked:?}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    drop(checked);
    assert_eq!(ctx.stats().retained_memory_bytes, 0);
}

fn work(
    ctx: &mut CallContext,
    script: &Script,
    arguments: &[Value],
    keywords: &[(String, Value)],
    options: &CallOptions,
) -> Result<()> {
    let checked = check(ctx, script, arguments, keywords, options)?;
    let Analysis {
        issues, incomplete, ..
    } = &checked.analysis;
    assert!(issues.data.is_empty(), "{checked:?}");
    assert!(incomplete.data.is_empty(), "{checked:?}");
    Ok(())
}

#[test]
fn concrete_entry_and_descriptor_tables_obey_exact_and_sampled_quotas() {
    let method = HostMethod::new("send", |_, _, _| panic!("checker ran a callback"))
        .with_signature(signature(Some("array<Alias>"), "Alias", false))
        .unwrap();
    let args = [Value::object(vec![(b"send".to_vec(), method.value())])];
    let keywords = [("items".into(), value("[:ready]"))];
    let options = CallOptions {
        globals: [("Alias".into(), value("enum State;Ready;end;State"))].into(),
        ..CallOptions::default()
    };
    let script = Engine::new()
        .compile("def run(sms,items:,extra:7);sms.send(items).name;end")
        .unwrap();
    let mut ctx = CallContext::new(CallOptions::default());
    work(&mut ctx, &script, &args, &keywords, &options).unwrap();
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
            work(&mut ctx, &script, &args, &keywords, &options)
                .err()
                .map(|error| error.kind),
            expected
        );
        if let Some(expected) = expected {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
    for sample in 0..24 {
        for memory in [false, true] {
            let mut limits = Limits::default();
            let expected = if memory {
                limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 24);
                ErrorKind::Memory
            } else {
                limits.steps = Some(stats.steps * sample as u64 / 24);
                ErrorKind::Steps
            };
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(
                work(&mut ctx, &script, &args, &keywords, &options)
                    .unwrap_err()
                    .kind,
                expected
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}

#[test]
fn exact_call_entry_preserves_cancelled_and_expired_budgets() {
    let script = Engine::new().compile("def run(x);x;end").unwrap();
    for name in ["run", "unknown"] {
        for deadline in [false, true] {
            let mut options = CallOptions::default();
            if deadline {
                options.deadline = Some(std::time::Instant::now());
            } else {
                options.cancellation.cancel();
            }
            let mut ctx = CallContext::new(options.clone());
            let error = entry::check(
                &mut ctx,
                Call {
                    script: &script,
                    name,
                    arguments: &[Value::int(1)],
                    keywords: &[],
                    options: &options,
                },
            )
            .unwrap_err();
            let expected = if deadline {
                ErrorKind::Deadline
            } else {
                ErrorKind::Cancelled
            };
            assert_eq!(error.kind, expected);
            assert_eq!(
                script
                    .call(name, &[Value::int(1)], options)
                    .unwrap_err()
                    .kind,
                expected
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
