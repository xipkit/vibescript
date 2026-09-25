#![cfg(feature = "tokio")]

mod common;

use std::{
    future::{pending, poll_fn},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use vibescript::{
    CallOptions, Capability, Engine, ErrorKind, HostMethod, Signature, SignatureParam, Value,
    asynchronous::Runner,
};

fn single_worker(test: impl Future<Output = ()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_time()
        .build()
        .unwrap();
    let result = runtime.block_on(tokio_timeout(test));
    runtime.shutdown_timeout(Duration::from_secs(1));
    result.unwrap();
}

async fn tokio_timeout<F: Future>(future: F) -> Result<F::Output, tokio::time::error::Elapsed> {
    tokio::time::timeout(Duration::from_secs(5), future).await
}

fn methods() -> Engine {
    let mut engine = Engine::new();
    engine.register_method(
        "sync",
        HostMethod::new_with_block("sync", |call, args, _| call.call_block(args)),
    );
    engine.register_method(
        "later",
        HostMethod::new_async("later", |call, args, _| {
            Box::pin(async move {
                tokio::task::yield_now().await;
                call.context()?.charge(3)?;
                if call.block_given() {
                    call.call_block(args.to_vec()).await
                } else {
                    Ok(args.first().cloned().unwrap_or_else(Value::nil))
                }
            })
        }),
    );
    engine
}

#[test]
fn native_waits_release_the_only_worker_for_another_call() {
    single_worker(async {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let mut engine = Engine::new();
        let (ready, gate) = (entered.clone(), release.clone());
        engine.register_method(
            "wait",
            HostMethod::new_async("wait", move |call, args, _| {
                let (ready, gate) = (ready.clone(), gate.clone());
                Box::pin(async move {
                    ready.notify_one();
                    gate.notified().await;
                    call.context()?.array(args)
                })
            }),
        );
        let script = engine.compile("def run;wait(7);end").unwrap();
        let runner = Runner::new(1).unwrap();
        let background = runner.clone();
        let first = tokio::spawn(async move {
            background
                .call(script, "run".into(), vec![], CallOptions::default())
                .await
        });
        entered.notified().await;
        assert_eq!(runner.available_slots(), 1);
        let fast = Engine::new().compile("def run;42;end").unwrap();
        assert_eq!(
            runner
                .call(fast, "run".into(), vec![], CallOptions::default())
                .await
                .unwrap()
                .value
                .as_int(),
            Some(42)
        );
        release.notify_one();
        assert_eq!(first.await.unwrap().unwrap().value.to_string(), "[7]");
        assert_eq!(runner.available_slots(), 1);
    });
}

#[test]
fn mixed_blocks_preserve_mutations_handlers_initialization_and_control() {
    single_worker(async {
        let mut engine = methods();
        engine.register_method(
            "swallow",
            HostMethod::new_with_block("swallow", |call, _, _| {
                assert_eq!(
                    call.call_block(&[]).unwrap_err().kind,
                    ErrorKind::ControlFlow
                );
                assert_eq!(
                    call.call_block(&[]).unwrap_err().kind,
                    ErrorKind::ControlFlow
                );
                Ok(Value::int(999))
            }),
        );
        engine.register_method(
            "swallow_async",
            HostMethod::new_async("swallow_async", |call, _, _| {
                Box::pin(async move {
                    assert_eq!(
                        call.call_block(vec![]).await.unwrap_err().kind,
                        ErrorKind::ControlFlow
                    );
                    tokio::task::yield_now().await;
                    assert_eq!(
                        call.call_block(vec![]).await.unwrap_err().kind,
                        ErrorKind::ControlFlow
                    );
                    Ok(Value::int(999))
                })
            }),
        );
        let runner = Runner::new(1).unwrap();
        for (source, expected) in [
            ("def run;sync(){later(){sync(){later(7)}}};end", "7"),
            ("def run;a=[1];a[-1]+=later(){a.push(2);3};a;end", "[4, 2]"),
            ("module M;N=later(7);end;def run;M::N;end", "7"),
            (
                "def run;n=1;begin;later(){sync(){raise 'stop'}};rescue;n+=2;ensure;n+=later(4);end;n;end",
                "7",
            ),
            ("def run;swallow(){later();return 7};false;end", "7"),
            ("def run;swallow(){later();break 7};end", "7"),
            (
                "def run;swallow_async(){sync(){later();return 7}};false;end",
                "7",
            ),
            ("def run;swallow_async(){later();break 7};end", "7"),
        ] {
            let result = runner
                .call(
                    engine.compile(source).unwrap(),
                    "run".into(),
                    vec![],
                    CallOptions::default(),
                )
                .await
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(result.value.to_string(), expected, "{source}");
            drop(result);
            assert_eq!(runner.available_slots(), 1);
        }
    });
}

#[test]
fn synchronous_bridges_do_not_deadlock_queued_calls_in_a_single_thread_pool() {
    single_worker(async {
        let mut engine = methods();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (ready, gate) = (entered.clone(), release.clone());
        engine.register_method(
            "wait",
            HostMethod::new_async("wait", move |_, _, _| {
                let (ready, gate) = (ready.clone(), gate.clone());
                Box::pin(async move {
                    ready.notify_one();
                    gate.notified().await;
                    Ok(Value::int(7))
                })
            }),
        );
        let script = engine.compile("def run;sync(){wait()};end").unwrap();
        let runner = Runner::new(1).unwrap();
        let background = runner.clone();
        let first = tokio::spawn(async move {
            background
                .call(script, "run".into(), vec![], CallOptions::default())
                .await
        });
        entered.notified().await;
        assert_eq!(runner.available_slots(), 0);
        let background = runner.clone();
        let second = tokio::spawn(async move {
            let script = Engine::new().compile("def run;9;end").unwrap();
            background
                .call(script, "run".into(), vec![], CallOptions::default())
                .await
        });
        tokio::task::yield_now().await;
        release.notify_one();
        assert_eq!(first.await.unwrap().unwrap().value.as_int(), Some(7));
        assert_eq!(second.await.unwrap().unwrap().value.as_int(), Some(9));
        assert_eq!(runner.available_slots(), 1);
    });
}

#[tokio::test]
async fn async_capabilities_keep_contracts_grants_and_attachment_rules() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let method = HostMethod::new_async("typed.echo", move |call, args, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            tokio::task::yield_now().await;
            call.context()?.checkpoint()?;
            Ok(args[0].clone())
        })
    })
    .with_signature(Signature {
        params: vec![SignatureParam {
            name: "input".into(),
            ty: "int".into(),
            optional: false,
        }],
        result: "int".into(),
        accepts_block: false,
    })
    .unwrap();
    let options = || CallOptions {
        capabilities: vec![Capability::new("typed", {
            let method = method.clone();
            move |_| Ok(Value::object(vec![(b"echo".to_vec(), method.value())]))
        })],
        ..CallOptions::default()
    };
    let runner = Runner::new(1).unwrap();
    for expression in [
        "typed.echo(7)",
        "typed::echo(7)",
        "typed[:echo](7)",
        "typed.send(:echo,7)",
        "typed.public_send(:echo,7)",
        "typed&.echo(7)",
        "typed.dup.echo(7)",
    ] {
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        let script = engine
            .compile(&format!("def run;{expression};end"))
            .unwrap();
        let value = runner
            .call(script, "run".into(), vec![], options())
            .await
            .unwrap();
        assert_eq!(value.value.as_int(), Some(7), "{expression}");
        assert_eq!(value.stats.retained_memory_bytes, 0);
    }
    assert_eq!(calls.load(Ordering::Relaxed), 7);
    for expression in ["typed.echo('bad')", "typed[:echo]", "f=typed::echo;f(7)"] {
        let script = Engine::new()
            .compile(&format!("def run;{expression};end"))
            .unwrap();
        assert!(
            runner
                .call(script, "run".into(), vec![], options())
                .await
                .is_err(),
            "{expression}"
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 7);
    let script = Engine::new()
        .compile("def run;begin;typed.echo(7);rescue;42;end;end")
        .unwrap();
    assert_eq!(
        script.call("run", &[], options()).unwrap().value.as_int(),
        Some(42)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 7);
}

#[tokio::test]
async fn cancellation_deadlines_and_ignored_quotas_interrupt_pending_host_futures() {
    for kind in [
        ErrorKind::Cancelled,
        ErrorKind::Deadline,
        ErrorKind::Steps,
        ErrorKind::Memory,
    ] {
        let entered = Arc::new(Notify::new());
        let ready = entered.clone();
        let mut engine = Engine::new();
        engine.register_method(
            "wait",
            HostMethod::new_async("wait", move |call, _, _| {
                let ready = ready.clone();
                Box::pin(async move {
                    ready.notify_one();
                    match kind {
                        ErrorKind::Steps => {
                            let _ = call.context()?.charge(u64::MAX);
                        }
                        ErrorKind::Memory => {
                            let _ = call.context()?.bytes(&vec![0; 65_536]);
                        }
                        _ => (),
                    }
                    pending::<vibescript::Result<Value>>().await
                })
            }),
        );
        let script = engine
            .compile("def run;begin;wait();rescue;999;end;end")
            .unwrap();
        let mut options = CallOptions::default();
        if kind == ErrorKind::Memory {
            options.limits.memory_bytes = Some(65_536);
        }
        if kind == ErrorKind::Deadline {
            options.deadline = Some(Instant::now() + Duration::from_millis(100));
        }
        let cancellation = options.cancellation.clone();
        let runner = Runner::new(1).unwrap();
        let task_runner = runner.clone();
        let task = tokio::spawn(async move {
            task_runner
                .call(script, "run".into(), vec![], options)
                .await
        });
        tokio_timeout(entered.notified()).await.unwrap();
        if kind == ErrorKind::Cancelled {
            cancellation.cancel();
        }
        let error = tokio_timeout(task).await.unwrap().unwrap().unwrap_err();
        assert_eq!(error.kind, kind, "{error}");
        assert!(error.diagnostic.is_some());
        assert_eq!(runner.available_slots(), 1);
    }
}

#[tokio::test]
async fn unpolled_block_futures_are_inert_and_abandoned_polled_blocks_retire_the_call() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = methods();
    engine.register("touch", move |_, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(7))
    });
    engine.register_method(
        "unused",
        HostMethod::new_async("unused", |call, _, _| {
            Box::pin(async move {
                drop(call.call_block(vec![]));
                call.context()?.checkpoint()?;
                call.call_block(vec![]).await
            })
        }),
    );
    let runner = Runner::new(1).unwrap();
    let script = engine.compile("def run;unused(){touch()};end").unwrap();
    assert_eq!(
        runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    engine.register_method(
        "abandon",
        HostMethod::new_async("abandon", |call, _, _| {
            Box::pin(async move {
                {
                    let future = call.call_block(vec![]);
                    tokio::pin!(future);
                    poll_fn(|cx| {
                        assert!(future.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                }
                assert_eq!(call.context().err().unwrap().kind, ErrorKind::Cancelled);
                assert_eq!(
                    call.call_block(vec![]).await.unwrap_err().kind,
                    ErrorKind::Cancelled
                );
                Ok(Value::int(999))
            })
        }),
    );
    let script = engine
        .compile("def run;abandon(){while true;1;end};end")
        .unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = None;
    let result = tokio_timeout(runner.call(script, "run".into(), vec![], options))
        .await
        .unwrap();
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(runner.available_slots(), 1);
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn dropping_calls_and_async_panics_release_the_invocation() {
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let (ready, observed) = (entered.clone(), dropped.clone());
    let mut engine = methods();
    engine.register_method(
        "wait",
        HostMethod::new_async("wait", move |_, _, _| {
            let (ready, observed) = (ready.clone(), observed.clone());
            Box::pin(async move {
                let _drop = Dropped(observed);
                ready.notify_one();
                pending::<vibescript::Result<Value>>().await
            })
        }),
    );
    let runner = Runner::new(1).unwrap();
    let task_runner = runner.clone();
    let script = engine.compile("def run;wait();end").unwrap();
    let task = tokio::spawn(async move {
        task_runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
    });
    entered.notified().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(runner.available_slots(), 1);
    engine.register_method(
        "panic",
        HostMethod::new_async("panic", |_, _, _| {
            Box::pin(async {
                tokio::task::yield_now().await;
                panic!("async host panic");
            })
        }),
    );
    for source in [
        "def run;panic();end",
        "def run;sync(){panic()};end",
        "def run;later(){sync(){panic()}};end",
    ] {
        let result = runner
            .call(
                engine.compile(source).unwrap(),
                "run".into(),
                vec![],
                CallOptions::default(),
            )
            .await;
        assert_eq!(result.unwrap_err().kind, ErrorKind::Host, "{source}");
        assert_eq!(runner.available_slots(), 1);
    }
}

#[test]
fn native_and_mixed_recursion_reach_the_default_limit() {
    single_worker(async {
        let engine = methods();
        let runner = Runner::new(1).unwrap();
        for body in [
            "later(){recurse()}",
            "sync(){recurse()}",
            "later(){sync(){recurse()}}",
        ] {
            let script = engine
                .compile(&format!("def recurse;{body};end;def run;recurse();end"))
                .unwrap();
            let error = runner
                .call(script, "run".into(), vec![], CallOptions::default())
                .await
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion, "{body}: {error}");
            assert_eq!(runner.available_slots(), 1);
        }
    });
}

#[tokio::test]
async fn async_storage_has_exact_limits_and_releases_ephemeral_charges() {
    let engine = methods();
    let runner = Runner::new(1).unwrap();
    for source in [
        "def run;later(7);end",
        "def run;later(){sync(){later(7)}};end",
        "def run;a=[1];a[-1]+=later(){a.push(2);3};a;end",
        "def run;begin;later(){raise 'ordinary'};rescue;later(7);end;end",
    ] {
        let script = engine.compile(source).unwrap();
        let baseline = runner
            .call(script.clone(), "run".into(), vec![], CallOptions::default())
            .await
            .unwrap();
        let mut exact = CallOptions::default();
        exact.limits.steps = Some(baseline.stats.steps);
        exact.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes);
        let repeated = runner
            .call(script.clone(), "run".into(), vec![], exact.clone())
            .await
            .unwrap();
        assert_eq!(
            repeated.value.to_string(),
            baseline.value.to_string(),
            "{source}"
        );
        assert_eq!(
            repeated.stats.peak_memory_bytes, baseline.stats.peak_memory_bytes,
            "{source}"
        );
        assert_eq!(
            repeated.stats.retained_memory_bytes, baseline.stats.retained_memory_bytes,
            "{source}"
        );
        if baseline.value.as_int().is_some() {
            assert_eq!(baseline.stats.retained_memory_bytes, 0);
        }
        for kind in [ErrorKind::Steps, ErrorKind::Memory] {
            let mut short = exact.clone();
            if kind == ErrorKind::Steps {
                short.limits.steps = Some(baseline.stats.steps - 1);
            } else {
                short.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
            }
            let error = runner
                .call(script.clone(), "run".into(), vec![], short)
                .await
                .unwrap_err();
            assert_eq!(error.kind, kind, "{source}: {error}");
        }
    }
}

#[test]
fn checking_async_signatures_never_constructs_or_polls_host_futures() {
    let called = Arc::new(AtomicUsize::new(0));
    let observed = called.clone();
    let mut engine = common::gradual_engine();
    engine.register_method(
        "later",
        HostMethod::new_async("later", move |_, _, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { panic!("checker polled host future") })
        })
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "n".into(),
                ty: "int".into(),
                optional: false,
            }],
            result: "int".into(),
            accepts_block: false,
        })
        .unwrap(),
    );
    for (source, clean) in [
        ("def run;later(7);end", true),
        ("def run;later('bad');end", false),
        ("def run -> string;later(7);end", false),
    ] {
        let script = engine.compile(source).unwrap();
        let checked = script.check(&CallOptions::default()).unwrap();
        assert_eq!(checked.is_clean(), clean, "{source}: {checked:?}");
    }
    assert_eq!(called.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn worker_panics_unwind_async_parent_callbacks_without_resuming_them() {
    let continued = Arc::new(AtomicBool::new(false));
    let observed = continued.clone();
    let mut engine = methods();
    engine.register("panic", |_, _| {
        panic!("plain host panic inside async block")
    });
    engine.register_method(
        "parent",
        HostMethod::new_async("parent", move |call, _, _| {
            let observed = observed.clone();
            Box::pin(async move {
                let _ = call.call_block(vec![]).await;
                observed.store(true, Ordering::SeqCst);
                Ok(Value::int(999))
            })
        }),
    );
    let runner = Runner::new(1).unwrap();
    let script = engine.compile("def run;parent(){panic()};end").unwrap();
    assert_eq!(
        runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Host
    );
    assert!(!continued.load(Ordering::SeqCst));
    assert_eq!(runner.available_slots(), 1);
}

#[tokio::test]
async fn abandoning_a_block_queued_for_a_busy_worker_preserves_the_other_call() {
    let entered = Arc::new(Notify::new());
    let start = Arc::new(Notify::new());
    let busy = Arc::new(Notify::new());
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let mut engine = Engine::new();
    let (ready, begin) = (entered.clone(), start.clone());
    engine.register_method(
        "abandon",
        HostMethod::new_async("abandon", move |call, _, _| {
            let (ready, begin) = (ready.clone(), begin.clone());
            Box::pin(async move {
                ready.notify_one();
                begin.notified().await;
                {
                    let future = call.call_block(vec![]);
                    tokio::pin!(future);
                    poll_fn(|cx| {
                        assert!(future.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                }
                Ok(Value::int(999))
            })
        }),
    );
    let (busy_signal, host_gate) = (busy.clone(), gate.clone());
    engine.register("busy", move |_, _| {
        busy_signal.notify_one();
        let (lock, changed) = &*host_gate;
        let _result = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(3), |released| {
                !*released
            })
            .unwrap();
        Ok(Value::int(7))
    });
    let first = engine
        .compile("def run;abandon(){raise 'block must not run'};end")
        .unwrap();
    let second = engine.compile("def run;busy();end").unwrap();
    let runner = Runner::new(1).unwrap();
    let first_runner = runner.clone();
    let first_task = tokio::spawn(async move {
        first_runner
            .call(first, "run".into(), vec![], CallOptions::default())
            .await
    });
    tokio_timeout(entered.notified()).await.unwrap();
    let second_runner = runner.clone();
    let second_task = tokio::spawn(async move {
        second_runner
            .call(second, "run".into(), vec![], CallOptions::default())
            .await
    });
    tokio_timeout(busy.notified()).await.unwrap();
    start.notify_one();
    let first_result = tokio_timeout(first_task).await.unwrap().unwrap();
    assert_eq!(first_result.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(runner.available_slots(), 0);
    let (lock, changed) = &*gate;
    *lock.lock().unwrap() = true;
    changed.notify_one();
    assert_eq!(
        tokio_timeout(second_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    assert_eq!(runner.available_slots(), 1);
}

#[tokio::test]
async fn async_keywords_result_contracts_and_constructor_calls_preserve_boundaries() {
    let mut engine = methods();
    engine.register_method(
        "keywords",
        HostMethod::new_async("keywords", |call, args, keywords| {
            Box::pin(async move {
                tokio::task::yield_now().await;
                call.context()?
                    .array(&[args[0].clone(), keywords[0].1.clone()])
            })
        }),
    );
    engine.register_method(
        "typed",
        HostMethod::new_async("typed", |call, _, _| {
            Box::pin(async move { call.call_block(vec![]).await })
        })
        .with_signature(Signature {
            params: vec![],
            result: "int".into(),
            accepts_block: true,
        })
        .unwrap(),
    );
    let runner = Runner::new(1).unwrap();
    for (source, expected) in [
        ("def run;keywords(4,tag:5);end", "[4, 5]"),
        ("def run;typed(){break 7};end", "7"),
        ("def run -> string;typed(){return 'outer'};end", "outer"),
        (
            "class Box;property n;def initialize;@n=later(7);end;end;def run;later(){Box.new.n};end",
            "7",
        ),
    ] {
        let result = runner
            .call(
                engine.compile(source).unwrap(),
                "run".into(),
                vec![],
                CallOptions::default(),
            )
            .await
            .unwrap_or_else(|e| panic!("{source}: {e}"));
        assert_eq!(result.value.to_string(), expected, "{source}");
    }
    let script = engine.compile("def run;typed(){break 'bad'};end").unwrap();
    assert_eq!(
        runner
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
}
