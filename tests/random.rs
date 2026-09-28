mod common;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use vibescript::{
    CallOptions, CancellationToken, Engine, Error, ErrorKind, Limits, Value, stringify_json,
};

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

fn engine(byte: u8) -> (Engine, Arc<AtomicUsize>) {
    let mut engine = Engine::new();
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    engine.set_random_source(move |_, output| {
        count.fetch_add(1, Ordering::SeqCst);
        output.fill(byte);
        Ok(output.len())
    });
    (engine, reads)
}

#[test]
fn seed_sequences_reset_and_entropy_functions_do_not_advance_them() {
    let (engine, reads) = engine(0);
    let script = engine
        .compile(
            r#"
def run() -> array<bool | int | nil>
 srand(42)
 expected=[rand,rand(100),rand(-9223372036854775808..9223372036854775807)]
 old=srand(42)
 uuid
 random_id(8)
 actual=[rand,rand(100),rand(-9223372036854775808..9223372036854775807)]
 [expected==actual,old,srand(-1),srand(0),srand(9223372036854775807)]
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([true, 42, 42, -1, 0])
    );
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert!(output.stats.retained_memory_bytes < 1024);
}

#[test]
fn seeded_state_is_isolated_between_calls_and_threads_and_released_on_return() {
    let (engine, reads) = engine(0);
    let script = engine
        .compile(
            r#"
def seeded -> array<int | float>
 srand(7)
 [rand,rand(10),rand(-9223372036854775808..9223372036854775807)]
end
def unseeded -> array<int | float | nil>
 [rand,srand(1),rand(1)]
end
def discard
 srand(7)
 nil
end
"#,
        )
        .unwrap();
    let expected = json(
        &script
            .call("seeded", &[], CallOptions::default())
            .unwrap()
            .value,
    );
    common::scope(|scope| {
        let jobs = (0..8)
            .map(|_| scope.spawn(|| script.call("seeded", &[], CallOptions::default()).unwrap()))
            .collect::<Vec<_>>();
        for job in jobs {
            let output = job.join().unwrap();
            assert_eq!(json(&output.value), expected);
            assert!(output.stats.retained_memory_bytes < 1024);
        }
    });
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        json(
            &script
                .call("unseeded", &[], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([0, null, 0])
    );
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    for _ in 0..20 {
        let output = script.call("discard", &[], CallOptions::default()).unwrap();
        assert_eq!(output.stats.retained_memory_bytes, 0);
        assert!(output.stats.peak_memory_bytes < 16384);
    }
}

#[test]
fn fixed_entropy_covers_float_endpoints_and_signed_range_extremes() {
    for byte in [0, 255] {
        let (engine, reads) = engine(byte);
        let script = engine
            .compile(
                r#"
[
 rand,rand(1),rand(2),rand(-9223372036854775808..9223372036854775807),
 rand(9223372036854775807..-9223372036854775808),rand(0..9223372036854775807)
]
"#,
            )
            .unwrap();
        let output = script.run(CallOptions::default()).unwrap();
        let values = output.value.as_array().unwrap();
        let f = values[0].as_float().unwrap();
        assert_eq!(
            f.to_bits(),
            (if byte == 0 {
                0.0f64
            } else {
                1.0f64 - f64::EPSILON / 2.0
            })
            .to_bits()
        );
        assert_eq!(values[1].as_int(), Some(0));
        assert_eq!(values[2].as_int(), Some(if byte == 0 { 0 } else { 1 }));
        assert_eq!(
            values[3].as_int(),
            Some(if byte == 0 { i64::MIN } else { i64::MAX })
        );
        assert_eq!(values[4].as_int(), values[3].as_int());
        assert_eq!(
            values[5].as_int(),
            Some(if byte == 0 { 0 } else { i64::MAX })
        );
        assert_eq!(reads.load(Ordering::SeqCst), 6);
    }
}

#[test]
fn unbiased_tokens_retry_rejected_bytes_and_accept_short_reads() {
    let bytes = [
        248, 249, 250, 251, 252, 253, 254, 255, 0, 61, 62, 123, 124, 185, 186, 247,
    ];
    let offset = Arc::new(AtomicUsize::new(0));
    let cursor = offset.clone();
    let mut engine = Engine::new();
    engine.set_random_source(move |_, output| {
        let count = output.len().min(3);
        let start = cursor.fetch_add(count, Ordering::SeqCst);
        output[..count].copy_from_slice(&bytes[start..start + count]);
        Ok(count)
    });
    let output = engine
        .compile("random_id(8)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_bytes().unwrap(), b"a9a9a9a9");
    assert_eq!(offset.load(Ordering::SeqCst), 16);
}

#[test]
fn rejected_integer_samples_retry_without_modulo_bias() {
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    let mut engine = Engine::new();
    engine.set_random_source(move |_, output| {
        output.fill(if count.fetch_add(1, Ordering::SeqCst) == 0 {
            255
        } else {
            0
        });
        Ok(output.len())
    });
    let output = engine
        .compile("rand(3)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_int(), Some(0));
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[test]
fn invalid_signatures_stop_before_entropy_or_followup_host_effects() {
    let (mut engine, reads) = engine(0);
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "rand(0)",
        "rand(-1)",
        "rand(9223372036854775808)",
        "rand(1...1)",
        "rand(..3)",
        "rand(1..)",
        "srand(9223372036854775808)",
        "random_id(0)",
        "random_id(-1)",
        "random_id(1025)",
        "random_id(9223372036854775808)",
    ] {
        assert!(
            engine
                .compile(&format!("{source};effect()"))
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{source}"
        );
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    // An argument of the wrong type from the host fails when it is cast,
    // after its call and before any entropy is read.
    assert!(
        engine
            .compile("random_id(effect().as(int));effect()")
            .unwrap()
            .run(CallOptions::default())
            .is_err()
    );
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    // Other wrong signatures are refused before anything runs.
    let mut checked = vibescript::Engine::new();
    checked.register("effect", |_, _| panic!("effect ran"));
    for (source, code) in [
        ("rand(1.0)", "V0101"),
        ("rand(1,2)", "V0301"),
        ("rand(extra:1)", "V0301"),
        ("rand{effect()}", "V0301"),
        ("srand(1.0)", "V0101"),
        ("srand(1,2)", "V0301"),
        ("srand(nil){effect()}", "V0305"),
        ("uuid(1)", "V0301"),
        ("uuid(extra:1)", "V0302"),
        ("uuid{effect()}", "V0305"),
        ("random_id(nil)", "V0101"),
        ("random_id(8.9)", "V0101"),
        ("random_id(8){effect()}", "V0305"),
    ] {
        let error = checked
            .compile(&format!("{source};effect()"))
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), [code], "{source}");
    }
}

#[test]
fn entropy_errors_short_stalls_and_invalid_counts_propagate() {
    for outcome in [0, usize::MAX, 1] {
        let mut engine = Engine::new();
        engine.set_random_source(move |_, _| {
            if outcome == 1 {
                Err(Error::new(ErrorKind::Host, "entropy unavailable"))
            } else {
                Ok(outcome)
            }
        });
        for source in ["rand", "srand", "uuid", "random_id"] {
            assert_eq!(
                engine
                    .compile(source)
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::Host,
                "{source}"
            );
        }
    }
    let (engine, reads) = engine(255);
    assert_eq!(
        engine
            .compile("random_id(4)")
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Host
    );
    assert_eq!(reads.load(Ordering::SeqCst), 9);
}

#[test]
fn cancellation_and_deadlines_are_checked_before_and_after_entropy_callbacks() {
    for source in ["rand", "srand", "uuid", "random_id"] {
        let (engine, reads) = engine(0);
        let script = engine.compile(source).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        for (options, kind) in [
            (
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                },
                ErrorKind::Cancelled,
            ),
            (
                CallOptions {
                    deadline: Some(Instant::now()),
                    ..CallOptions::default()
                },
                ErrorKind::Deadline,
            ),
        ] {
            assert_eq!(script.run(options).unwrap_err().kind, kind);
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        for partial in [false, true] {
            let mut engine = Engine::new();
            let effects = Arc::new(AtomicUsize::new(0));
            let count = effects.clone();
            engine.register("effect", move |_, _| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(Value::nil())
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            engine.set_random_source(move |ctx, output| {
                count.fetch_add(1, Ordering::SeqCst);
                output.fill(0);
                ctx.cancellation().cancel();
                Ok(if partial { 1 } else { output.len() })
            });
            assert_eq!(
                engine
                    .compile(&format!("{source};effect()"))
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err()
                    .kind,
                ErrorKind::Cancelled
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(effects.load(Ordering::SeqCst), 0);
        }
    }
}

#[test]
fn actual_os_entropy_produces_valid_uuid_timestamps_and_tokens() {
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let output = Engine::new()
        .compile("[uuid,random_id(1024),rand]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let after = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let values = output.value.as_array().unwrap();
    let uuid = std::str::from_utf8(values[0].as_bytes().unwrap()).unwrap();
    assert_eq!(uuid.len(), 36);
    let compact = uuid.replace('-', "");
    assert_eq!(compact.len(), 32);
    assert!(
        compact
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(&uuid[14..15], "7");
    assert!(matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    let millis = u128::from_str_radix(&compact[..12], 16).unwrap();
    assert!(millis >= before && millis <= after);
    let token = values[1].as_bytes().unwrap();
    assert_eq!(token.len(), 1024);
    assert!(token.iter().all(u8::is_ascii_alphanumeric));
    assert!((0.0..1.0).contains(&values[2].as_float().unwrap()));
}

#[test]
fn uuids_sort_in_creation_order_within_and_across_calls() {
    let script = Engine::new()
        .compile(
            "def run -> array<string>\n  ids: array<string> = []\n  2000.times { ids << uuid }\n  ids\nend\n",
        )
        .unwrap();
    let batches: Vec<Vec<String>> = common::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    let output = script.call("run", &[], CallOptions::default()).unwrap();
                    output
                        .value
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|id| String::from_utf8(id.as_bytes().unwrap().to_vec()).unwrap())
                        .collect()
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    let mut all = std::collections::HashSet::new();
    for ids in &batches {
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
        for id in ids {
            assert_eq!(&id[14..15], "7");
            assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
            assert!(all.insert(id.clone()));
        }
    }
    assert_eq!(all.len(), 8000);
}

#[test]
fn reader_ignored_exhaustion_and_rejected_sampling_stop_later_effects() {
    for ignored in [false, true] {
        let mut engine = Engine::new();
        let effects = Arc::new(AtomicUsize::new(0));
        let count = effects.clone();
        engine.register("effect", move |_, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        engine.set_random_source(move |ctx, output| {
            if ignored {
                let _ = ctx.charge(1000);
            }
            output.fill(255);
            Ok(output.len())
        });
        let source = if ignored {
            "rand;effect()"
        } else {
            "rand(3);effect()"
        };
        let options = CallOptions {
            limits: Limits {
                steps: Some(128),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(options)
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn configured_readers_are_captured_at_compile_time_and_allow_host_reentry() {
    let mut engine = Engine::new();
    let nested = Engine::new().compile("srand(7);rand(1)").unwrap();
    engine.set_random_source(move |ctx, output| {
        ctx.charge(1)?;
        let result = nested.run(CallOptions::default())?;
        assert_eq!(result.value.as_int(), Some(0));
        output.fill(0);
        Ok(output.len())
    });
    let old = engine.compile("rand(2)").unwrap();
    engine.set_random_source(|_, output| {
        output.fill(255);
        Ok(output.len())
    });
    let new = engine.compile("rand(2)").unwrap();
    assert_eq!(
        old.run(CallOptions::default()).unwrap().value.as_int(),
        Some(0)
    );
    assert_eq!(
        new.run(CallOptions::default()).unwrap().value.as_int(),
        Some(1)
    );
    assert_eq!(
        old.run(CallOptions::default()).unwrap().value.as_int(),
        Some(0)
    );
}

#[test]
fn bare_random_helpers_run_like_empty_calls() {
    let (engine, _) = engine(0);
    let script = engine
        .compile(
            r#"
def run() -> array<int?>
 first=srand
 srand(42)
 previous=srand
 id=random_id
 [first,previous,srand,id.length,random_id.length]
end
"#,
        )
        .unwrap();
    let output = script.call("run", &[], CallOptions::default()).unwrap();
    let value = json(&output.value);
    assert_eq!(value[0], serde_json::Value::Null);
    assert_eq!(value[1], 42);
    assert_eq!(value[3], 16);
    assert_eq!(value[4], 16);
    for source in ["srand { 1 }", "random_id { 1 }"] {
        let error = vibescript::Engine::new().compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0305"], "{source}");
    }
}
