use std::{path::PathBuf, time::Instant};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Limits, ModuleConfig, Value};

fn engine(path: &str) -> Engine {
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![PathBuf::from(path)],
            development: true,
            ..ModuleConfig::default()
        })
        .unwrap_or_else(|error| panic!("root {path}: {error}"));
    engine
}

fn result(engine: &Engine, source: &str) -> Value {
    engine
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap_or_else(|error| panic!("{source}: {error}"))
        .value
}

fn overlapping_preopens() {
    // A root resolves like its canonical guest path, so a link naming the
    // nested preopen's path selects that preopen.
    for path in ["/sandbox/real", "/sandbox/alias"] {
        assert_eq!(
            result(&engine(path), "require('numbers').run()").as_int(),
            Some(999),
            "{path}"
        );
    }
    // Walking a configured root stays within its directory, where the nested
    // preopen's guest path names the outer directory's own entry. Relative
    // imports stay within the root that supplied the importing file.
    let mut combined = Engine::new();
    combined
        .set_module_config(ModuleConfig {
            paths: vec!["/sandbox".into(), "/sandbox/real".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    for (name, expected) in [
        ("real/numbers", 7),
        ("alias/numbers", 7),
        ("only_other", 999),
    ] {
        assert_eq!(
            result(&combined, &format!("require('{name}').run()")).as_int(),
            Some(expected),
            "{name}"
        );
    }
    println!("{{\"status\":\"passed\",\"case\":\"overlapping-preopens\"}}");
}

fn module_guards(root: &str, path_based_directories: bool) {
    let config = ModuleConfig {
        paths: vec![root.into()],
        ..ModuleConfig::default()
    };
    let mut restricted = Engine::new();
    restricted.set_strict_effects(true);
    restricted.set_module_config(config.clone()).unwrap();
    let script = restricted.compile("require('numbers').run()").unwrap();
    let error = script.run(CallOptions::default()).unwrap_err();
    assert!(error.message.starts_with("strict effects: "), "{error}");
    assert_eq!(
        script
            .run(CallOptions {
                allow_require: true,
                ..CallOptions::default()
            })
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    // Cache hits must still respect each invocation's permission.
    assert!(script.run(CallOptions::default()).is_err());
    for config in [
        ModuleConfig {
            deny: vec!["numbers".into()],
            ..config.clone()
        },
        ModuleConfig {
            allow: vec!["elsewhere".into()],
            ..config.clone()
        },
        ModuleConfig {
            source_limit: 4,
            ..config
        },
    ] {
        let mut engine = Engine::new();
        engine.set_module_config(config).unwrap();
        assert!(
            engine
                .compile("require('numbers').run()")
                .unwrap()
                .run(CallOptions::default())
                .is_err()
        );
    }
    let prefix = root.rsplit_once('/').unwrap().0;
    let moved = engine(&format!("{prefix}/moving"));
    std::fs::rename(format!("{prefix}/moving"), format!("{prefix}/moved")).unwrap();
    std::fs::rename(format!("{prefix}/replacement"), format!("{prefix}/moving")).unwrap();
    assert_eq!(
        result(&moved, "require('numbers').run()").as_int(),
        Some(if path_based_directories { 999 } else { 7 })
    );
    assert_eq!(
        result(
            &engine(&format!("{prefix}/moving")),
            "require('numbers').run()"
        )
        .as_int(),
        Some(999)
    );
}

fn cwd_roots(prefix: &str) {
    std::env::set_current_dir(prefix).unwrap();
    for path in ["allowed", "allowed/sub/..", "directory_alias/.."] {
        assert_eq!(
            result(&engine(path), "require('numbers').run()").as_int(),
            Some(7)
        );
    }
    assert!(
        Engine::new()
            .set_module_config(ModuleConfig {
                paths: vec!["missing/../allowed".into()],
                ..ModuleConfig::default()
            })
            .is_err()
    );
    let long = format!(
        "allowed/{}",
        "long-directory-component-012345678901234567890123456789/".repeat(12)
    );
    assert_eq!(
        result(&engine(&long), "require('numbers').run()").as_int(),
        Some(7)
    );
}

fn main() {
    let root = std::env::args().nth(1).expect("module root");
    if root == "--overlap" {
        overlapping_preopens();
        return;
    }
    let absolute_links = !std::env::args().any(|arg| arg == "--deny-absolute-links");
    let path_based_directories = std::env::args().any(|arg| arg == "--path-based-directories");
    let prefix = root.rsplit_once('/').unwrap().0;
    let engine = engine(&root);
    assert_eq!(
        result(&engine, "[1,2,1].index(1,9223372036854775807)").to_string(),
        "nil"
    );
    assert_eq!(
        result(&engine, "[1,2,1].rindex(1,9223372036854775807)").as_int(),
        Some(2)
    );
    assert_eq!(result(&engine, "(1..100).sum").as_int(), Some(5050));
    assert_eq!(
        result(&engine, "JSON.parse('{\"value\":7}').value").as_int(),
        Some(7)
    );
    assert_eq!(
        result(&engine, "Time.utc(2024,2,29).strftime('%F')").as_bytes(),
        Some(b"2024-02-29".as_slice())
    );
    assert!(result(&engine, "Time.now.to_i").as_int().unwrap() > 1_700_000_000);
    assert_eq!(result(&engine, "uuid().length").as_int(), Some(36));
    assert_eq!(
        result(&engine, "require('numbers').run()").as_int(),
        Some(7)
    );
    for name in ["alias", "sub/relative"] {
        assert_eq!(
            result(&engine, &format!("require('{name}').run()")).as_int(),
            Some(7)
        );
    }
    let absolute = |engine: &Engine| {
        let result = engine
            .compile("require('absolute').run()")
            .unwrap()
            .run(CallOptions::default());
        if absolute_links {
            assert_eq!(result.unwrap().value.as_int(), Some(7));
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.kind, ErrorKind::Runtime);
            assert!(error.message.contains("reading module symlink"), "{error}");
        }
    };
    absolute(&engine);
    for name in [
        "../outside",
        "escape",
        "absolute_escape",
        "prefix_escape",
        "broken",
        "Numbers",
        "folder",
    ] {
        let source = format!("require('{name}')");
        assert!(
            engine
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .is_err(),
            "{name}"
        );
    }
    // Wasmtime rejects names that are not UTF-8; like native targets, the
    // engine reports them missing rather than as a filesystem failure.
    let error = engine
        .compile("require(\"\\xff\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("module not found"), "{error}");
    for _ in 0..2 {
        assert_eq!(
            result(&engine, "require('counter').increment()").as_int(),
            Some(1)
        );
    }
    let fresh = engine.compile("require('changed').run()").unwrap();
    assert_eq!(
        fresh.run(CallOptions::default()).unwrap().value.as_int(),
        Some(3)
    );
    std::fs::write(format!("{root}/changed.vibe"), "def run; 2222; end").unwrap();
    assert_eq!(
        fresh.run(CallOptions::default()).unwrap().value.as_int(),
        Some(2222)
    );
    let alias = self::engine(&format!("{prefix}/root_alias"));
    absolute(&alias);
    for path in [
        format!("{root}/."),
        format!("{root}/sub/.."),
        format!("{prefix}/root_alias/sub/.."),
        format!("{prefix}/directory_alias/.."),
        format!("{prefix}//allowed"),
        root.trim_start_matches('/').to_owned(),
    ] {
        assert_eq!(
            result(&self::engine(&path), "require('numbers').run()").as_int(),
            Some(7),
            "{path}"
        );
    }
    assert_eq!(
        result(&self::engine(prefix), "require('allowed/numbers').run()").as_int(),
        Some(7)
    );
    for path in [
        format!("{prefix}/missing/../allowed"),
        format!("{root}/numbers.vibe"),
        format!("{prefix}/root_loop_a"),
    ] {
        assert!(
            Engine::new()
                .set_module_config(ModuleConfig {
                    paths: vec![PathBuf::from(&path)],
                    ..ModuleConfig::default()
                })
                .is_err(),
            "{path}"
        );
    }
    let script = engine
        .compile("begin; (1..100000).sum; rescue; 7; end")
        .unwrap();
    assert_eq!(
        script
            .run(CallOptions {
                limits: Limits {
                    steps: Some(100),
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        script
            .run(CallOptions {
                cancellation: cancelled,
                ..CallOptions::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(
        script
            .run(CallOptions {
                deadline: Some(Instant::now()),
                ..CallOptions::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::Deadline
    );
    module_guards(&root, path_based_directories);
    let counted = self::engine(&format!("{prefix}/accounting"))
        .compile("require('numbers').run()")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(counted.value.as_int(), Some(7));
    cwd_roots(prefix);
    println!(
        "{{\"status\":\"passed\",\"target\":\"wasm32-wasip1\",\"accounting\":[{},{},{}]}}",
        counted.stats.steps, counted.stats.peak_memory_bytes, counted.stats.retained_memory_bytes
    );
}
