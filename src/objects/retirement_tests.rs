use super::*;
use crate::{CallOptions, CancellationToken, Engine, HostMethod};

struct RetiredCode {
    heap: Arc<Mutex<Weak<Heap>>>,
    observed: Arc<AtomicUsize>,
    cancellation: Option<CancellationToken>,
}

impl Drop for RetiredCode {
    fn drop(&mut self) {
        let heap = self.heap.lock().unwrap().upgrade();
        let Some(heap) = heap else {
            self.observed.store(3, Ordering::SeqCst);
            return;
        };
        let unlocked = heap.data.try_lock().is_ok();
        self.observed
            .store(if unlocked { 2 } else { 1 }, Ordering::SeqCst);
        if unlocked {
            let data = heap.data.lock().unwrap();
            assert!(!data.entries.data.is_empty());
            // Host-held snapshots can add live instances and environments.
            // Callback retirement must still happen after sweeping dead entries.
            assert!(
                data.entries
                    .data
                    .iter()
                    .all(|entry| { entry.internal.identity.marked.load(Ordering::Relaxed) })
            );
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
}

#[test]
fn imported_host_callbacks_retire_after_object_collection_unlocks() {
    for captured in [false, true] {
        for ending in [
            "keep",
            "hold(keep); 1/0",
            "hold(keep); stop()",
            "hold(keep); keep",
        ] {
            let heap = Arc::new(Mutex::new(Weak::new()));
            let observed = Arc::new(AtomicUsize::new(0));
            let cancellation = CancellationToken::new();
            let cancel_on_drop = ending == "hold(keep); keep";
            let retired = RetiredCode {
                heap: heap.clone(),
                observed: observed.clone(),
                cancellation: cancel_on_drop.then(|| cancellation.clone()),
            };
            let namespace = {
                let mut producer = Engine::new();
                producer.register("host", move |_, _| {
                    let _ = &retired;
                    Ok(Value::nil())
                });
                producer
                    .compile("module Foreign\n def self.value; host(); end\nend\nForeign")
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap()
                    .value
            };
            let incoming = Mutex::new(Some(namespace));
            let retained = Arc::new(Mutex::new(None));
            let held = retained.clone();
            let mut receiver = Engine::new();
            receiver.register("take_foreign", move |ctx, _| {
                *heap.lock().unwrap() = Arc::downgrade(ctx.objects.as_ref().unwrap());
                let value = incoming.lock().unwrap().take().unwrap();
                if !captured {
                    return Ok(value);
                }
                let Kind::Namespace(namespace) = &value.0 else {
                    unreachable!();
                };
                let namespace = Namespace::import(ctx, namespace)?;
                let environment = new(ctx, &namespace)?;
                let namespace = Namespace::with_environment(ctx, &namespace, environment.clone())?;
                let value = Value(Kind::Namespace(namespace));
                set(ctx, &environment, "cycle", &value)?;
                Ok(value)
            });
            receiver.register("hold", move |_, args| {
                *held.lock().unwrap() = Some(args[0].clone());
                Ok(Value::nil())
            });
            receiver.register("stop", |ctx, _| {
                ctx.cancellation().cancel();
                Ok(Value::nil())
            });
            let source = format!(
                "class Box\n property value\nend\ndef run\n keep=Box.new\n discarded=Box.new\n discarded.value=take_foreign()\n discarded=nil\n {ending}\nend"
            );
            let result = receiver.compile(&source).unwrap().call(
                "run",
                &[],
                CallOptions {
                    cancellation,
                    ..CallOptions::default()
                },
            );
            let succeeded = result.is_ok();
            let error = result.as_ref().err().map(|error| error.kind);
            let observation = observed.load(Ordering::SeqCst);
            drop(retained.lock().unwrap().take());
            drop(result);
            drop(receiver);
            if ending == "keep" {
                assert!(succeeded);
            } else if ending.ends_with("stop()") || cancel_on_drop {
                assert_eq!(error, Some(ErrorKind::Cancelled), "{ending}");
            } else {
                assert!(!succeeded);
            }
            assert_eq!(
                observation, 2,
                "callback retired with a locked or expired heap: {ending}"
            );
        }
    }
}

#[test]
fn imported_capability_callbacks_retire_after_object_collection_unlocks() {
    for ending in [
        "keep",
        "hold(keep); 1/0",
        "hold(keep); stop()",
        "hold(keep); keep",
    ] {
        let heap = Arc::new(Mutex::new(Weak::new()));
        let observed = Arc::new(AtomicUsize::new(0));
        let cancellation = CancellationToken::new();
        let cancel_on_drop = ending == "hold(keep); keep";
        let retirement_token = cancellation.clone();
        let observation = observed.clone();
        let retained = Arc::new(Mutex::new(None));
        let held = retained.clone();
        let mut receiver = Engine::new();
        receiver.register("take_capability", move |ctx, _| {
            *heap.lock().unwrap() = Arc::downgrade(ctx.objects.as_ref().unwrap());
            let retired = RetiredCode {
                heap: heap.clone(),
                observed: observation.clone(),
                cancellation: cancel_on_drop.then(|| retirement_token.clone()),
            };
            let method = HostMethod::new("temporary.run", move |_, _, _| {
                let _ = &retired;
                Ok(Value::nil())
            });
            Ok(Value::object(vec![(b"run".to_vec(), method.value())]))
        });
        receiver.register("hold", move |_, args| {
            *held.lock().unwrap() = Some(args[0].clone());
            Ok(Value::nil())
        });
        receiver.register("stop", |ctx, _| {
            ctx.cancellation().cancel();
            Ok(Value::nil())
        });
        let source = format!(
            "class Box\n property value\nend\ndef run\n keep=Box.new\n discarded=Box.new\n discarded.value=take_capability()\n discarded=nil\n {ending}\nend"
        );
        let result = receiver.compile(&source).unwrap().call(
            "run",
            &[],
            CallOptions {
                cancellation,
                ..CallOptions::default()
            },
        );
        let error = result.as_ref().err().map(|error| error.kind);
        let observation = observed.load(Ordering::SeqCst);
        drop(retained.lock().unwrap().take());
        drop(result);
        drop(receiver);
        let expected = if ending == "keep" {
            None
        } else if ending.ends_with("stop()") || cancel_on_drop {
            Some(ErrorKind::Cancelled)
        } else {
            Some(ErrorKind::Arithmetic)
        };
        assert_eq!(error, expected, "{ending}");
        assert_eq!(
            observation, 2,
            "capability retired with a locked or expired heap: {ending}"
        );
    }
}
