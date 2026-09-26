mod common;

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, CancellationToken, Capability, ErrorKind, HostMethod, Limits, Value,
};

fn field(value: &Value, name: &str) -> Option<Value> {
    value
        .as_hash()?
        .iter()
        .find(|(key, _)| key.as_bytes() == Some(name.as_bytes()))
        .map(|(_, value)| value.clone())
}

/// `put(key, value)` publishes one field and returns whether a binding received it.
fn put(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, args, _| {
        let key = args[0].as_bytes().unwrap().to_vec();
        let published = call.set_receiver_field(&key, &args[1])?;
        Ok(Value::boolean(published))
    })
}

/// `get(key)` reads a field from a fresh receiver snapshot.
fn get(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, args, _| {
        let receiver = call.receiver()?.expect("member call");
        Ok(field(
            &receiver,
            std::str::from_utf8(args[0].as_bytes().unwrap()).unwrap(),
        )
        .unwrap_or_else(Value::nil))
    })
}

/// Publishes `a`, runs the block, publishes `c`, and returns the block's value.
fn around(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, _, _| {
        call.set_receiver_field(b"a", &Value::int(1))?;
        let inner = call.call_block(&[])?;
        call.set_receiver_field(b"c", &Value::int(3))?;
        Ok(inner)
    })
}

/// Publishes a key and returns the key as its own snapshot sees it afterwards.
fn put_then_get(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, args, _| {
        let key = args[0].as_bytes().unwrap().to_vec();
        let published = call.set_receiver_field(&key, &args[1])?;
        let receiver = call.receiver()?.expect("member call");
        let seen = field(&receiver, std::str::from_utf8(&key).unwrap()).unwrap_or_else(Value::nil);
        call.context().array(&[Value::boolean(published), seen])
    })
}

/// Installs a new method descriptor into its receiver.
fn install(name: &str) -> HostMethod {
    HostMethod::new_with_block(name, |call, _, _| {
        let added = HostMethod::new("cap.added", |ctx, _, _| ctx.bytes(b"added"));
        call.set_receiver_field(b"added", &added.value())?;
        Ok(Value::nil())
    })
}

fn methods(prefix: &str) -> Vec<(Vec<u8>, Value)> {
    vec![
        (b"put".to_vec(), put(&format!("{prefix}.put")).value()),
        (b"get".to_vec(), get(&format!("{prefix}.get")).value()),
        (
            b"around".to_vec(),
            around(&format!("{prefix}.around")).value(),
        ),
        (
            b"put_then_get".to_vec(),
            put_then_get(&format!("{prefix}.put_then_get")).value(),
        ),
        (
            b"install".to_vec(),
            install(&format!("{prefix}.install")).value(),
        ),
    ]
}

/// The capability's value: methods and data, including `notes`, which a
/// script updates in place, and an inner object of both.
fn fields() -> Vec<(Vec<u8>, Value)> {
    let notes = || (b"notes".to_vec(), Value::array(vec![Value::int(0)]));
    let mut inner = methods("cap.inner");
    inner.push((b"tag".to_vec(), Value::int(2)));
    inner.push(notes());
    let mut fields = methods("cap");
    fields.push((b"tag".to_vec(), Value::int(1)));
    fields.push(notes());
    fields.push((b"inner".to_vec(), Value::object(inner)));
    fields
}

fn capability() -> Capability {
    Capability::from_value("cap", Value::object(fields()))
}

fn options() -> CallOptions {
    CallOptions {
        capabilities: vec![capability()],
        ..CallOptions::default()
    }
}

/// Runs `body` as a function returning `any`, with the value template of
/// every capability `options` grants declared. Static types never index a
/// namespace, since a host may publish any field into it, so `body` reads
/// published fields through a method such as `get`, or through data the
/// template declares.
fn run(body: &str, options: CallOptions) -> vibescript::Result<vibescript::Outcome> {
    let mut engine = vibescript::Engine::new();
    for capability in &options.capabilities {
        engine.declare_capability(capability).unwrap();
    }
    engine
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
fn published_fields_are_visible_to_later_script_reads() {
    check(&[
        ("cap.put(\"k\", 5)\ncap.get(\"k\")", "5"),
        ("cap.put(\"k\", [1, 2])\ncap.get(\"k\")", "[1, 2]"),
        ("cap.put(\"tag\", 9)\ncap.tag", "9"),
        ("cap.put(\"k\", 5)", "true"),
        ("cap.put(\"k\", 1)\ncap.put(\"k\", 2)\ncap.get(\"k\")", "2"),
    ]);
}

#[test]
fn static_programs_see_publications_through_capability_methods() {
    let mut engine = vibescript::Engine::new();
    engine
        .declare_capability(&Capability::from_value("cap", Value::object(fields())))
        .unwrap();
    let script = engine
        .compile("def run -> any\ncap.put(\"k\", 5)\ncap.get(\"k\")\nend")
        .unwrap();
    let result = script.call("run", &[], options()).unwrap();
    assert_eq!(result.value.to_string(), "5");
    for source in [
        "def run -> any\ncap.put(\"k\", 5)\ncap[\"k\"]\nend",
        "def run\ncap[\"k\"] = 6\nnil\nend",
    ] {
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error)[0], "V0112", "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find("cap[").unwrap(),
            "{source}"
        );
    }
}

#[test]
fn every_call_route_publishes_to_the_binding() {
    check(&[
        ("cap.put \"k\", 1\ncap.get(\"k\")", "1"),
        ("cap.put(*[\"k\", 1])\ncap.get(\"k\")", "1"),
        ("cap.put(\"k\", 1) { 0 }\ncap.get(\"k\")", "1"),
    ]);
}

#[test]
fn nested_receivers_publish_inside_their_capability() {
    check(&[
        (
            "cap.inner.put(\"k\", 1)\n[cap.inner.get(\"k\"), cap.get(\"k\")]",
            "[1, nil]",
        ),
        (
            "i = cap.inner\ni.put(\"k\", 1)\n[i.get(\"k\"), cap.inner.get(\"k\")]",
            "[nil, 1]",
        ),
        (
            "cap.inner.put(\"tag\", 7)\n[cap.tag, cap.inner.tag]",
            "[1, 7]",
        ),
    ]);
}

#[test]
fn blocks_and_later_host_calls_observe_publications_immediately() {
    check(&[
        ("cap.around { cap.get(\"a\") }", "1"),
        ("cap.around { cap.get(\"c\") }", "nil"),
        (
            "cap.around { 0 }\n[cap.get(\"a\"), cap.get(\"c\")]",
            "[1, 3]",
        ),
        ("cap.put(\"k\", 4)\ncap.get(\"k\")", "4"),
        ("cap.notes << 6\ncap.get(\"notes\")", "[0, 6]"),
    ]);
}

#[test]
fn script_writes_between_publications_are_kept() {
    check(&[
        (
            "cap.around { cap.notes << 2 }\n[cap.get(\"a\"), cap.notes, cap.get(\"c\")]",
            "[1, [0, 2], 3]",
        ),
        (
            "cap.inner.around { cap.inner.notes << 2 }\n[cap.inner.get(\"a\"), cap.inner.notes, cap.inner.get(\"c\")]",
            "[1, [0, 2], 3]",
        ),
    ]);
}

#[test]
fn copies_stay_independent_values() {
    check(&[
        (
            "c = cap\ncap.put(\"k\", 1)\n[c.get(\"k\"), cap.get(\"k\")]",
            "[nil, 1]",
        ),
        (
            "c = cap\nc.put(\"k\", 1)\n[c.get(\"k\"), cap.get(\"k\")]",
            "[nil, 1]",
        ),
        (
            "cap.put(\"list\", [1])\ns = cap.get(\"list\")\ncap.put(\"list\", [2])\n[s, cap.get(\"list\")]",
            "[[1], [2]]",
        ),
        (
            "cap.put(\"h\", {x: 1})\nh = cap.get(\"h\").as(hash<string, int>)\nh[\"x\"] = 2\n[h[\"x\"], cap.get(\"h\").as(hash<string, int>)[\"x\"]]",
            "[2, 1]",
        ),
    ]);
}

#[test]
fn stale_copies_only_change_their_own_receiver() {
    check(&[
        ("c = cap\ncap.notes << 1\nc.put(\"k\", 2)", "false"),
        (
            "c = cap\ncap.notes << 1\nc.put(\"k\", 2)\n[c.get(\"k\"), cap.get(\"k\")]",
            "[nil, nil]",
        ),
        (
            "c = cap\ncap.notes << 1\nc.put_then_get(\"k\", 2)",
            "[false, 2]",
        ),
        ("cap.put_then_get(\"k\", 2)", "[true, 2]"),
    ]);
}

/// A static program calls only the members its host declares, so a method
/// published during a call cannot be named.
#[test]
fn published_methods_are_not_declared_members() {
    let mut engine = vibescript::Engine::new();
    engine.declare_capability(&capability()).unwrap();
    let source = "def run -> any\ncap.install()\ncap.added()\nend";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0203"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("added").unwrap()
    );
}

#[test]
fn methods_without_a_member_receiver_cannot_publish() {
    let options = CallOptions {
        capabilities: vec![Capability::from_value("put", put("put").value())],
        ..CallOptions::default()
    };
    let error = run("put(\"k\", 1)", options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert!(error.message.contains("no member receiver"), "{error}");
}

#[test]
fn publications_end_with_the_invocation() {
    let binds = Arc::new(AtomicUsize::new(0));
    let counted = binds.clone();
    let options = CallOptions {
        capabilities: vec![Capability::new("cap", move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Value::object(methods("cap")))
        })],
        ..CallOptions::default()
    };
    let mut engine = vibescript::Engine::new();
    engine
        .declare_capability(&Capability::from_value(
            "cap",
            Value::object(methods("cap")),
        ))
        .unwrap();
    let script = engine
        .compile("def run(v: int) -> any\nbefore = cap.get(\"k\")\ncap.put(\"k\", v)\n[before, cap.get(\"k\")]\nend")
        .unwrap();
    let first = script
        .call("run", &[Value::int(1)], options.clone())
        .unwrap();
    let second = script.call("run", &[Value::int(2)], options).unwrap();
    assert_eq!(first.value.to_string(), "[nil, 1]");
    assert_eq!(second.value.to_string(), "[nil, 2]");
    assert_eq!(binds.load(Ordering::SeqCst), 2);
}

#[test]
fn host_retained_values_do_not_change_published_fields() {
    let retained = Arc::new(Mutex::new(None::<Value>));
    let kept = retained.clone();
    let publish = HostMethod::new_with_block("cap.publish", move |call, _, _| {
        let value = call.context().array(&[Value::int(1)])?;
        call.set_receiver_field(b"k", &value)?;
        *kept.lock().unwrap() = Some(value);
        Ok(Value::nil())
    });
    // The template declares `k`, so the script can update the field in place.
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"publish".to_vec(), publish.value()),
                (b"k".to_vec(), Value::array(vec![Value::int(0)])),
            ]),
        )],
        ..CallOptions::default()
    };
    let result = run("cap.publish()\ncap.k << 2\ncap.k", options).unwrap();
    assert_eq!(result.value.to_string(), "[1, 2]");
    assert_eq!(
        retained.lock().unwrap().as_ref().unwrap().to_string(),
        "[1]"
    );
}

#[test]
fn publication_charges_memory_and_respects_cancellation() {
    let big = HostMethod::new_with_block("cap.big", |call, _, _| {
        call.set_receiver_field(b"payload", &Value::bytes(vec![b'x'; 1 << 20]))?;
        Ok(Value::nil())
    });
    let options = |limits: Limits, cancellation: CancellationToken| {
        let big = big.clone();
        CallOptions {
            capabilities: vec![Capability::from_value(
                "cap",
                Value::object(vec![
                    (b"big".to_vec(), big.value()),
                    (b"payload".to_vec(), Value::bytes("")),
                ]),
            )],
            limits,
            cancellation,
            ..CallOptions::default()
        }
    };
    let tight = Limits {
        memory_bytes: Some(256 << 10),
        ..Limits::default()
    };
    let error = run(
        "cap.big()\ncap.payload.bytesize",
        options(tight, CancellationToken::new()),
    )
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    let roomy = run(
        "cap.big()\ncap.payload.bytesize",
        options(Limits::default(), CancellationToken::new()),
    )
    .unwrap();
    assert_eq!(roomy.value.as_int(), Some(1 << 20));
    assert!(roomy.stats.peak_memory_bytes >= 1 << 20);

    let cancelled = CancellationToken::new();
    let token = cancelled.clone();
    let publish = HostMethod::new_with_block("cap.cancel", move |call, _, _| {
        token.cancel();
        call.set_receiver_field(b"k", &Value::int(1))?;
        Ok(Value::nil())
    });
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"cancel".to_vec(), publish.value()),
                (b"k".to_vec(), Value::int(0)),
            ]),
        )],
        cancellation: cancelled,
        ..CallOptions::default()
    };
    let error = run("cap.cancel()\ncap.k", options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
}

#[test]
fn publications_before_a_host_error_remain() {
    let failing = HostMethod::new_with_block("cap.fail", |call, _, _| {
        call.set_receiver_field(b"k", &Value::int(1))?;
        Err(vibescript::Error::new(ErrorKind::Host, "boom"))
    });
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "cap",
            Value::object(vec![
                (b"fail".to_vec(), failing.value()),
                (b"k".to_vec(), Value::int(0)),
            ]),
        )],
        ..CallOptions::default()
    };
    let result = run("begin\ncap.fail()\nrescue => e\nnil\nend\ncap.k", options).unwrap();
    assert_eq!(result.value.as_int(), Some(1));
}

#[test]
fn publication_cannot_replace_a_method_field() {
    let error = run("cap.put(\"get\", 1)", options()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert!(
        error
            .message
            .contains("cannot replace capability method get"),
        "{error}"
    );
    check(&[("cap.put(\"k\", 1)\ncap.get(\"k\")", "1")]);
}

#[test]
fn the_shallowest_binding_that_holds_the_receiver_wins() {
    let options = CallOptions {
        capabilities: vec![
            Capability::from_value("other", Value::object(methods("other"))),
            capability(),
        ],
        ..CallOptions::default()
    };
    let result = run(
        "other.put(\"x\", cap)\ncap.put(\"k\", 1)\n[cap.get(\"k\"), other.get(\"x\").as(hash<string, any>)[\"k\"]]",
        options,
    )
    .unwrap();
    assert_eq!(result.value.to_string(), "[1, nil]");
}

#[test]
fn later_publications_in_a_call_land_at_the_same_binding_path() {
    check(&[(
        "cap.inner.around { cap.put(\"inner\", {x: 9}) }\ninner = cap.get(\"inner\").as(hash<string, any>)\n[inner[\"a\"], inner[\"c\"], inner[\"x\"]]",
        "[nil, 3, 9]",
    )]);
}
