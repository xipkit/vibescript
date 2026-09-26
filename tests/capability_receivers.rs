mod common;

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript::{
    CallOptions, CancellationToken, Capability, Engine, ErrorKind, HostMethod, ModuleConfig,
    Signature, SignatureParam, Value,
};

fn field(value: &Value, name: &str) -> Option<Value> {
    value
        .as_hash()?
        .iter()
        .find(|(key, _)| key.as_bytes() == Some(name.as_bytes()))
        .map(|(_, value)| value.clone())
}

fn tag(receiver: Option<Value>) -> Value {
    match receiver {
        Some(receiver) => field(&receiver, "tag").unwrap_or_else(Value::nil),
        None => Value::bytes("none"),
    }
}

/// Returns the receiver's `tag` field, or "none" when called without a receiver.
fn reader(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, _, _| Ok(tag(call.receiver()?)))
}

/// Reads the receiver, runs the block if given, then reads the receiver again.
fn nester(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, _, _| {
        let before = tag(call.receiver()?);
        let inner = if call.block_given() {
            call.call_block(&[])?
        } else {
            Value::nil()
        };
        let after = tag(call.receiver()?);
        call.context().array(&[before, inner, after])
    })
}

/// `set(key, value)` publishes one field of its receiver, which is how a
/// static program rewrites the capability's data.
fn setter(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, args, _| {
        let key = args[0].as_bytes().unwrap().to_vec();
        Ok(Value::boolean(call.set_receiver_field(&key, &args[1])?))
    })
}

/// The capability's value, whose `inner` object shares the very same `read`
/// descriptor value.
fn value() -> Value {
    let read = reader("cap.read").value();
    let nest = nester("cap.nest").value();
    let set = setter("cap.set").value();
    let inner = Value::object(vec![
        (b"tag".to_vec(), Value::int(2)),
        (b"read".to_vec(), read.clone()),
        (b"nest".to_vec(), nest.clone()),
        (b"set".to_vec(), set.clone()),
    ]);
    Value::object(vec![
        (b"tag".to_vec(), Value::int(1)),
        (b"read".to_vec(), read),
        (b"nest".to_vec(), nest),
        (b"set".to_vec(), set),
        (b"inner".to_vec(), inner),
    ])
}

/// Grants `cap`.
fn options() -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::from_value("cap", value())],
        ..CallOptions::default()
    }
}

/// An engine that declares the value template of every capability
/// `options` grants.
fn declaring(options: &CallOptions) -> Engine {
    let mut engine = Engine::new();
    for capability in &options.capabilities {
        engine.declare_capability(capability).unwrap();
    }
    engine
}

/// Runs `body` as a function returning `any`, with the capabilities that
/// `options` grants declared.
fn run(body: &str, options: CallOptions) -> vibescript::Result<vibescript::Outcome> {
    declaring(&options)
        .compile(&format!("def run -> any\n{body}\nend"))
        .unwrap()
        .call("run", &[], options)
}

fn check(cases: &[(&str, &str)]) {
    for (body, expected) in cases {
        let result = run(body, options()).unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.to_string(), *expected, "{body}");
    }
}

#[test]
fn member_calls_keep_their_receiver_on_static_routes() {
    let engine = declaring(&options());
    for (body, expected) in [
        ("cap.read()", "1"),
        ("cap.read(1, 2)", "1"),
        ("cap.read(*[1])", "1"),
        ("cap.read(limit: 1)", "1"),
        ("cap.read() { 0 }", "1"),
        ("cap.read(*[1]) { 0 }", "1"),
        ("a = cap; a.read()", "1"),
        ("cap.dup.read()", "1"),
        ("[cap].fetch(0).read()", "1"),
    ] {
        let script = engine
            .compile(&format!("def run -> any\n{body}\nend"))
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        let result = script
            .call("run", &[], options())
            .unwrap_or_else(|error| panic!("{body}: {error}"));
        assert_eq!(result.value.to_string(), expected, "{body}");
    }
    // A namespace is not indexed, so the callee is never computed, and a
    // method is called with a dot.
    for (source, code, at) in [
        ("def run -> any\ncap[\"read\"]()\nend", "V0112", "cap["),
        ("def run -> any\ncap::read()\nend", "V0416", "::"),
    ] {
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error)[0], code, "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{source}"
        );
    }
}

#[test]
fn nested_aliased_and_duplicated_receivers_select_the_object_actually_used() {
    check(&[
        ("cap.inner.read()", "2"),
        ("[cap.read(), cap.inner.read()]", "[1, 2]"),
        ("a = cap; a.read()", "1"),
        ("cap.dup.read()", "1"),
        ("[cap][0]&.read()", "1"),
        ("a = cap.inner; a.read()", "2"),
    ]);
}

#[test]
fn receivers_snapshot_the_value_selected_before_arguments_and_blocks_run() {
    check(&[
        ("cap.set(\"tag\", 7); cap.read()", "7"),
        ("cap.inner.set(\"tag\", 8); cap.inner.read()", "8"),
        (
            "copy = cap; cap.set(\"tag\", 7); [copy.read(), cap.read()]",
            "[1, 7]",
        ),
        (
            "[cap.nest { cap.set(\"tag\", 9); cap.tag }, cap.tag]",
            "[[1, 9, 1], 9]",
        ),
    ]);
}

/// An argument that rewrites the receiver's field runs after the callee was
/// selected, so the callback sees the original object while the script sees
/// the new field afterwards. The argument publishes the field, since a static
/// program cannot index the capability.
#[test]
fn arguments_that_rewrite_the_receiver_do_not_change_the_selected_snapshot() {
    check(&[
        ("[cap.read(cap.set(\"tag\", 9)), cap.tag]", "[1, 9]"),
        (
            "[cap.read(*[0].map { |x| cap.set(\"tag\", 9); x }), cap.tag]",
            "[1, 9]",
        ),
        ("[cap.read(limit: cap.set(\"tag\", 9)), cap.tag]", "[1, 9]"),
        ("[cap.read(cap.set(\"tag\", 9)) { 0 }, cap.tag]", "[1, 9]"),
        (
            "[cap.inner.read(cap.inner.set(\"tag\", 8)), cap.inner.tag]",
            "[2, 8]",
        ),
    ]);
}

#[test]
fn builtin_named_host_methods_keep_receivers_on_special_dispatch_routes() {
    for name in ["push", "call", "each", "is_type?", "send"] {
        for args in [
            "cap.set(\"tag\", 9)",
            "*[0].map { |x| cap.set(\"tag\", 9); x }",
        ] {
            let source = format!("[cap.{name}({args}), cap.tag]");
            let options = CallOptions {
                capabilities: vec![Capability::from_value(
                    "cap",
                    Value::object(vec![
                        (b"tag".to_vec(), Value::int(1)),
                        (
                            name.as_bytes().to_vec(),
                            reader(&format!("cap.{name}")).value(),
                        ),
                        (b"set".to_vec(), setter("cap.set").value()),
                    ]),
                )],
                ..CallOptions::default()
            };
            let outcome = run(&source, options).unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(outcome.value.to_string(), "[1, 9]", "{source}");
        }
    }
}

#[test]
fn signed_methods_keep_the_receiver_through_argument_normalization() {
    let method = HostMethod::new_with_block("cap.read", |call, args, _| {
        let selected = tag(call.receiver()?);
        call.context().array(&[selected, args[0].clone()])
    })
    .with_signature(Signature {
        params: vec![SignatureParam {
            name: "value".into(),
            ty: "int".into(),
            optional: false,
        }],
        result: "array<int>".into(),
        accepts_block: true,
    })
    .unwrap();
    for source in ["cap.read(3)", "cap.read(*[3])", "cap.read(3) { 0 }"] {
        let options = CallOptions {
            capabilities: vec![Capability::from_value(
                "cap",
                Value::object(vec![
                    (b"tag".to_vec(), Value::int(1)),
                    (b"read".to_vec(), method.value()),
                ]),
            )],
            ..CallOptions::default()
        };
        let outcome = run(source, options).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(outcome.value.to_string(), "[1, 3]", "{source}");
    }
}

#[test]
fn top_level_scripts_select_receivers_like_called_functions() {
    for (source, expected) in [
        ("cap.read()", "1"),
        ("cap.inner.read { 0 }", "2"),
        ("[cap.read(cap.set(\"tag\", 9)), cap.tag]", "[1, 9]"),
    ] {
        let result = declaring(&options())
            .compile(source)
            .unwrap()
            .run(options())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(result.value.to_string(), expected, "{source}");
    }
}

/// A snapshot that the callback drops leaves nothing behind once the call ends.
#[test]
fn dropped_receiver_snapshots_retain_no_memory_after_the_call() {
    let with = run("cap.read(); 0", options()).unwrap();
    let without = run("0", options()).unwrap();
    assert_eq!(with.value.as_int(), Some(0));
    assert_eq!(
        with.stats.retained_memory_bytes,
        without.stats.retained_memory_bytes
    );
}

#[test]
fn nested_host_and_block_reentry_restores_the_outer_receiver() {
    check(&[
        ("cap.nest { cap.inner.nest { 0 } }", "[1, [2, 0, 2], 1]"),
        (
            "cap.inner.nest { cap.nest { cap.inner.read() } }",
            "[2, [1, 2, 1], 2]",
        ),
        ("cap.nest { cap.read() }", "[1, 1, 1]"),
    ]);
}

#[test]
fn bare_registered_and_globally_granted_methods_have_no_receiver() {
    let mut engine = Engine::new();
    engine.register_method("bare", reader("bare"));
    let script = engine.compile("def run -> any; bare(); end").unwrap();
    assert_eq!(
        script
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .to_string(),
        "none"
    );
    let solo = CallOptions {
        capabilities: vec![Capability::new("solo", |_| Ok(reader("solo").value()))],
        ..CallOptions::default()
    };
    let mut engine = Engine::new();
    engine
        .declare_capability(&Capability::from_value("solo", reader("solo").value()))
        .unwrap();
    let script = engine.compile("def run -> any; solo(); end").unwrap();
    assert_eq!(
        script.call("run", &[], solo).unwrap().value.to_string(),
        "none"
    );
}

#[test]
fn required_scripts_see_the_receiver_of_the_receiving_calls_grant() {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
    fs::create_dir_all(&base).unwrap();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = base.join(format!(
        "receivers-{}-{}",
        common::process_id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("reader.vibe"),
        "def read_tags -> array<any>\n  [cap.read(), cap.inner.read(), cap.dup.read()]\nend\n",
    )
    .unwrap();
    let mut engine = declaring(&options());
    engine
        .set_module_config(ModuleConfig {
            paths: vec![dir.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine
        .compile("def run -> any\n  require(\"reader\").read_tags\nend")
        .unwrap();
    let result = script.call(
        "run",
        &[],
        CallOptions {
            allow_require: true,
            ..options()
        },
    );
    let _ = fs::remove_dir_all(&dir);
    assert_eq!(result.unwrap().value.to_string(), "[1, 2, 1]");
}

#[test]
fn escaped_receiver_snapshots_keep_their_expired_grant() {
    let kept: Arc<Mutex<Option<Value>>> = Arc::default();
    let store = kept.clone();
    let keep = HostMethod::new_with_block("cap.keep", move |call, _, _| {
        *store.lock().unwrap() = call.receiver()?;
        Ok(Value::nil())
    });
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"tag".to_vec(), Value::int(1)),
                (b"read".to_vec(), reader("cap.read").value()),
                (b"keep".to_vec(), keep.value()),
            ]),
        )],
        ..CallOptions::default()
    };
    run("cap.keep()", options).unwrap();
    let escaped = kept.lock().unwrap().take().unwrap();
    assert_eq!(field(&escaped, "tag").unwrap().as_int(), Some(1));
    // Host values may only re-enter through a capability binding, never as a
    // plain global, so the snapshot is offered back as a value template.
    let later = || CallOptions {
        capabilities: vec![Capability::from_value("old", escaped.clone())],
        ..CallOptions::default()
    };
    assert_eq!(run("old.tag", later()).unwrap().value.as_int(), Some(1));
    let error = run("old.read()", later()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime, "{error}");
    assert!(error.message.contains("not granted"), "{error}");
}

/// A quota failure latched by a block call stays visible through `receiver`.
#[test]
fn receiver_reads_cannot_swallow_a_latched_step_failure() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let method = HostMethod::new_with_block("cap.spin", move |call, _, _| {
        assert!(call.receiver()?.is_some());
        let first = call.call_block(&[]).unwrap_err().kind;
        let second = call.receiver().unwrap_err().kind;
        observed.lock().unwrap().push((first, second));
        Ok(Value::int(5))
    });
    let mut options = CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"tag".to_vec(), Value::int(1)),
                (b"spin".to_vec(), method.value()),
            ]),
        )],
        ..CallOptions::default()
    };
    options.limits.steps = Some(10_000);
    let error = run("cap.spin { while true; 1; end }", options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps, "{error}");
    assert_eq!(
        *seen.lock().unwrap(),
        [(ErrorKind::Steps, ErrorKind::Steps)]
    );
}

#[test]
fn receiver_reads_surface_cancellation_and_latched_errors() {
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let method = HostMethod::new_with_block("cap.cancel", move |call, _, _| {
        assert!(call.receiver()?.is_some());
        cancel.cancel();
        let error = call.receiver().unwrap_err();
        observed.lock().unwrap().push(error.kind);
        let error = call.receiver().unwrap_err();
        observed.lock().unwrap().push(error.kind);
        Ok(Value::int(5))
    });
    let options = CallOptions {
        cancellation,
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"tag".to_vec(), Value::int(1)),
                (b"cancel".to_vec(), method.value()),
            ]),
        )],
        ..CallOptions::default()
    };
    let error = run("cap.cancel()", options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(
        *seen.lock().unwrap(),
        [ErrorKind::Cancelled, ErrorKind::Cancelled]
    );
}

#[test]
fn detached_methods_stay_rejected_even_when_receivers_are_tracked() {
    assert_eq!(run("cap.read", options()).unwrap().value.as_int(), Some(1));
    // A namespace is not indexed and a local is never called, so a method
    // cannot be detached into a value.
    let engine = declaring(&options());
    for (body, codes, at) in [
        ("cap[:read]", &["V0112", "V0409"][..], "cap["),
        ("f = cap::read; f()", &["V0416", "V0310"], "::"),
        ("f = cap[:read]; f()", &["V0112", "V0409", "V0310"], "cap["),
        ("[cap[:read]]", &["V0112", "V0409"], "cap["),
    ] {
        let source = format!("def run -> any\n{body}\nend");
        let error = engine.compile(&source).err().unwrap();
        assert_eq!(common::codes(&error), codes, "{body}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{body}"
        );
    }
}
