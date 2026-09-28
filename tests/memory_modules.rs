use vibescript::{CallOptions, Engine, ErrorKind, Limits};

#[test]
fn memory_modules_keep_snapshots_and_independent_call_state() {
    let mut engine = Engine::new();
    engine
        .set_module_sources(
            [(
                "counter.vibe".into(),
                "n = 0\ndef increment -> int\n n += 1\nend".into(),
            )]
            .into(),
        )
        .unwrap();
    let script = engine
        .compile("require('counter').increment + require('counter').increment")
        .unwrap();
    engine
        .set_module_sources(
            [(
                "counter.vibe".into(),
                "def increment -> int\n 99\nend".into(),
            )]
            .into(),
        )
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(3)
        );
    }
    assert_eq!(
        engine
            .compile("require('counter').increment")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(99)
    );
}

#[test]
fn memory_module_names_and_relative_escapes_are_rejected() {
    for name in [
        "../secret.vibe",
        "/secret.vibe",
        "a/../b.vibe",
        "x",
        "./x.vibe",
        "a\\b.vibe",
        "C:secret.vibe",
        "a\0.vibe",
    ] {
        assert!(
            Engine::new()
                .set_module_sources([(name.into(), "1".into())].into())
                .is_err(),
            "{name}"
        );
    }
    let mut engine = Engine::new();
    engine
        .set_module_sources([("a.vibe".into(), "require('../secret')".into())].into())
        .unwrap();
    assert!(engine.compile("require('a')").is_err());
    assert!(engine.compile("require('Cargo.toml')").is_err());
}

#[test]
fn call_stats_match_existing_success_counters_and_survive_failures() {
    let engine = Engine::new();
    let script = engine.compile("[1, 2, 3].map { |n| n + 1 }").unwrap();
    let existing = script.run(CallOptions::default()).unwrap();
    let (value, stats) = script.call_with_stats("__main__", &[], CallOptions::default());
    assert_eq!(value.unwrap().to_string(), existing.value.to_string());
    assert_eq!(
        (
            stats.steps,
            stats.peak_memory_bytes,
            stats.retained_memory_bytes
        ),
        (
            existing.stats.steps,
            existing.stats.peak_memory_bytes,
            existing.stats.retained_memory_bytes
        )
    );
    let script = engine.compile("loop { }").unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: Some(100),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    let (result, stats) = script.call_with_stats("__main__", &[], options);
    assert_eq!(result.unwrap_err().kind, ErrorKind::Steps);
    assert!(stats.steps >= 100);
    let (result, stats) = script.call_with_stats("absent", &[], CallOptions::default());
    assert_eq!(result.unwrap_err().kind, ErrorKind::Name);
    assert_eq!(stats.steps, 0);
}
