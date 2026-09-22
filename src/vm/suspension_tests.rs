use super::*;
use crate::{CallOptions, Capability, Engine, HostMethod, Outcome, Script};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

fn script(source: &str, calls: &Arc<AtomicUsize>) -> Script {
    let calls = calls.clone();
    let mut engine = Engine::new();
    engine.register_method(
        "pause",
        HostMethod::new_with_block("pause", move |call, args, _| {
            calls.fetch_add(1, Ordering::Relaxed);
            call.context().charge(3)?;
            if call.block_given() {
                call.call_block(args)
            } else {
                Ok(args.first().cloned().unwrap_or_else(Value::nil))
            }
        }),
    );
    engine.compile(source).unwrap()
}

fn migrated(script: &Script, options: CallOptions) -> (Result<Outcome>, usize) {
    let mut execution = match Execution::new(script, "run", &[], &[], options) {
        Ok(execution) => execution,
        Err(error) => return (Err(error), 0),
    };
    let mut pending = None;
    let mut pauses = 0;
    loop {
        let resume = move || {
            let step =
                execution
                    .run
                    .as_mut()
                    .unwrap()
                    .resume(&mut execution.context, None, pending);
            (execution, step)
        };
        // WASI has no threads, so each step resumes on the calling thread.
        let (next, step) = if cfg!(target_os = "wasi") {
            resume()
        } else {
            std::thread::spawn(resume).join().unwrap()
        };
        execution = next;
        match step {
            Ok(Step::Host) => {
                pauses += 1;
                pending = Some(execution.run.as_mut().unwrap().host(&mut execution.context));
            }
            Ok(Step::Complete(Exit::Value(value))) => {
                return (execution.finish(Ok(value)), pauses);
            }
            Ok(Step::Complete(Exit::Control(_))) => panic!("control escaped the invocation"),
            Err(error) => return (execution.finish(Err(error)), pauses),
        }
    }
}

#[test]
fn suspended_execution_moves_between_workers_without_changing_values_or_accounting() {
    for (source, expected, pauses) in [
        (
            "class Box;property n;end;def run;box=Box.new;box.n=1;box.n+=pause(3);[1,2].each{|n|box.n+=pause(n)};box.n;end",
            "7",
            3,
        ),
        (
            "def run;a=[1];a[-1]+=pause(){a.push(2);3};a;end",
            "[4, 2]",
            1,
        ),
        (
            "def run;n=1;begin;pause(){raise 'stop'};rescue;n+=2;ensure;n+=pause(4);end;n;end",
            "7",
            2,
        ),
        ("module M;N=pause(7);end;def run;M::N;end", "7", 1),
        ("def run;pause(){return 7};false;end", "7", 1),
        ("def run;pause(){break 7};end", "7", 1),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let script = script(source, &calls);
        let direct = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(direct.value.to_string(), expected, "{source}");
        calls.store(0, Ordering::Relaxed);
        let (result, observed) = migrated(&script, CallOptions::default());
        let result = result.unwrap();
        assert_eq!(result.value.to_string(), expected, "{source}");
        assert_eq!(observed, pauses, "{source}");
        assert_eq!(calls.load(Ordering::Relaxed), pauses, "{source}");
        assert_eq!(result.stats.steps, direct.stats.steps, "{source}");
        assert_eq!(
            result.stats.peak_memory_bytes, direct.stats.peak_memory_bytes,
            "{source}"
        );
        assert_eq!(
            result.stats.retained_memory_bytes, direct.stats.retained_memory_bytes,
            "{source}"
        );
    }
}

#[test]
fn cancellation_at_a_host_boundary_stops_before_the_callback() {
    let calls = Arc::new(AtomicUsize::new(0));
    let script = script("def run;pause(7);end", &calls);
    let mut execution = Execution::new(&script, "run", &[], &[], CallOptions::default()).unwrap();
    let memory = Arc::downgrade(&execution.context.identity());
    let run = execution.run.as_mut().unwrap();
    let ctx = &mut execution.context;
    assert!(matches!(run.resume(ctx, None, None).unwrap(), Step::Host));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    ctx.cancellation().cancel();
    let result = run.host(ctx);
    assert_eq!(result.err().unwrap().kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let error = run.resume(ctx, None, None).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    drop(execution.finish(Err(error)));
    assert!(memory.upgrade().is_none());
}

#[test]
fn suspended_execution_preserves_limits_and_error_locations() {
    for source in [
        "def run;a=[1];a[-1]+=pause(){a.push(2);3};a;end",
        "def run;begin;pause(){raise 'stop'};rescue;pause(7);end;end",
        "def run;pause(){break [1,2,3]};end",
    ] {
        let script = script(source, &Arc::new(AtomicUsize::new(0)));
        let baseline = script.call("run", &[], CallOptions::default()).unwrap();
        let mut exact = CallOptions::default();
        exact.limits.steps = Some(baseline.stats.steps);
        exact.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes);
        assert_eq!(
            migrated(&script, exact.clone())
                .0
                .unwrap()
                .value
                .to_string(),
            baseline.value.to_string(),
        );
        for kind in [ErrorKind::Steps, ErrorKind::Memory] {
            let mut short = exact.clone();
            if kind == ErrorKind::Steps {
                short.limits.steps = Some(baseline.stats.steps - 1);
            } else {
                short.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
            }
            let direct = script.call("run", &[], short.clone()).unwrap_err();
            assert_eq!(direct.kind, kind);
            assert_eq!(migrated(&script, short).0.unwrap_err(), direct, "{source}");
        }
    }
    let script = script(
        "def fail\n pause(){raise 'failure'}\nend\ndef run\n fail\nend",
        &Arc::new(AtomicUsize::new(0)),
    );
    let direct = script.call("run", &[], CallOptions::default()).unwrap_err();
    assert!(direct.diagnostic.is_some());
    assert_eq!(
        migrated(&script, CallOptions::default()).0.unwrap_err(),
        direct
    );
}

#[test]
fn abandoned_execution_releases_cycles_without_running_ensure() {
    for expired in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let script = script(
            "class Box;property link;end;def run;b=Box.new;b.link=b;begin;pause();ensure;pause();end;end",
            &calls,
        );
        let mut execution =
            Execution::new(&script, "run", &[], &[], CallOptions::default()).unwrap();
        assert!(matches!(
            execution
                .run
                .as_mut()
                .unwrap()
                .resume(&mut execution.context, None, None)
                .unwrap(),
            Step::Host
        ));
        let memory = Arc::downgrade(&execution.context.identity());
        let heap = Arc::downgrade(execution.context.objects.as_ref().unwrap());
        if expired {
            execution.context.options.deadline = Some(Instant::now());
            let error = execution
                .run
                .as_mut()
                .unwrap()
                .resume(&mut execution.context, None, None)
                .err()
                .unwrap();
            assert_eq!(error.kind, ErrorKind::Deadline);
        }
        drop(execution);
        assert!(heap.upgrade().is_none());
        assert!(memory.upgrade().is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn abandoned_execution_keeps_objects_retained_by_the_host() {
    let retained = Arc::new(Mutex::new(None));
    let held = retained.clone();
    let mut engine = Engine::new();
    engine.register("hold", move |_, args| {
        *held.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    engine.register_method(
        "pause",
        HostMethod::new_with_block("pause", |_, _, _| unreachable!()),
    );
    let script = engine.compile("class Box;property n;property link;end;def run;b=Box.new;b.n=3;b.link=b;hold(b);pause();b.n=4;end").unwrap();
    let mut execution = Execution::new(&script, "run", &[], &[], CallOptions::default()).unwrap();
    assert!(matches!(
        execution
            .run
            .as_mut()
            .unwrap()
            .resume(&mut execution.context, None, None)
            .unwrap(),
        Step::Host
    ));
    let memory = Arc::downgrade(&execution.context.identity());
    drop(execution);
    let value = retained.lock().unwrap().take().unwrap();
    assert!(memory.upgrade().is_some());
    let reader = Engine::new()
        .compile("def read(b);[b.n,b.link==b];end")
        .unwrap();
    let result = reader
        .call("read", std::slice::from_ref(&value), CallOptions::default())
        .unwrap();
    assert_eq!(result.value.to_string(), "[3, true]");
    drop(value);
    assert!(memory.upgrade().is_none());
}

#[test]
fn completed_host_results_can_outlive_an_abandoned_execution() {
    let script = script(
        "class Box;property link;end;def run;pause(){b=Box.new;b.link=b;b};end",
        &Arc::new(AtomicUsize::new(0)),
    );
    for result_first in [false, true] {
        let mut execution =
            Execution::new(&script, "run", &[], &[], CallOptions::default()).unwrap();
        assert!(matches!(
            execution
                .run
                .as_mut()
                .unwrap()
                .resume(&mut execution.context, None, None)
                .unwrap(),
            Step::Host
        ));
        let result = execution
            .run
            .as_mut()
            .unwrap()
            .host(&mut execution.context)
            .unwrap();
        let memory = Arc::downgrade(&execution.context.identity());
        if result_first {
            drop(result);
            drop(execution);
        } else {
            drop(execution);
            assert!(memory.upgrade().is_some());
            drop(result);
        }
        assert!(memory.upgrade().is_none());
    }
}

#[test]
#[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
fn host_panics_release_the_invocation_heap_and_accounting() {
    for framed in [false, true] {
        let memory = Arc::new(Mutex::new(Weak::new()));
        let observed = memory.clone();
        let callback = move |ctx: &mut CallContext| -> Result<Value> {
            *observed.lock().unwrap() = Arc::downgrade(&ctx.identity());
            assert!(ctx.objects.is_some());
            panic!("host callback panic");
        };
        let mut engine = Engine::new();
        if framed {
            engine.register_method(
                "fail",
                HostMethod::new_with_block("fail", move |call, _, _| callback(call.context())),
            );
        } else {
            engine.register("fail", move |ctx, _| callback(ctx));
        }
        let script = engine
            .compile("class Box;property link;end;def run;b=Box.new;b.link=b;fail();end")
            .unwrap();
        assert!(
            catch_unwind(AssertUnwindSafe(|| script.call(
                "run",
                &[],
                CallOptions::default()
            )))
            .is_err()
        );
        assert!(memory.lock().unwrap().upgrade().is_none());
    }
}

#[test]
#[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
fn failed_or_panicked_preparation_releases_imported_cycles() {
    for panicked in [false, true] {
        let foreign = Engine::new()
            .compile("class Box;property link;end;b=Box.new;b.link=b;b")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        let memory = Arc::new(Mutex::new(Weak::new()));
        let observed = memory.clone();
        let options = CallOptions {
            capabilities: vec![Capability::new("incoming", move |ctx| {
                let _value = ctx.import(&foreign)?;
                *observed.lock().unwrap() = Arc::downgrade(&ctx.identity());
                if panicked {
                    panic!("capability factory panic");
                }
                Err(Error::new(ErrorKind::Host, "capability factory error"))
            })],
            ..CallOptions::default()
        };
        let script = Engine::new().compile("def run;nil;end").unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| script.call("run", &[], options)));
        if panicked {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().kind, ErrorKind::Host);
        }
        assert!(memory.lock().unwrap().upgrade().is_none());
    }
}
