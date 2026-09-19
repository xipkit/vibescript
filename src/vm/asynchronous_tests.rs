use crate::{CallOptions, Engine, ErrorKind, HostMethod, Value, asynchronous::Runner};
use std::{
    future::pending,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::Notify;

type Memory = Arc<Mutex<Weak<crate::budget::Memory>>>;

#[tokio::test]
async fn abandoned_async_invocations_release_unreachable_cycles() {
    for nested in [false, true] {
        let memory: Memory = Arc::new(Mutex::new(Weak::new()));
        let heap = Arc::new(Mutex::new(Weak::new()));
        let entered = Arc::new(Notify::new());
        let (saved_memory, saved_heap, ready) = (memory.clone(), heap.clone(), entered.clone());
        let mut engine = Engine::new();
        engine.register_method(
            "pause",
            HostMethod::new_async("pause", move |call, _, _| {
                let (saved_memory, saved_heap, ready) =
                    (saved_memory.clone(), saved_heap.clone(), ready.clone());
                Box::pin(async move {
                    let ctx = call.context()?;
                    *saved_memory.lock().unwrap() = Arc::downgrade(&ctx.identity());
                    *saved_heap.lock().unwrap() = Arc::downgrade(ctx.objects.as_ref().unwrap());
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
            "class Box;property link;end;def run;outer(){b=Box.new;b.link=b;pause(b)};end"
        } else {
            "class Box;property link;end;def run;b=Box.new;b.link=b;pause(b);end"
        };
        let script = engine.compile(source).unwrap();
        let runner = Runner::new(1).unwrap();
        let worker = runner.clone();
        let task = tokio::spawn(async move {
            worker
                .call(script, "run".into(), vec![], CallOptions::default())
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(heap.lock().unwrap().upgrade().is_none(), "nested={nested}");
        assert!(
            memory.lock().unwrap().upgrade().is_none(),
            "nested={nested}"
        );
        assert_eq!(runner.available_slots(), 1);
    }
}

#[tokio::test]
async fn async_panic_paths_release_cycles_and_accounting() {
    for source in [
        "class Box;property link;end;def run;b=Box.new;b.link=b;fail(b);end",
        "class Box;property link;end;def run;outer(){b=Box.new;b.link=b;fail(b)};end",
        "class Box;property link;end;def run;sync(){b=Box.new;b.link=b;fail(b)};end",
    ] {
        for construction in [false, true] {
            let memory: Memory = Arc::new(Mutex::new(Weak::new()));
            let observed = memory.clone();
            let mut engine = Engine::new();
            engine.register_method(
                "fail",
                HostMethod::new_async("fail", move |call, _, _| {
                    *observed.lock().unwrap() = Arc::downgrade(&call.context().unwrap().identity());
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
            assert!(memory.lock().unwrap().upgrade().is_none(), "{source}");
            assert_eq!(runner.available_slots(), 1);
        }
    }
}

#[tokio::test]
async fn objects_retained_by_async_hosts_outlive_cancelled_invocations() {
    let memory: Memory = Arc::new(Mutex::new(Weak::new()));
    let retained = Arc::new(Mutex::new(None));
    let entered = Arc::new(Notify::new());
    let (observed, saved, ready) = (memory.clone(), retained.clone(), entered.clone());
    let mut engine = Engine::new();
    engine.register_method(
        "hold",
        HostMethod::new_async("hold", move |call, args, _| {
            let (observed, saved, ready) = (observed.clone(), saved.clone(), ready.clone());
            Box::pin(async move {
                *observed.lock().unwrap() = Arc::downgrade(&call.context()?.identity());
                *saved.lock().unwrap() = Some(args[0].clone());
                ready.notify_one();
                pending::<crate::Result<Value>>().await
            })
        }),
    );
    let script=engine.compile("class Box;property n;property link;end;def run;b=Box.new;b.n=3;b.link=b;hold(b);b.n=4;end").unwrap();
    let runner = Runner::new(1).unwrap();
    let task = tokio::spawn(async move {
        runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(memory.lock().unwrap().upgrade().is_some());
    let value = retained.lock().unwrap().take().unwrap();
    let reader = Engine::new()
        .compile("def read(b);[b.n,b.link==b];end")
        .unwrap();
    let result = reader
        .call("read", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "[3, true]");
    drop(value);
    assert!(memory.lock().unwrap().upgrade().is_none());
}
