use std::sync::{Arc, Mutex};
use vibescript::{CallOptions, Engine, ErrorClass, ErrorKind, Value};

/// Deep enough that depth-proportional drop or formatting glue would overflow a test thread.
const DEEP: usize = 10_001;

fn on_small_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(work)
        .unwrap()
        .join()
        .unwrap()
}

fn array_chain(levels: usize) -> Value {
    let mut value = Value::int(0);
    for _ in 0..levels {
        value = Value::array(vec![value]);
    }
    value
}

fn hash_chain(levels: usize) -> Value {
    let mut value = Value::int(0);
    for _ in 0..levels {
        value = Value::hash(vec![(b"k".to_vec(), value)]);
    }
    value
}

fn mixed_chain(levels: usize) -> Value {
    let mut value = Value::int(0);
    for level in 0..levels {
        value = if level % 2 == 0 {
            Value::array(vec![value])
        } else {
            Value::hash(vec![(b"k".to_vec(), value)])
        };
    }
    value
}

#[test]
fn deep_host_values_drop_on_a_small_stack() {
    on_small_stack(|| {
        drop(array_chain(DEEP));
        drop(hash_chain(DEEP));
        drop(mixed_chain(DEEP));
        let mut pending = vec![array_chain(DEEP), hash_chain(DEEP)];
        pending.pop();
        drop(pending);
    });
}

#[test]
fn shared_branches_drop_once_and_stay_intact_for_other_owners() {
    on_small_stack(|| {
        let shared = mixed_chain(DEEP);
        let left = Value::array(vec![shared.clone(), Value::int(1)]);
        let right = Value::hash(vec![(b"deep".to_vec(), shared)]);
        drop(left);
        let kept = &right.as_hash().unwrap()[0].1;
        assert_eq!(kept.as_array().unwrap().len(), 1);
        drop(right);

        let mut ladder = Value::int(0);
        for _ in 0..40 {
            ladder = Value::array(vec![ladder.clone(), ladder]);
        }
        drop(ladder);
    });
}

#[test]
fn deep_values_format_without_recursion() {
    on_small_stack(|| {
        assert_eq!(
            array_chain(DEEP).to_string(),
            format!("{}0{}", "[".repeat(DEEP), "]".repeat(DEEP))
        );
        let text = hash_chain(DEEP).to_string();
        assert_eq!(
            text,
            format!("{}0{}", "{k: ".repeat(DEEP), "}".repeat(DEEP))
        );
        let debug = format!("{:?}", mixed_chain(DEEP));
        assert!(debug.starts_with("Value(Array([Value(Hash({"));
        assert_eq!(debug.matches("Value(Int(0))").count(), 1);
        assert_eq!(debug.matches("Value(Array([").count(), DEEP.div_ceil(2));
        assert_eq!(debug.matches("Value(Hash({").count(), DEEP / 2);
        assert!(debug.ends_with("]))"));
        assert_eq!(
            Value::array(vec![
                Value::int(1),
                Value::hash(vec![(b"a".to_vec(), Value::nil())])
            ])
            .to_string(),
            "[1, {a: nil}]"
        );
    });
}

#[test]
fn host_functions_import_and_release_deep_values() {
    let mut engine = Engine::new();
    engine.register("probe", |ctx, _| {
        let before = ctx.stats().retained_memory_bytes;
        let imported = ctx.import(&mixed_chain(100))?;
        assert!(ctx.stats().retained_memory_bytes > before);
        assert_eq!(imported.to_string(), mixed_chain(100).to_string());
        drop(imported);
        assert_eq!(ctx.stats().retained_memory_bytes, before);

        let error = ctx.import(&array_chain(DEEP)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().retained_memory_bytes, before);
        Ok(Value::int(1))
    });
    let outcome = engine
        .compile("probe()")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(1));
    assert_eq!(outcome.stats.retained_memory_bytes, 0);
}

#[test]
fn interrupted_imports_release_partially_built_containers() {
    let measured = Arc::new(Mutex::new(None));
    let recorder = measured.clone();
    let mut engine = Engine::new();
    engine.register("probe", move |ctx, _| {
        let wide = Value::array((0..4).map(|_| mixed_chain(100)).collect());
        let before = ctx.stats();
        match ctx.import(&wide) {
            Ok(imported) => {
                let after = ctx.stats();
                drop(imported);
                assert_eq!(
                    ctx.stats().retained_memory_bytes,
                    before.retained_memory_bytes
                );
                *recorder.lock().unwrap() = Some((before.steps, after.steps));
                Ok(Value::nil())
            }
            Err(error) => {
                assert_eq!(error.kind, ErrorKind::Steps);
                assert_eq!(
                    ctx.stats().retained_memory_bytes,
                    before.retained_memory_bytes
                );
                Err(error)
            }
        }
    });
    let script = engine.compile("probe()").unwrap();
    script.run(CallOptions::default()).unwrap();
    let recorded: Option<(u64, u64)> = *measured.lock().unwrap();
    let (before, after) = recorded.unwrap();
    assert!(after > before + 400);
    let mut options = CallOptions::default();
    options.limits.steps = Some(before + (after - before) / 2);
    let error = script.run(options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Steps);
}

#[test]
fn deep_arguments_are_rejected_as_limit_errors() {
    let script = Engine::new().compile("def run(x)\nx\nend").unwrap();
    let error = script
        .call("run", &[array_chain(DEEP)], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Recursion);
    assert_eq!(error.class(), Some(ErrorClass::Limit));
    let outcome = script
        .call("run", &[mixed_chain(100)], CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.to_string(), mixed_chain(100).to_string());
}
