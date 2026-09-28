#![cfg(feature = "tokio")]
use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};
use vibescript::{CallOptions, Engine, Limits, Value, asynchronous::Runner};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_call_cancels_cpu_worker() {
    let runner = Runner::new(1).unwrap();
    let script = Engine::new()
        .compile("def run()\n while true\n  1\n end\nend")
        .unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: None,
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let result = tokio::time::timeout(
        Duration::from_millis(20),
        runner.call(script, "run".into(), vec![], options),
    )
    .await;
    assert!(result.is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while runner.available_slots() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_future_keeps_permit_until_host_returns() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = gate.clone();
    let (entered, mut ready) = tokio::sync::mpsc::channel(1);
    let mut engine = Engine::new();
    engine.register("host", move |_, _| {
        entered.blocking_send(()).unwrap();
        let (lock, cond) = &*worker_gate;
        let released = lock.lock().unwrap();
        let _released = cond
            .wait_timeout_while(released, Duration::from_secs(3), |v| !*v)
            .unwrap();
        Ok(Value::int(1))
    });
    let script = engine.compile("def run()\n host()\nend").unwrap();
    let runner = Runner::new(1).unwrap();
    let task_runner = runner.clone();
    let task = tokio::spawn(async move {
        task_runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), ready.recv())
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(runner.available_slots(), 0);
    let (lock, cond) = &*gate;
    *lock.lock().unwrap() = true;
    cond.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runner.available_slots() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn async_success_and_cancelled_queue() {
    let runner = Runner::new(1).unwrap();
    let script = Engine::new()
        .compile("def run(a: int) -> int\n a+1\nend")
        .unwrap();
    assert_eq!(
        runner
            .call(
                script.clone(),
                "run".into(),
                vec![Value::int(41)],
                CallOptions::default()
            )
            .await
            .unwrap()
            .value
            .as_int(),
        Some(42)
    );
    let options = CallOptions::default();
    options.cancellation.cancel();
    assert_eq!(
        runner
            .call(script, "run".into(), vec![Value::int(41)], options)
            .await
            .unwrap_err()
            .kind,
        vibescript::ErrorKind::Cancelled
    );
}

#[tokio::test]
async fn async_keyword_calls_use_defaults_and_release_the_worker() {
    let runner = Runner::new(1).unwrap();
    let script = Engine::new()
        .compile("def run(*, a: int, b: int = a+1) -> int\na+b\nend")
        .unwrap();
    let result = runner
        .call_with_keywords(
            script,
            "run".into(),
            vec![],
            vec![("a".into(), Value::int(20))],
            CallOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.value.as_int(), Some(41));
    assert_eq!(runner.available_slots(), 1);
}
