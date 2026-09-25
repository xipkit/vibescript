use crate::{CallOptions, Capability, Engine, ErrorKind, HostMethod, Stats, Value, budget::Memory};
use std::{
    sync::{Arc, Mutex, Weak},
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
enum Limit {
    Measure,
    Steps(u64),
    Memory(usize),
    Cancel,
    Deadline,
}

#[derive(Default)]
struct Observation {
    before: Stats,
    after: Stats,
    error: Option<ErrorKind>,
    memory: Weak<Memory>,
    called: bool,
}

fn probe(limit: Limit) -> (crate::Result<crate::Outcome>, Observation) {
    let observed = Arc::new(Mutex::new(Observation::default()));
    let record = observed.clone();
    let method = HostMethod::new_with_block("cap.capture", move |call, _, _| {
        let ctx = call.context();
        let before = ctx.stats();
        let memory = Arc::downgrade(&ctx.identity());
        let previous = ctx.options.limits.clone();
        match limit {
            Limit::Measure => (),
            Limit::Steps(work) => ctx.options.limits.steps = Some(before.steps + work),
            Limit::Memory(bytes) => {
                ctx.options.limits.memory_bytes = Some(before.retained_memory_bytes + bytes);
            }
            Limit::Cancel => ctx.cancellation().cancel(),
            Limit::Deadline => ctx.options.deadline = Some(Instant::now()),
        }
        let result = call.receiver();
        let ctx = call.context();
        let after = ctx.stats();
        assert!(ctx.snapshot_objects.is_none());
        assert!(ctx.snapshot_namespaces.is_none());
        assert!(ctx.pending_objects.data.is_empty());
        assert!(!ctx.importing_objects);
        let error = result.as_ref().err().map(|error| error.kind);
        if let Some(kind) = error {
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        ctx.options.limits = previous;
        *record.lock().unwrap() = Observation {
            before,
            after,
            error,
            memory,
            called: true,
        };
        // The callback deliberately ignores failures; the invocation must still
        // surface a latched error after the temporary snapshot state is cleared.
        Ok(Value::nil())
    });
    // The program stores a module in the capability object it is granted,
    // which static types refuse, so it compiles without them.
    let mut engine = Engine::new();
    engine.set_static_types(false);
    let result = engine
        .compile(
            "module Data
               ITEMS=(1..256).to_a
               @@value=4
               def self.value; @@value; end
             end
             cap[:data]=Data
             cap.capture()
             7",
        )
        .unwrap()
        .run(CallOptions {
            capabilities: vec![Capability::from_value(
                "cap",
                Value::object(vec![(b"capture".to_vec(), method.value())]),
            )],
            ..CallOptions::default()
        });
    let observation = std::mem::take(&mut *observed.lock().unwrap());
    assert!(observation.called, "{limit:?}");
    assert_eq!(observation.memory.strong_count(), 0, "{limit:?}");
    (result, observation)
}

#[test]
fn vm_snapshot_limits_are_exact_latched_and_release_partial_state() {
    let (result, measured) = probe(Limit::Measure);
    assert_eq!(result.unwrap().value.as_int(), Some(7));
    assert!(measured.error.is_none());
    assert!(measured.after.peak_memory_bytes > measured.before.peak_memory_bytes);
    let work = measured.after.steps - measured.before.steps;
    let bytes = measured.after.peak_memory_bytes - measured.before.retained_memory_bytes;
    for limit in [Limit::Steps(work), Limit::Memory(bytes)] {
        let (result, observation) = probe(limit);
        assert!(observation.error.is_none(), "{limit:?}");
        assert_eq!(result.unwrap().value.as_int(), Some(7));
    }
    let mut failures = vec![
        (Limit::Cancel, ErrorKind::Cancelled),
        (Limit::Deadline, ErrorKind::Deadline),
    ];
    for allowed in [0, 1, work / 4, work / 2, work - 1] {
        failures.push((Limit::Steps(allowed), ErrorKind::Steps));
    }
    for allowed in [0, 1, bytes / 4, bytes / 2, bytes - 1] {
        failures.push((Limit::Memory(allowed), ErrorKind::Memory));
    }
    for (limit, kind) in failures {
        let (result, observation) = probe(limit);
        assert_eq!(observation.error, Some(kind), "{limit:?}");
        assert_eq!(result.unwrap_err().kind, kind, "{limit:?}");
    }
}
