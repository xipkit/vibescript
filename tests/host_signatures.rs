use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Capability, Engine, Error, ErrorKind, HostMethod, Signature,
    SignatureParam, Value,
};

fn signature(params: &[(&str, &str, bool)], result: &str, accepts_block: bool) -> Signature {
    Signature {
        params: params
            .iter()
            .map(|(name, ty, optional)| SignatureParam {
                name: (*name).into(),
                ty: (*ty).into(),
                optional: *optional,
            })
            .collect(),
        result: result.into(),
        accepts_block,
    }
}

fn echo(ty: &str) -> HostMethod {
    HostMethod::new("typed.echo", |_, args, _| Ok(args[0].clone()))
        .with_signature(signature(&[("value", ty, false)], ty, false))
        .unwrap()
}

fn options(method: HostMethod) -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::new("typed", move |_| {
            Ok(Value::object(vec![(b"echo".to_vec(), method.value())]))
        })],
        ..CallOptions::default()
    }
}

#[test]
fn signatures_validate_declarations_without_changing_shared_methods() {
    for ty in ["array<", "int trailing", "int; nil", "int\nstring", "int??"] {
        let error = HostMethod::new("test", |_, _, _| Ok(Value::nil()))
            .with_signature(signature(&[("input", ty, false)], "", false))
            .unwrap_err();
        assert!(
            error.message.contains("signature for test parameter input"),
            "{error}"
        );
    }
    let method = HostMethod::new("test", |_, _, _| Ok(Value::nil()));
    let error = method
        .clone()
        .with_signature(signature(&[], "hash<", false))
        .unwrap_err();
    assert!(error.message.contains("signature for test result"));
    let error = method
        .clone()
        .with_signature(signature(
            &[("a", "", true), ("b", "int", false)],
            "",
            false,
        ))
        .unwrap_err();
    assert!(error.message.contains("optional parameters must trail"));
    let first = method
        .clone()
        .with_signature(signature(&[("", " int | nil ", false)], "", false))
        .unwrap();
    let mut changed = first.signature().unwrap().clone();
    changed.params[0].ty = "string".into();
    let second = first.clone().with_signature(changed).unwrap();
    assert!(method.signature().is_none());
    assert_eq!(first.signature().unwrap().params[0].ty, " int | nil ");
    assert_eq!(second.signature().unwrap().params[0].ty, "string");
    let deep = format!("{}int{}", "array<".repeat(66), ">".repeat(66));
    assert!(
        method
            .with_signature(signature(&[("", &deep, false)], "", false))
            .is_err()
    );
}

#[test]
fn typed_capability_methods_cover_every_immediate_dispatch_form() {
    for source in [
        "typed.echo(7)",
        "typed.echo 7",
        "typed::echo(7)",
        "typed[:echo](7)",
        "(typed[:echo])(7)",
        "typed.send(:echo,7)",
        "typed.public_send(:echo,7)",
        "typed&.echo(7)",
        "typed.dup.echo(7)",
        "[typed][0].echo(7)",
        "typed.echo(*[7])",
    ] {
        for strict in [false, true] {
            let mut engine = Engine::new();
            engine.set_strict_effects(strict);
            let outcome = engine
                .compile(source)
                .unwrap()
                .run(options(echo("int")))
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(outcome.value.as_int(), Some(7), "{source}");
            assert_eq!(outcome.stats.retained_memory_bytes, 0);
        }
    }
    for source in [
        "typed.echo",
        "typed::echo",
        "typed[:echo]",
        "a=typed[:echo]; a(7)",
        "[typed[:echo]]",
        "{f: typed::echo}",
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(options(echo("int")))
            .unwrap_err();
        assert!(
            error.message.contains("cannot be used as a value"),
            "{source}: {error}"
        );
    }
}

#[test]
fn signatures_reject_invalid_calls_before_the_callback_and_keep_omissions() {
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let method = HostMethod::new("typed.echo", move |ctx, args, _| {
        seen.fetch_add(1, Ordering::Relaxed);
        ctx.array(args)
    })
    .with_signature(signature(
        &[("value", "int", false), ("flag", "bool", true)],
        "array",
        false,
    ))
    .unwrap();
    for (source, message) in [
        (
            "typed.echo()",
            "typed.echo expects at least 1 arguments, got 0",
        ),
        (
            "typed.echo(1,true,2)",
            "typed.echo expects at most 2 arguments, got 3",
        ),
        (
            "typed.echo(1, flag: true)",
            "typed.echo does not take keyword arguments",
        ),
        ("typed.echo(1) { 2 }", "typed.echo does not take a block"),
        (
            "typed.echo(\"bad\")",
            "typed.echo argument value expected int, got string",
        ),
        (
            "typed.echo(1, nil)",
            "typed.echo argument flag expected bool, got nil",
        ),
    ] {
        let error = Engine::new()
            .compile(source)
            .unwrap()
            .run(options(method.clone()))
            .unwrap_err();
        assert_eq!(error.message, message, "{source}");
    }
    assert_eq!(count.load(Ordering::Relaxed), 0);
    for (source, expected) in [
        ("typed.echo(1)", "[1]"),
        ("typed.echo(1,true)", "[1, true]"),
    ] {
        assert_eq!(
            Engine::new()
                .compile(source)
                .unwrap()
                .run(options(method.clone()))
                .unwrap()
                .value
                .to_string(),
            expected
        );
    }
    let method = HostMethod::new("typed.echo", |_, _, _| {
        panic!("invalid unnamed argument ran")
    })
    .with_signature(signature(&[("", "int", false)], "", false))
    .unwrap();
    assert_eq!(
        Engine::new()
            .compile("typed.echo(nil)")
            .unwrap()
            .run(options(method))
            .unwrap_err()
            .message,
        "typed.echo argument 1 expected int, got nil"
    );
}

#[test]
fn host_signatures_normalize_nested_enums_without_mutating_arguments() {
    let source = "enum Status; Draft; Sent; end; def run; a=[{state: :draft}]; b=typed.echo(a); [a[0].state.is_type?(:symbol), b[0].state == Status::Draft]; end";
    let result = Engine::new()
        .compile(source)
        .unwrap()
        .call("run", &[], options(echo("array<{ state: Status }>")))
        .unwrap();
    assert_eq!(result.value.to_string(), "[true, true]");
    for (ty, value) in [
        ("int | string", "\"yes\""),
        ("int?", "nil"),
        ("array<int>", "[1,2]"),
        ("hash<string,int>", "{a: 1}"),
        ("{ a: int, b?: string }", "{a: 1}"),
        ("any", "{a: [1,nil]}"),
        ("", "[1,2]"),
    ] {
        let source = format!("typed.echo({value}) == {value}");
        assert_eq!(
            Engine::new()
                .compile(&source)
                .unwrap()
                .run(options(echo(ty)))
                .unwrap()
                .value
                .to_string(),
            "true",
            "{ty}"
        );
    }
}

#[test]
fn custom_contracts_see_raw_arguments_and_normalized_results_once() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let called = trace.clone();
    let arguments = trace.clone();
    let returned = trace.clone();
    let method = HostMethod::new("typed.echo", move |_, args, _| {
        called
            .lock()
            .unwrap()
            .push(format!("call:{}", args[0].type_name()));
        Ok(Value::symbol("sent"))
    })
    .with_signature(signature(&[("status", "Status", false)], "Status", false))
    .unwrap()
    .with_contract(
        move |_, args, _| {
            arguments
                .lock()
                .unwrap()
                .push(format!("arg:{}", args[0].type_name()));
            Ok(())
        },
        move |_, result| {
            returned
                .lock()
                .unwrap()
                .push(format!("return:{}", result.type_name()));
            Ok(())
        },
    );
    let result = Engine::new()
        .compile("enum Status; Draft; Sent; end; typed.echo(:draft) == Status::Sent")
        .unwrap()
        .run(options(method))
        .unwrap();
    assert_eq!(result.value.to_string(), "true");
    assert_eq!(
        *trace.lock().unwrap(),
        ["arg:symbol", "call:enum value", "return:enum value"]
    );
}

#[test]
fn signature_returns_are_enforced_and_callback_failures_preserved() {
    for (ty, value, expected) in [
        (
            "int",
            Value::bytes("bad"),
            "return value for typed.echo expected int, got string",
        ),
        (
            "Missing",
            Value::int(1),
            "return type check failed for typed.echo: unknown type Missing",
        ),
    ] {
        let method = HostMethod::new("typed.echo", move |_, _, _| Ok(value.clone()))
            .with_signature(signature(&[], ty, false))
            .unwrap();
        assert_eq!(
            Engine::new()
                .compile("typed.echo()")
                .unwrap()
                .run(options(method))
                .unwrap_err()
                .message,
            expected
        );
    }
    let method = HostMethod::new("typed.echo", |_, _, _| {
        Err(Error::new(ErrorKind::Runtime, "host failed"))
    })
    .with_signature(signature(&[], "Missing", false))
    .unwrap();
    assert_eq!(
        Engine::new()
            .compile("typed.echo()")
            .unwrap()
            .run(options(method))
            .unwrap_err()
            .message,
        "host failed"
    );
}

#[test]
fn signatures_validate_break_results_and_preserve_nonlocal_returns() {
    let method = HostMethod::new_with_block("typed.echo", |call, args, _| call.call_block(args))
        .with_signature(signature(&[("value", "int", false)], "int", true))
        .unwrap();
    for (body, expected) in [
        ("typed.echo(1) { |n| n+1 }", "2"),
        ("typed.echo(1) { next 7 }", "7"),
        ("typed.echo(1) { break 8 }", "8"),
        ("typed.echo(1) { return \"ok\" }; 99", "ok"),
        (
            "begin; typed.echo(1) { break \"bad\" }; rescue => e; e.message; end",
            "return value for typed.echo expected int, got string",
        ),
    ] {
        let result = Engine::new()
            .compile(&format!("def run; {body}; end"))
            .unwrap()
            .call("run", &[], options(method.clone()))
            .unwrap();
        assert_eq!(result.value.to_string(), expected, "{body}");
    }
    let method = HostMethod::new("typed.echo", |_, _, _| Ok(Value::int(7)))
        .with_signature(signature(&[], "int", true))
        .unwrap();
    assert_eq!(
        Engine::new()
            .compile("typed.echo { raise \"unused\" }")
            .unwrap()
            .run(options(method))
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn registered_methods_keep_compiled_snapshots_and_call_resolution() {
    let mut engine = Engine::new();
    engine.register_method("echo", echo("int"));
    let earlier = engine.compile("echo(7)").unwrap();
    for source in [
        "echo(7)",
        "echo(*[7])",
        "(echo)(7)",
        "module M; def self.run; echo(7); end; end; M.run",
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap()
                .value
                .as_int(),
            Some(7),
            "{source}"
        );
    }
    engine.register_method("echo", echo("string"));
    assert_eq!(
        earlier.run(CallOptions::default()).unwrap().value.as_int(),
        Some(7)
    );
    assert!(
        engine
            .compile("echo(7)")
            .unwrap()
            .run(CallOptions::default())
            .is_err()
    );
    assert_eq!(
        engine
            .compile("def echo(n); n+1; end; echo(7)")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(8)
    );
    engine.register("echo", |_, _| Ok(Value::int(9)));
    assert_eq!(
        engine
            .compile("echo(nil)")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(9)
    );
    engine.register_method("echo", echo("int"));
    let global = HostMethod::new("echo", |_, _, _| Ok(Value::int(11)));
    assert_eq!(
        engine
            .compile("echo(7)")
            .unwrap()
            .run(CallOptions {
                globals: [("echo".into(), global.value())].into(),
                ..CallOptions::default()
            })
            .unwrap()
            .value
            .as_int(),
        Some(11)
    );
    engine.register_method(
        "visit",
        HostMethod::new_with_block("visit", |call, args, _| call.call_block(args))
            .with_signature(signature(&[("n", "int", false)], "int", true))
            .unwrap(),
    );
    assert_eq!(
        engine
            .compile("visit(7) { |n| n+1 }")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(8)
    );
}

#[test]
fn typed_methods_work_in_globals_without_regranting_saved_capabilities() {
    let method = echo("int");
    let opts = CallOptions {
        globals: [("echo".into(), method.value())].into(),
        ..CallOptions::default()
    };
    assert_eq!(
        Engine::new()
            .compile("echo(7)")
            .unwrap()
            .run(opts)
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    let script = Engine::new()
        .compile("def save; typed; end; def use(saved); saved.echo(7); end")
        .unwrap();
    let saved = script
        .call("save", &[], options(method.clone()))
        .unwrap()
        .value;
    assert!(
        script
            .call("use", &[saved], options(method))
            .unwrap_err()
            .message
            .contains("was not granted")
    );
}

#[test]
fn signature_metadata_and_normalization_obey_exact_budgets() {
    let signature = signature(
        &[("label", "array<int | string>", false)],
        "array<int | string>",
        false,
    );
    let mut large = signature.clone();
    large.params[0].name = "x".repeat(16384);
    let method = HostMethod::new("typed.echo", |_, args, _| Ok(args[0].clone()))
        .with_signature(large)
        .unwrap();
    let script = Engine::new().compile("typed.echo([1,2,3])").unwrap();
    let outcome = script.run(options(method.clone())).unwrap();
    assert!(outcome.stats.peak_memory_bytes > 16384);
    let smaller = HostMethod::new("typed.echo", |_, args, _| Ok(args[0].clone()))
        .with_signature(signature)
        .unwrap();
    let small = script.run(options(smaller)).unwrap();
    assert!(outcome.stats.peak_memory_bytes >= small.stats.peak_memory_bytes + 16000);
    for exact in [true, false] {
        for memory in [true, false] {
            let mut opts = options(method.clone());
            if memory {
                opts.limits.memory_bytes =
                    Some(outcome.stats.peak_memory_bytes - usize::from(!exact));
            } else {
                opts.limits.steps = Some(outcome.stats.steps - u64::from(!exact));
            }
            let result = script.run(opts);
            if exact {
                assert!(result.is_ok(), "{result:?}");
            } else {
                assert_eq!(
                    result.unwrap_err().kind,
                    if memory {
                        ErrorKind::Memory
                    } else {
                        ErrorKind::Steps
                    }
                );
            }
        }
    }
    let save = Engine::new()
        .compile("typed")
        .unwrap()
        .run(options(method))
        .unwrap();
    assert!(save.stats.retained_memory_bytes >= 16384);
}

#[test]
fn typed_callback_cancellation_wins_over_result_validation() {
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let method = HostMethod::new("typed.echo", move |_, _, _| {
        cancel.cancel();
        Ok(Value::bytes("bad"))
    })
    .with_signature(signature(&[], "int", false))
    .unwrap();
    let mut opts = options(method);
    opts.cancellation = cancellation;
    assert_eq!(
        Engine::new()
            .compile("begin; typed.echo(); rescue; 7; ensure; raise \"wrong\"; end")
            .unwrap()
            .run(opts)
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn typed_block_recursion_reaches_the_default_limit_without_stack_overflow() {
    let mut engine = Engine::new();
    engine.register_method(
        "visit",
        HostMethod::new_with_block("visit", |call, args, _| call.call_block(args))
            .with_signature(signature(&[("n", "int", false)], "int", true))
            .unwrap(),
    );
    let script = engine
        .compile("def recur(n); visit(n) { |i| recur(i+1) }; end")
        .unwrap();
    let error = script
        .call("recur", &[Value::int(0)], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
}

#[test]
fn typed_host_results_preserve_pending_writes_and_binding_defaults() {
    let opts = options(echo("int"));
    for (body, expected) in [
        ("a=[1]; a[0]+=typed.echo(2); a", "[3]"),
        ("a=[1]; a[typed.echo(0)]=typed.echo(3); a", "[3]"),
        ("a={n:1}; a.n+=typed.echo(2); a.n", "3"),
        ("[1,2,3].sum { |n| typed.echo(n) }", "6"),
        (
            "class Box; property n: int; def initialize(@n); end; end; a=Box.new(1); a.n=typed.echo(3); a.n",
            "3",
        ),
        ("def f(n=typed.echo(3)); n; end; f()", "3"),
    ] {
        let result = Engine::new()
            .compile(body)
            .unwrap()
            .run(opts.clone())
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.to_string(), expected, "{body}");
    }
    let mut engine = Engine::new();
    engine.register_method("len", echo("int"));
    assert_eq!(
        engine
            .compile("len(7)")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn rescued_signature_lookup_errors_obey_exact_diagnostic_budgets() {
    let method = echo("Missing");
    let script = Engine::new()
        .compile("begin; typed.echo(1); rescue => e; e.message; end")
        .unwrap();
    let outcome = script.run(options(method.clone())).unwrap();
    assert_eq!(
        outcome.value.to_string(),
        "typed.echo argument value type check failed: unknown type Missing"
    );
    for exact in [true, false] {
        for memory in [true, false] {
            let mut opts = options(method.clone());
            if memory {
                opts.limits.memory_bytes =
                    Some(outcome.stats.peak_memory_bytes - usize::from(!exact));
            } else {
                opts.limits.steps = Some(outcome.stats.steps - u64::from(!exact));
            }
            let result = script.run(opts);
            if exact {
                assert!(result.is_ok(), "{result:?}");
            } else {
                assert_eq!(
                    result.unwrap_err().kind,
                    if memory {
                        ErrorKind::Memory
                    } else {
                        ErrorKind::Steps
                    }
                );
            }
        }
    }
}
