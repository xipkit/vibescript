use super::*;
use crate::{CallOptions, CancellationToken, Engine};

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
            assert_eq!(data.entries.data.len(), 1);
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
    }
}

#[test]
fn imported_host_callbacks_retire_after_object_collection_unlocks() {
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
            Ok(incoming.lock().unwrap().take().unwrap())
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
