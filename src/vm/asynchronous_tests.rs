use crate::{
    CallOptions, ErrorKind, HostMethod, Value, actor::Actor, asynchronous::Runner,
    objects::HeapWatch,
};
use std::{future::pending, sync::Arc, time::Duration};
use tokio::sync::Notify;

type Memory = Arc<Actor<Option<Arc<crate::budget::Memory>>>>;

/// The strong count of an observed memory account: one once only the
/// observer holds it.
fn holders(memory: &Memory) -> Option<usize> {
    memory.read(|memory| memory.as_ref().map(Arc::strong_count))
}

#[tokio::test]
async fn abandoned_async_invocations_release_unreachable_cycles() {
    for nested in [false, true] {
        let memory: Memory = Arc::new(Actor::new(None));
        let heap = Arc::new(Actor::new(HeapWatch::default()));
        let entered = Arc::new(Notify::new());
        let (saved_memory, saved_heap, ready) = (memory.clone(), heap.clone(), entered.clone());
        let mut engine = crate::Engine::new();
        engine.register_method(
            "pause",
            HostMethod::new_async("pause", move |call, _, _| {
                let (saved_memory, saved_heap, ready) =
                    (saved_memory.clone(), saved_heap.clone(), ready.clone());
                Box::pin(async move {
                    let ctx = call.context()?;
                    saved_memory.with(|memory| *memory = Some(ctx.identity()));
                    saved_heap.with(|heap| *heap = crate::objects::watch_heap(ctx));
                    ready.notify_one();
                    pending::<crate::Result<Value>>().await
                })
            }),
        );
        engine.register_method(
            "outer",
            HostMethod::new_async("outer", |call, _, _| {
                Box::pin(async move { call.call_block(vec![]).await })
            }),
        );
        let source = if nested {
            "class Box;property link: Box?;end;def run;outer(){b=Box.new;b.link=b;pause(b)};end"
        } else {
            "class Box;property link: Box?;end;def run;b=Box.new;b.link=b;pause(b);end"
        };
        let script = engine.compile(source).unwrap();
        let runner = Runner::new(1).unwrap();
        // Dropping the call abandons it, as a host that gives up on it would.
        let mut call = Box::pin(runner.call(script, "run".into(), vec![], CallOptions::default()));
        tokio::select! {
            _ = &mut call => panic!("the paused call finished"),
            entered = tokio::time::timeout(Duration::from_secs(2), entered.notified()) => {
                entered.unwrap();
            }
        }
        drop(call);
        assert!(heap.read(HeapWatch::gone), "nested={nested}");
        assert_eq!(holders(&memory), Some(1), "nested={nested}");
        assert_eq!(runner.available_slots(), 1);
    }
}

#[tokio::test]
async fn async_panic_paths_release_cycles_and_accounting() {
    for source in [
        "class Box;property link: Box?;end;def run;b=Box.new;b.link=b;fail(b);end",
        "class Box;property link: Box?;end;def run;outer(){b=Box.new;b.link=b;fail(b)};end",
        "class Box;property link: Box?;end;def run;sync(){b=Box.new;b.link=b;fail(b)};end",
    ] {
        for construction in [false, true] {
            let memory: Memory = Arc::new(Actor::new(None));
            let observed = memory.clone();
            let mut engine = crate::Engine::new();
            engine.register_method(
                "fail",
                HostMethod::new_async("fail", move |call, _, _| {
                    observed.with(|memory| *memory = Some(call.context().unwrap().identity()));
                    if construction {
                        panic!("constructing a host future");
                    }
                    Box::pin(async {
                        tokio::task::yield_now().await;
                        panic!("polling a host future");
                    })
                }),
            );
            engine.register_method(
                "outer",
                HostMethod::new_async("outer", |call, _, _| {
                    Box::pin(async move { call.call_block(vec![]).await })
                }),
            );
            engine.register_method(
                "sync",
                HostMethod::new_with_block("sync", |call, _, _| call.call_block(&[])),
            );
            let runner = Runner::new(1).unwrap();
            let error = runner
                .call(
                    engine.compile(source).unwrap(),
                    "run".into(),
                    vec![],
                    CallOptions::default(),
                )
                .await
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Host);
            assert_eq!(holders(&memory), Some(1), "{source}");
            assert_eq!(runner.available_slots(), 1);
        }
    }
}

#[tokio::test]
async fn objects_retained_by_async_hosts_outlive_cancelled_invocations() {
    let memory: Memory = Arc::new(Actor::new(None));
    let retained = Arc::new(Actor::new(None));
    let entered = Arc::new(Notify::new());
    let (observed, saved, ready) = (memory.clone(), retained.clone(), entered.clone());
    let mut engine = crate::Engine::new();
    engine.register_method(
        "hold",
        HostMethod::new_async("hold", move |call, args, _| {
            let (observed, saved, ready) = (observed.clone(), saved.clone(), ready.clone());
            Box::pin(async move {
                let identity = call.context()?.identity();
                observed.with(|memory| *memory = Some(identity));
                drop(saved.with(|saved| saved.replace(args[0].clone())));
                ready.notify_one();
                pending::<crate::Result<Value>>().await
            })
        }),
    );
    let script=engine.compile("class Box;property n: int?;property link: Box?;end;def run;b=Box.new;b.n=3;b.link=b;hold(b);b.n=4;end\ndef read(b: Box) -> array<int | bool | nil>;[b.n,b.link==b];end").unwrap();
    let reader = script.clone();
    let runner = Runner::new(1).unwrap();
    let mut call = Box::pin(runner.call(script, "run".into(), vec![], CallOptions::default()));
    tokio::select! {
        _ = &mut call => panic!("the held call finished"),
        entered = tokio::time::timeout(Duration::from_secs(2), entered.notified()) => {
            entered.unwrap();
        }
    }
    drop(call);
    assert!(holders(&memory).is_some_and(|holders| holders > 1));
    let value = retained.with(Option::take).unwrap();
    // The instance returns to a later call of the script that declares its
    // class.
    let result = reader
        .call("read", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "[3, true]");
    drop(value);
    assert_eq!(holders(&memory), Some(1));
}

/// A wake that arrives while a host future blocks the worker thread on
/// another primitive must still end the bridge's wait: a channel receive
/// parks the same thread and takes the unpark token with it.
#[test]
fn a_wake_during_a_blocking_poll_still_resumes_the_synchronous_bridge() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let finished = runtime.block_on(async {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Actor::new(Some(released));
        let mut engine = crate::Engine::new();
        engine.register_method(
            "slow",
            HostMethod::new_async("slow", move |_, _, _| {
                let released = released.with(Option::take);
                Box::pin(async move {
                    // The timer's wake arrives while the receive blocks.
                    tokio::join!(tokio::time::sleep(Duration::from_millis(1)), async move {
                        if let Some(released) = released {
                            released.recv().unwrap();
                        }
                    });
                    Ok(Value::int(7))
                })
            }),
        );
        engine.register_method(
            "sync",
            HostMethod::new_with_block("sync", |call, _, _| call.call_block(&[])),
        );
        let script = engine.compile("def run -> any;sync(){slow()};end").unwrap();
        let runner = Runner::new(1).unwrap();
        let call = runner.call(script, "run".into(), vec![], CallOptions::default());
        tokio::time::timeout(Duration::from_secs(5), async {
            let (outcome, ()) = tokio::join!(call, async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                release.send(()).unwrap();
            });
            outcome.map(|outcome| outcome.value.as_int())
        })
        .await
    });
    // A bridge that lost the wake leaves its worker parked for good.
    runtime.shutdown_timeout(Duration::from_secs(1));
    assert_eq!(finished.expect("the call finished").unwrap(), Some(7));
}
