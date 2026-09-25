#![cfg(feature = "tokio")]

use std::{future::poll_fn, task::Poll};
use vibescript::{
    CallOptions, Capability, Engine, ErrorKind, HostMethod, Value, asynchronous::Runner,
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

/// Reads the receiver, waits, runs the block if given, waits, and reads again.
fn holder(name: &str) -> HostMethod {
    HostMethod::new_async(name, |call, _, _| {
        Box::pin(async move {
            let before = tag(call.receiver()?);
            tokio::task::yield_now().await;
            let inner = if call.block_given() {
                call.call_block(vec![]).await?
            } else {
                Value::nil()
            };
            tokio::task::yield_now().await;
            let after = tag(call.receiver()?);
            call.context()?.array(&[before, inner, after])
        })
    })
}

/// A synchronous block-capable reader, bridged under the async runner.
fn reader(name: &str) -> HostMethod {
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

/// The capability's value: data, methods and an inner object of both.
fn capability() -> Value {
    let hold = holder("cap.hold").value();
    let read = reader("cap.read").value();
    let inner = Value::object(vec![
        (b"tag".to_vec(), Value::int(2)),
        (b"hold".to_vec(), hold.clone()),
        (b"read".to_vec(), read.clone()),
    ]);
    Value::object(vec![
        (b"tag".to_vec(), Value::int(1)),
        (b"hold".to_vec(), hold),
        (b"read".to_vec(), read),
        (b"inner".to_vec(), inner),
    ])
}

fn options() -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::new("cap", |_| Ok(capability()))],
        ..CallOptions::default()
    }
}

/// An engine that declares the capability each call's factory builds.
fn declaring() -> Engine {
    let mut engine = Engine::new();
    engine
        .declare_capability(&Capability::from_value("cap", capability()))
        .unwrap();
    engine
}

/// An engine without static types, for call forms they refuse or cannot
/// type: indexing the capability or writing its data, `send`, and methods
/// of an inner object, which the declaration types as plain data.
fn untyped() -> Engine {
    let mut engine = Engine::new();
    engine.set_static_types(false);
    engine
}

async fn run(runner: &Runner, engine: &Engine, body: &str, options: CallOptions) -> String {
    let script = engine
        .compile(&format!("def run -> any\n{body}\nend"))
        .unwrap();
    runner
        .call(script, "run".into(), vec![], options)
        .await
        .unwrap_or_else(|error| panic!("{body}: {error}"))
        .value
        .to_string()
}

#[tokio::test]
async fn async_and_bridged_sync_methods_hold_their_receiver_across_waits_and_blocks() {
    let runner = Runner::new(1).unwrap();
    let engine = declaring();
    for (body, expected) in [
        ("cap.hold()", "[1, nil, 1]"),
        ("cap.hold { 5 }", "[1, 5, 1]"),
        ("cap.read()", "[1, nil, 1]"),
    ] {
        assert_eq!(
            run(&runner, &engine, body, options()).await,
            expected,
            "{body}"
        );
    }
    let engine = untyped();
    for (body, expected) in [
        ("cap[:hold]()", "[1, nil, 1]"),
        ("cap::hold()", "[1, nil, 1]"),
        ("cap.send(:hold)", "[1, nil, 1]"),
        ("cap.inner.hold()", "[2, nil, 2]"),
        ("cap.hold { cap.inner.hold { 0 } }", "[1, [2, 0, 2], 1]"),
        ("cap.inner.read { cap.hold { 0 } }", "[2, [1, 0, 1], 2]"),
        (
            "cap.hold { cap.inner.read { cap.hold { 0 } } }",
            "[1, [2, [1, 0, 1], 2], 1]",
        ),
        ("cap.hold { cap[:tag] = 9 }", "[1, 9, 1]"),
        ("cap[:tag] = 7; cap.hold()", "[7, nil, 7]"),
        (
            "[cap.hold(0.tap { cap[:tag] = 9 }), cap[:tag]]",
            "[[1, nil, 1], 9]",
        ),
        (
            "[cap[:hold](*[0].map { |x| cap[:tag] = 9; x }), cap[:tag]]",
            "[[1, nil, 1], 9]",
        ),
    ] {
        assert_eq!(
            run(&runner, &engine, body, options()).await,
            expected,
            "{body}"
        );
    }
}

#[tokio::test]
async fn bare_async_methods_have_no_receiver() {
    let runner = Runner::new(1).unwrap();
    let mut engine = Engine::new();
    engine.register_method("bare", holder("bare"));
    assert_eq!(
        run(&runner, &engine, "bare()", CallOptions::default()).await,
        "[none, nil, none]"
    );
}

#[tokio::test]
async fn receiver_reads_fail_once_an_abandoned_block_retires_the_call() {
    let method = HostMethod::new_async("cap.abandon", |call, _, _| {
        Box::pin(async move {
            assert!(call.receiver()?.is_some());
            {
                let future = call.call_block(vec![]);
                tokio::pin!(future);
                poll_fn(|cx| {
                    assert!(future.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            assert_eq!(call.receiver().unwrap_err().kind, ErrorKind::Cancelled);
            Ok(Value::int(999))
        })
    });
    let template = Capability::from_value(
        "cap",
        Value::object(vec![
            (b"tag".to_vec(), Value::int(1)),
            (b"abandon".to_vec(), method.value()),
        ]),
    );
    let options = CallOptions {
        capabilities: vec![Capability::new("cap", move |_| {
            Ok(Value::object(vec![
                (b"tag".to_vec(), Value::int(1)),
                (b"abandon".to_vec(), method.value()),
            ]))
        })],
        ..CallOptions::default()
    };
    let mut options = options;
    options.limits.steps = None;
    let runner = Runner::new(1).unwrap();
    let mut engine = Engine::new();
    engine.declare_capability(&template).unwrap();
    let script = engine
        .compile("def run; cap.abandon { while true; 1; end }; end")
        .unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runner.call(script, "run".into(), vec![], options),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(runner.available_slots(), 1);
}

#[tokio::test]
async fn async_and_bridged_sync_methods_publish_across_waits_and_blocks() {
    let publish = HostMethod::new_async("cap.publish", |call, _, _| {
        Box::pin(async move {
            call.set_receiver_field(b"a", &Value::int(1))?;
            tokio::task::yield_now().await;
            let inner = if call.block_given() {
                call.call_block(vec![]).await?
            } else {
                Value::nil()
            };
            tokio::task::yield_now().await;
            call.set_receiver_field(b"c", &Value::int(3))?;
            Ok(inner)
        })
    });
    let bridged = HostMethod::new_with_block("cap.bridged", |call, _, _| {
        call.set_receiver_field(b"d", &Value::int(4))?;
        Ok(Value::nil())
    });
    let options = CallOptions {
        capabilities: vec![Capability::new("cap", move |_| {
            Ok(Value::object(vec![
                (b"publish".to_vec(), publish.value()),
                (b"bridged".to_vec(), bridged.value()),
            ]))
        })],
        ..CallOptions::default()
    };
    let runner = Runner::new(1).unwrap();
    // The script reads fields the methods publish, which the capability
    // does not declare, so it compiles without static types.
    let engine = untyped();
    for (body, expected) in [
        ("cap.publish()\n[cap[:a], cap[:c]]", "[1, 3]"),
        ("cap.publish { cap[:a] }", "1"),
        (
            "cap.publish { cap[:b] = 2 }\n[cap[:a], cap[:b], cap[:c]]",
            "[1, 2, 3]",
        ),
        ("c = cap\ncap.publish()\n[c[:a], cap[:a]]", "[nil, 1]"),
        ("cap.bridged()\ncap[:d]", "4"),
    ] {
        assert_eq!(
            run(&runner, &engine, body, options.clone()).await,
            expected,
            "{body}"
        );
    }
}
