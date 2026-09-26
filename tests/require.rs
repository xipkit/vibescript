mod common;

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript::{
    CallOptions, CancellationToken, Engine, Error, ErrorClass, ErrorKind, ModuleConfig, Position,
    Value, stringify_json,
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let path = base.join(format!(
                "require-{}-{}",
                common::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create require fixtures: {error}"),
            }
        }
    }

    fn write(&self, name: &str, source: &str) {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn engine(&self) -> Engine {
        self.configure(Engine::new())
    }

    fn configure(&self, mut engine: Engine) -> Engine {
        engine
            .set_module_config(ModuleConfig {
                paths: vec![self.0.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
        engine
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let result = fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.unwrap();
        }
    }
}

fn json(value: &Value) -> serde_json::Value {
    let encoded = stringify_json(value, CallOptions::default()).unwrap();
    serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn host_signatures_resolve_required_source_types_defaults_and_root_fallbacks() {
    use vibescript::{HostMethod, Signature, SignatureParam};
    let files = Files::new();
    files.write(
        "widgets.vibe",
        "class Widget; def id -> int; 7; end; end; def run(x: Widget = tag(Widget.new).as(Widget)) -> int; tag(x).as(Widget).id; end",
    );
    files.write("levels.vibe", "enum Level; Debug; Info; end; def run(x: Level = level(:debug).as(Level)) -> array<bool>; [x==Level::Debug, level(:info)==Level::Info]; end");
    files.write(
        "fallback.vibe",
        "def run -> bool; level(:root)==Level::Root; end",
    );
    for strict in [false, true] {
        // The checker types a host result of a script type as `any`, which
        // each file casts; the host contracts resolve the types in that file.
        let mut engine = files.engine();
        engine.set_strict_effects(strict);
        for (method, ty) in [("tag", "Widget"), ("level", "Level")] {
            engine.register_method(
                method,
                HostMethod::new(method, |_, args, _| Ok(args[0].clone()))
                    .with_signature(Signature {
                        params: vec![SignatureParam {
                            name: "value".into(),
                            ty: ty.into(),
                            optional: false,
                        }],
                        result: ty.into(),
                        accepts_block: false,
                    })
                    .unwrap(),
            );
        }
        let script = engine.compile("class Widget; def id -> int; 99; end; end; enum Level; Root; end; def run -> array<any>; [require(\"widgets\").run, require(\"levels\").run]; end").unwrap();
        for _ in 0..2 {
            assert_eq!(
                script
                    .call(
                        "run",
                        &[],
                        CallOptions {
                            allow_require: true,
                            ..CallOptions::default()
                        }
                    )
                    .unwrap()
                    .value
                    .to_string(),
                "[7, [true, true]]"
            );
        }
        // A required file never resolves the receiving script's declarations,
        // so the fallback's `Level` is unknown.
        let error = engine
            .compile("class Widget; end; enum Level; Root; end; require(\"fallback\").run")
            .err()
            .unwrap();
        assert_eq!(common::codes(&error), ["V0201"]);
        assert_eq!(
            error.diagnostics()[0].file.as_deref(),
            Some(b"fallback.vibe".as_slice())
        );
    }
}

#[test]
fn required_files_use_the_receiving_calls_capability_grants() {
    let files = Files::new();
    files.write(
        "notify.vibe",
        "def notify(n: int) -> any; sms.deliver(n); end",
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let method = vibescript::HostMethod::new("sms.deliver", move |_, args, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(args[0].as_int().unwrap() * 2))
    });
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    engine
        .declare_capability(&vibescript::Capability::from_value(
            "sms",
            Value::object(vec![(b"deliver".to_vec(), method.value())]),
        ))
        .unwrap();
    let script = engine
        .compile("def run -> any; require(\"notify\").notify(21); end")
        .unwrap();
    let options = CallOptions {
        allow_require: true,
        capabilities: vec![vibescript::Capability::new("sms", move |_| {
            Ok(Value::object(vec![(b"deliver".to_vec(), method.value())]))
        })],
        ..CallOptions::default()
    };
    for _ in 0..2 {
        assert_eq!(
            script
                .call("run", &[], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(42)
        );
    }
    let error = script
        .call(
            "run",
            &[],
            CallOptions {
                allow_require: true,
                ..CallOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert!(error.message.contains("missing capability sms"), "{error}");
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

fn require_denied(error: &Error) {
    assert_eq!(error.kind, ErrorKind::Runtime, "{error}");
    assert_eq!(error.class(), Some(ErrorClass::Runtime), "{error}");
    assert!(
        error
            .message
            .starts_with("strict effects: require is disabled"),
        "{error}"
    );
}

#[test]
fn cold_compilation_obeys_limits_without_publishing_or_initializing() {
    let files = Files::new();
    let source = format!(
        "initialized();def unused -> array<int>;{}end;def value -> int;42;end",
        "[1,2,3].map{|n|n+1};".repeat(256)
    );
    files.write("answer.vibe", &source);
    let initialized = Arc::new(AtomicUsize::new(0));
    let unwound = Arc::new(AtomicUsize::new(0));
    let mut engine = files.engine();
    let captured = initialized.clone();
    engine.register("initialized", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let captured = unwound.clone();
    engine.register("unwound", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("begin;require(\"answer\").value;rescue;unwound();ensure;unwound();end")
        .unwrap();
    let probe = engine.compile("require(\"answer\").value").unwrap();
    let mut unlimited = CallOptions::default();
    unlimited.limits.steps = None;
    let cold = script.run(unlimited.clone()).unwrap();
    let warm = script.run(unlimited.clone()).unwrap();
    assert_eq!(cold.value.as_int(), Some(42));
    assert_eq!(warm.value.as_int(), Some(42));
    assert!(cold.stats.steps > warm.stats.steps * 4);
    assert!(cold.stats.peak_memory_bytes > warm.stats.peak_memory_bytes * 4);
    assert_eq!(cold.stats.retained_memory_bytes, 0);
    for kind in [ErrorKind::Steps, ErrorKind::Memory] {
        engine.clear_module_cache();
        initialized.store(0, Ordering::SeqCst);
        unwound.store(0, Ordering::SeqCst);
        let mut limited = unlimited.clone();
        if kind == ErrorKind::Memory {
            limited.limits.memory_bytes = Some(cold.stats.peak_memory_bytes - 1);
        } else {
            limited.limits.steps = Some(cold.stats.steps / 2);
        }
        assert_eq!(script.run(limited).unwrap_err().kind, kind);
        assert_eq!(initialized.load(Ordering::SeqCst), 0);
        assert_eq!(unwound.load(Ordering::SeqCst), 0);
        fs::remove_file(files.0.join("answer.vibe")).unwrap();
        assert_eq!(
            probe.run(unlimited.clone()).unwrap_err().kind,
            ErrorKind::Name
        );
        files.write("answer.vibe", &source);
        let mut exact = unlimited.clone();
        exact.limits.steps = Some(cold.stats.steps);
        exact.limits.memory_bytes = Some(cold.stats.peak_memory_bytes);
        let retried = script.run(exact).unwrap();
        assert_eq!(retried.value.as_int(), Some(42));
        assert_eq!(retried.stats.steps, cold.stats.steps);
        assert_eq!(
            retried.stats.peak_memory_bytes,
            cold.stats.peak_memory_bytes
        );
        assert_eq!(retried.stats.retained_memory_bytes, 0);
        assert_eq!(initialized.load(Ordering::SeqCst), 1);
        assert_eq!(unwound.load(Ordering::SeqCst), 1);
        let mut cached = unlimited.clone();
        cached.limits.steps = Some(warm.stats.steps);
        cached.limits.memory_bytes = Some(warm.stats.peak_memory_bytes);
        assert_eq!(script.run(cached).unwrap().value.as_int(), Some(42));
    }
}

#[test]
fn cold_module_type_and_percent_parsing_cannot_rescue_exhaustion() {
    let files = Files::new();
    let mut engine = files.engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    // The file compiles with the script; each replacement is parsed cold
    // when the call requires it.
    files.write("input.vibe", "nil");
    let script = engine
        .compile("begin;require(\"input\");rescue;effect();ensure;effect();end")
        .unwrap();
    for source in [
        format!("x=1;x %w[{}", "abc ".repeat(1024)),
        format!("schema={{ {}", "field:array<int>,".repeat(512)),
        format!("\"{}", "\\n".repeat(2048)),
    ] {
        files.write("input.vibe", &source);
        engine.clear_module_cache();
        let mut options = CallOptions::default();
        options.limits.steps = Some(1000);
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Steps);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn required_compilation_accepts_deep_type_literals() {
    let files = Files::new();
    let literal = format!("{}int{}", "{x:".repeat(63), "}".repeat(63));
    files.write("deep.vibe", &format!("def value -> any;{literal};end"));
    let script = files.engine().compile("require(\"deep\").value").unwrap();
    let expected = format!("{}int{}", "{ x: ".repeat(63), " }".repeat(63));
    for _ in 0..2 {
        let result = script.run(CallOptions::default()).unwrap();
        assert_eq!(result.value.as_type_literal(), Some(expected.as_bytes()));
    }
}

#[test]
fn required_compilation_rejects_excessive_nesting_without_initializing() {
    let files = Files::new();
    let effects = Arc::new(AtomicUsize::new(0));
    let mut engine = files.engine();
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    // The file compiles with the script; each replacement is parsed cold
    // when the call requires it.
    files.write("deep.vibe", "nil");
    let script = engine.compile("require(\"deep\")").unwrap();
    for (prefix, suffix) in [
        ("(", ")"),
        ("[", "]"),
        ("{a:", "}"),
        ("!", ""),
        ("1 ** ", ""),
        ("if true then ", " end"),
        ("case 1; when 1; ", "; end"),
        ("zero {", "}"),
        ("zero do\n", "\nend"),
        ("plain(", ")"),
        ("C.new.take(options:", ")"),
        ("C.new&.take(options:", ")"),
        ("true ? ", " : 0"),
        ("true ? 0 : ", ""),
    ] {
        files.write(
            "deep.vibe",
            &format!("effect();{}1{}", prefix.repeat(1100), suffix.repeat(1100)),
        );
        engine.clear_module_cache();
        let mut options = CallOptions::default();
        options.limits.steps = None;
        assert_eq!(
            script.run(options).unwrap_err().kind,
            ErrorKind::Syntax,
            "{prefix}"
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{prefix}");
    }
}

#[test]
fn require_permission_is_per_call_and_engine_modes_are_snapshotted() {
    let files = Files::new();
    files.write("answer.vibe", "effect();def value -> int;42;end");
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let source = "def run(*, add: int = 0) -> int;require(\"answer\").value+add;end";
    let permissive = engine.compile(source).unwrap();
    assert_eq!(
        permissive
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(42)
    );
    engine.set_strict_effects(true);
    let restricted = engine.compile(source).unwrap();
    engine.set_strict_effects(false);
    let later = engine.compile(source).unwrap();
    // Every call keeps using the module compiled before its file went away.
    fs::remove_file(files.0.join("answer.vibe")).unwrap();
    let mut expected = 1;
    for (script, strict) in [(&permissive, false), (&restricted, true), (&later, false)] {
        for allow in [false, true, false] {
            let result = script.call_with_keywords(
                "run",
                &[],
                &[("add".into(), Value::int(1))],
                CallOptions {
                    allow_require: allow,
                    ..CallOptions::default()
                },
            );
            if strict && !allow {
                require_denied(&result.unwrap_err());
            } else {
                assert_eq!(result.unwrap().value.as_int(), Some(43));
                expected += 1;
            }
            assert_eq!(effects.load(Ordering::SeqCst), expected);
        }
    }
    common::scope(|scope| {
        let mut calls = Vec::new();
        for allow in [false, true, true, false, false, true] {
            let script = restricted.clone();
            calls.push(scope.spawn(move || {
                let result = script.call(
                    "run",
                    &[],
                    CallOptions {
                        allow_require: allow,
                        ..CallOptions::default()
                    },
                );
                if allow {
                    assert_eq!(result.unwrap().value.as_int(), Some(42));
                } else {
                    require_denied(&result.unwrap_err());
                }
            }));
        }
        for call in calls {
            call.join().unwrap();
        }
    });
    assert_eq!(effects.load(Ordering::SeqCst), expected + 3);
}

#[test]
fn denied_require_does_not_inspect_initialize_or_cache_modules() {
    let files = Files::new();
    let valid = "effect();def value -> int;1;end";
    files.write("allowed.vibe", "effect();def value -> int;2;end");
    for development in [false, true] {
        let effects = Arc::new(AtomicUsize::new(0));
        let captured = effects.clone();
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        engine
            .set_module_config(ModuleConfig {
                paths: vec![files.0.clone()],
                cache_limit: 1,
                development,
                ..ModuleConfig::default()
            })
            .unwrap();
        engine.register("effect", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        // Each file is valid when the program compiles and is then replaced,
        // so a denied call that inspected it would fail differently.
        for name in ["blocked", "invalid", "bytes", "directory", "missing"] {
            let path = files.0.join(format!("{name}.vibe"));
            if path.is_dir() {
                fs::remove_dir(&path).unwrap();
            }
            files.write(&format!("{name}.vibe"), valid);
            for expression in [
                format!("require(\"{name}\")"),
                format!("require(\"{name}\",as:\"Alias\")"),
            ] {
                let script = engine.compile(&expression).unwrap();
                match name {
                    "invalid" => files.write("invalid.vibe", "def"),
                    "bytes" => fs::write(&path, [0xff]).unwrap(),
                    "directory" => {
                        fs::remove_file(&path).unwrap();
                        fs::create_dir(&path).unwrap();
                    }
                    "missing" => fs::remove_file(&path).unwrap(),
                    _ => {}
                }
                engine.clear_module_cache();
                let error = script.run(CallOptions::default()).unwrap_err();
                require_denied(&error);
                assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
                if path.is_dir() {
                    fs::remove_dir(&path).unwrap();
                }
                files.write(&format!("{name}.vibe"), valid);
            }
        }
        // Unresolvable modules and computed arguments do not compile.
        for expression in [
            "require(\"../escape\")",
            "require(1)",
            "require(\"blocked\",as:123)",
            "require(\"blocked\",unknown:true)",
        ] {
            let error = engine.compile(expression).err().expect(expression);
            assert!(!common::codes(&error).is_empty(), "{expression}: {error}");
        }
        let result = engine
            .compile("require(\"allowed\").value")
            .unwrap()
            .run(CallOptions {
                allow_require: true,
                ..CallOptions::default()
            })
            .unwrap();
        assert_eq!(result.value.as_int(), Some(2));
        assert_eq!(effects.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn require_permission_follows_argument_evaluation_and_precedes_builtin_validation() {
    // `require` takes string literals and no block, so a computed name or
    // alias and an attached block are compile errors, not runtime ones.
    let files = Files::new();
    files.write("answer.vibe", "def value -> int;42;end");
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    for name in ["module_name", "alias_name", "effect"] {
        engine.register(name, |ctx, _| ctx.bytes(b"answer"));
    }
    let source = "def run\nrequire(module_name(),as:alias_name()){effect()}\nend";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0305", "V0309", "V0309"]);
    assert_eq!(
        error.diagnostics()[1].span.start,
        source.find("module_name").unwrap()
    );
}

#[test]
fn imported_code_uses_the_receivers_require_permission() {
    // A module held as `any` is never called, so the receiving script
    // requires the file whose functions, methods and module methods require
    // their sibling by relative path.
    let files = Files::new();
    files.write(
        "pkg/main.vibe",
        r#"
def child -> int;require("./child").value;end
class Reader
 def value -> int;require("./child").value;end
end
def reader -> Reader;Reader.new;end
module Tools
 def self.value -> int;require("./child").value;end
end
def tools -> int;Tools.value;end
"#,
    );
    files.write("pkg/child.vibe", "visited();def value -> int;42;end");
    for receiver_strict in [false, true] {
        let mut engine = files.engine();
        engine.set_strict_effects(receiver_strict);
        let visits = Arc::new(AtomicUsize::new(0));
        let captured = visits.clone();
        engine.register("visited", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        for expression in ["m.child", "m.reader.value", "m.tools"] {
            let source = format!("def run -> int;m=require(\"pkg/main\");{expression};end");
            let script = engine.compile(&source).unwrap();
            for allow in [false, true, false] {
                let before = visits.load(Ordering::SeqCst);
                let result = script.call(
                    "run",
                    &[],
                    CallOptions {
                        allow_require: allow,
                        ..CallOptions::default()
                    },
                );
                if receiver_strict && !allow {
                    require_denied(&result.unwrap_err());
                    assert_eq!(visits.load(Ordering::SeqCst), before);
                } else {
                    assert_eq!(result.unwrap().value.as_int(), Some(42), "{expression}");
                    assert_eq!(visits.load(Ordering::SeqCst), before + 1);
                }
            }
        }
    }
}

#[test]
fn require_permission_does_not_override_module_roots_or_policy() {
    let files = Files::new();
    files.write("answer.vibe", "def value -> int;42;end");
    // A module the roots or policy exclude does not resolve when the program
    // compiles.
    for (config, reason) in [
        (ModuleConfig::default(), "module paths not configured"),
        (
            ModuleConfig {
                paths: vec![files.0.clone()],
                deny: vec!["answer".into()],
                ..ModuleConfig::default()
            },
            "denied by policy",
        ),
        (
            ModuleConfig {
                paths: vec![files.0.clone()],
                allow: vec!["other".into()],
                ..ModuleConfig::default()
            },
            "not allowed by policy",
        ),
    ] {
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        engine.set_module_config(config).unwrap();
        let error = engine.compile("require(\"answer\")").err().unwrap();
        assert_eq!(common::codes(&error), ["V0201"], "{reason}");
    }
    // A file that grows past the source limit after the program compiles is
    // refused when the call loads it.
    files.write("answer.vibe", "1");
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            source_limit: 4,
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine.compile("require(\"answer\")").unwrap();
    files.write("answer.vibe", "def value -> int;42;end");
    engine.clear_module_cache();
    let error = script
        .run(CallOptions {
            allow_require: true,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert!(
        error.message.contains("source exceeds maximum size"),
        "{error}"
    );
}

#[test]
fn strict_effects_preserves_explicit_host_and_script_overrides() {
    // `require` always names the builtin, which takes a literal module name,
    // so neither a host function nor a script function replaces it.
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine.register("require", |_, _| Ok(Value::int(77)));
    let source = "require(:ignored)";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0201", "V0309"]);
    assert_eq!(error.diagnostics()[1].span.start, source.find(':').unwrap());
    let source = "def require(n: int) -> int;n+1;end;require(41)";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0210", "V0309"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("require").unwrap()
    );
}

#[test]
fn repeated_require_denials_release_memory_and_obey_execution_limits() {
    let files = Files::new();
    files.write("disabled.vibe", "def value -> int;1;end");
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    let script = engine.compile("def run(n: int) -> int;i=0;while i<n;begin;require(\"disabled\");rescue=>e;raise \"wrong failure\" if !e.message.start_with?(\"strict effects:\");end;i+=1;end;42;end").unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = None;
    options.limits.memory_bytes = Some(64 << 10);
    let first = script
        .call("run", &[Value::int(1)], options.clone())
        .unwrap();
    let repeated = script
        .call("run", &[Value::int(2048)], options.clone())
        .unwrap();
    assert_eq!(repeated.value.as_int(), Some(42));
    assert_eq!(repeated.stats.retained_memory_bytes, 0);
    assert!(repeated.stats.peak_memory_bytes <= first.stats.peak_memory_bytes + 2048);
    let baseline = script
        .call("run", &[Value::int(16)], options.clone())
        .unwrap();
    options.limits.steps = Some(baseline.stats.steps);
    script
        .call("run", &[Value::int(16)], options.clone())
        .unwrap();
    options.limits.steps = Some(baseline.stats.steps - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(16)], options.clone())
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    options.limits.steps = None;
    options.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes);
    script
        .call("run", &[Value::int(16)], options.clone())
        .unwrap();
    options.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
    assert_eq!(
        script
            .call("run", &[Value::int(16)], options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
}

#[test]
fn require_permission_never_masks_cancellation_or_latched_exhaustion() {
    // The module name is a literal, so the host stops the call in the
    // element before the denied `require`.
    let files = Files::new();
    files.write("disabled.vibe", "def value -> int;1;end");
    for cancel in [false, true] {
        let mut engine = files.engine();
        engine.set_strict_effects(true);
        let token = CancellationToken::new();
        let signal = token.clone();
        engine.register("stop", move |ctx, _| {
            if cancel {
                signal.cancel();
            } else {
                let _ = ctx.charge(u64::MAX);
            }
            Ok(Value::nil())
        });
        let effects = Arc::new(AtomicUsize::new(0));
        let captured = effects.clone();
        engine.register("effect", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        let script = engine
            .compile(
                "begin;[stop(),require(\"disabled\")];rescue;effect();ensure;effect();end;effect()",
            )
            .unwrap();
        let error = script
            .run(CallOptions {
                cancellation: token,
                ..CallOptions::default()
            })
            .unwrap_err();
        assert_eq!(
            error.kind,
            if cancel {
                ErrorKind::Cancelled
            } else {
                ErrorKind::Steps
            }
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn tokio_calls_keep_require_permission_independent() {
    use vibescript::asynchronous::Runner;
    let files = Files::new();
    files.write("answer.vibe", "def value -> int;42;end");
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    let script = engine
        .compile("def run(*, add: int = 0) -> int;require(\"answer\").value+add;end")
        .unwrap();
    let runner = Runner::new(1).unwrap();
    for allow in [false, true, false] {
        let result = runner
            .call_with_keywords(
                script.clone(),
                "run".into(),
                vec![],
                vec![("add".into(), Value::int(1))],
                CallOptions {
                    allow_require: allow,
                    ..CallOptions::default()
                },
            )
            .await;
        if allow {
            assert_eq!(result.unwrap().value.as_int(), Some(43));
        } else {
            require_denied(&result.unwrap_err());
        }
        assert_eq!(runner.available_slots(), 1);
    }
}

#[test]
fn required_diagnostics_follow_the_source_at_each_call_site() {
    let files = Files::new();
    files.write(
        "pkg/main.vibe",
        "inner=require(\"./inner\")\ndef fail\n inner.fail\nend",
    );
    files.write("pkg/inner.vibe", "def fail\n 1//0\nend");
    let script = files
        .engine()
        .compile("def run\n require(\"pkg/main\").fail\nend")
        .unwrap();
    let error = script.call("run", &[], CallOptions::default()).unwrap_err();
    let diagnostic = error.diagnostic.as_ref().unwrap();
    assert_eq!(
        diagnostic.filename.as_deref(),
        Some(b"pkg/inner.vibe".as_slice())
    );
    assert_eq!(diagnostic.position, Position { line: 2, column: 3 });
    assert_eq!(
        diagnostic.code_frame,
        "  --> pkg/inner.vibe:2:3\n 2 |  1//0\n   |   ^"
    );
    assert_eq!(
        diagnostic
            .frames
            .iter()
            .map(|frame| (
                &*frame.function,
                frame.filename.as_deref(),
                frame.position.line,
                frame.position.column,
            ))
            .collect::<Vec<_>>(),
        [
            ("fail", Some(b"pkg/inner.vibe".as_slice()), 2, 3),
            ("fail", Some(b"pkg/main.vibe".as_slice()), 3, 2),
            ("fail", None, 2, 2),
            ("run", None, 1, 1),
        ]
    );
    assert!(error.to_string().contains("at fail (pkg/main.vibe:3:2)"));
    assert!(Arc::ptr_eq(
        diagnostic.filename.as_ref().unwrap(),
        diagnostic.frames[0].filename.as_ref().unwrap(),
    ));
}

#[test]
fn required_parse_and_initializer_failures_identify_the_failed_file() {
    let files = Files::new();
    files.write("pkg/main.vibe", "require(\"./broken\")");
    // The files compile with the scripts and are then replaced, so each call
    // compiles the replacement cold. A replacement that does not parse is
    // required directly: through `pkg/main` it fails that file's check.
    files.write("pkg/broken.vibe", "nil");
    let engine = files.engine();
    let direct = engine.compile("require(\"pkg/broken\")").unwrap();
    let script = engine.compile("require(\"pkg/main\")").unwrap();
    files.write("pkg/broken.vibe", "def fail\n $\nend");
    engine.clear_module_cache();
    let error = direct.run(CallOptions::default()).unwrap_err();
    let diagnostic = error.diagnostic.as_ref().unwrap();
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert_eq!(
        diagnostic.filename.as_deref(),
        Some(b"pkg/broken.vibe".as_slice())
    );
    assert_eq!(diagnostic.position, Position { line: 2, column: 2 });
    assert!(diagnostic.frames.is_empty());
    assert!(
        error
            .to_string()
            .starts_with("parse error at pkg/broken.vibe:2:2:")
    );

    for source in ["1%0", "class C\n X=1%0\nend", "module C\n X=1%0\nend"] {
        files.write("pkg/broken.vibe", source);
        engine.clear_module_cache();
        let error = script.run(CallOptions::default()).unwrap_err();
        let diagnostic = error.diagnostic.as_ref().unwrap();
        assert_eq!(error.kind, ErrorKind::Arithmetic);
        assert_eq!(
            diagnostic.filename.as_deref(),
            Some(b"pkg/broken.vibe".as_slice())
        );
        assert_eq!(error.offset, Some(source.find('%').unwrap()));
        assert_eq!(diagnostic.frames[0].filename, diagnostic.filename);
        assert!(!error.to_string().contains("__main__"));
    }
}

#[test]
fn required_parse_errors_are_rescuable_without_caching_failed_sources() {
    // The file compiles with the script and is then broken, so each call
    // parses it cold.
    let files = Files::new();
    files.write("broken.vibe", "def answer -> int;42;end");
    let engine = files.engine();
    let script = engine
        .compile("def run -> [int, array<string>];events: array<string> = [];value=begin;require(\"broken\").answer;rescue=>e;events.push(e.class);7;ensure;events.push(\"ensure\");end;[value,events];end")
        .unwrap();
    files.write("broken.vibe", "def answer(");
    engine.clear_module_cache();
    for _ in 0..2 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([7, ["RuntimeError", "ensure"]])
        );
    }
    files.write("broken.vibe", "def answer -> int;42;end");
    assert_eq!(
        json(
            &script
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([42, ["ensure"]])
    );
}

#[test]
fn rescued_required_syntax_keeps_its_origin_and_obeys_receiving_limits() {
    let files = Files::new();
    // The file compiles with the scripts and is then broken, so each call
    // parses it cold.
    files.write("broken.vibe", "nil");
    let engine = files.engine();
    assert_eq!(engine.compile("def answer(").err().unwrap().class(), None);
    let reraise = engine
        .compile("begin;require(\"broken\");rescue;raise;end")
        .unwrap();
    let script = engine
        .compile("begin;require(\"broken\");rescue RuntimeError=>e;[e.class,e.code_frame.include?(\"broken.vibe\"),e.backtrace.empty?];end")
        .unwrap();
    files.write("broken.vibe", "def answer(");
    engine.clear_module_cache();
    let error = reraise.run(CallOptions::default()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert_eq!(error.class(), Some(ErrorClass::Runtime));
    assert_eq!(
        error.diagnostic.as_ref().unwrap().filename.as_deref(),
        Some(b"broken.vibe".as_slice())
    );
    assert!(error.diagnostic.as_ref().unwrap().frames.is_empty());
    let baseline = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&baseline.value),
        serde_json::json!(["RuntimeError", true, true])
    );
    for memory in [false, true] {
        for shortage in [0, 1] {
            let mut options = CallOptions::default();
            if memory {
                options.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - shortage);
            } else {
                options.limits.steps = Some(baseline.stats.steps - shortage as u64);
            }
            let result = script.run(options);
            if shortage == 0 {
                assert_eq!(json(&result.unwrap().value), json(&baseline.value));
            } else {
                assert_eq!(
                    result.unwrap_err().kind,
                    if memory {
                        ErrorKind::Memory
                    } else {
                        ErrorKind::Steps
                    }
                );
            }
        }
    }
}

#[test]
fn required_binding_defaults_and_blocks_keep_their_expression_origins() {
    let files = Files::new();
    files.write(
        "calls.vibe",
        "def typed(n:int);n;end\ndef default(n: any =1//0) -> any;n;end\ndef invoke(&block: () -> any) -> any;yield;end",
    );
    // A wrong argument fails the cast at the call site, where the parameter
    // check failed before.
    for (expression, file, line) in [
        ("m.typed(JSON.parse(\"\\\"bad\\\"\").as(int))", None, 3),
        ("m.default", Some(b"calls.vibe".as_slice()), 2),
        ("m.invoke{1//0}", None, 3),
    ] {
        let source = format!("def run -> any\n m=require(\"calls\")\n {expression}\nend");
        let error = files
            .engine()
            .compile(&source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        let diagnostic = error.diagnostic.as_ref().unwrap();
        assert_eq!(
            diagnostic.filename.as_deref(),
            file,
            "{expression}: {error}"
        );
        assert_eq!(diagnostic.position.line, line, "{expression}: {error}");
    }
    // A required file whose function returns the wrong type does not
    // compile, and the diagnostic names that file.
    files.write("returns.vibe", "def returned -> int;\"bad\";end");
    let engine = files.engine();
    let error = engine
        .compile("def run -> any\n m=require(\"returns\")\n m.returned\nend")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0101"]);
    assert_eq!(
        error.diagnostics()[0].file.as_deref(),
        Some(b"returns.vibe".as_slice())
    );
}

#[test]
fn rescued_required_errors_keep_named_snippets_and_traces_through_reraise() {
    let files = Files::new();
    files.write("pkg/failure.vibe", "def fail\n 1%0\nend");
    let script = files
        .engine()
        .compile(
            "m=require(\"pkg/failure\");begin\nm.fail\nrescue=>e\n[e.code_frame,e.backtrace]\nend",
        )
        .unwrap();
    // Compare budgets after warming the shared compilation cache.
    script.run(CallOptions::default()).unwrap();
    let output = script.run(CallOptions::default()).unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([
            "  --> pkg/failure.vibe:2:3\n 2 |  1%0\n   |   ^",
            ["pkg/failure.vibe:2:3:in `fail`", "2:1:in `fail`"],
        ])
    );
    let mut options = CallOptions::default();
    options.limits.steps = Some(output.stats.steps);
    script.run(options.clone()).unwrap();
    options.limits.steps = Some(output.stats.steps - 1);
    assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Steps);
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
    script.run(options.clone()).unwrap();
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Memory);

    let source = "m=require(\"pkg/failure\");begin\nm.fail\nrescue\nraise\nensure\n1\nend";
    let error = files
        .engine()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(
        error.diagnostic.as_ref().unwrap().filename.as_deref(),
        Some(b"pkg/failure.vibe".as_slice())
    );
    assert_eq!(
        error.diagnostic.as_ref().unwrap().position,
        Position { line: 2, column: 3 }
    );
}

#[test]
fn imported_module_errors_keep_original_filenames_without_retaining_host_state() {
    // A module held as `any` is never called, so the script that requires
    // the module calls it, and both are dropped before the error is read.
    let files = Files::new();
    files.write("original/module.vibe", "def fail\n host()\nend");
    let state = Arc::new(AtomicUsize::new(0));
    let captured = state.clone();
    let mut engine = files.engine();
    engine.register("host", move |_, _| {
        captured.fetch_add(1, Ordering::Relaxed);
        Err(vibescript::Error::new(ErrorKind::Host, "host failed"))
    });
    let receiver = engine
        .compile("def run\n require(\"original/module\").fail\nend")
        .unwrap();
    drop(engine);
    let error = receiver
        .call("run", &[], CallOptions::default())
        .unwrap_err();
    drop(receiver);
    assert_eq!(state.load(Ordering::Relaxed), 1);
    assert_eq!(Arc::strong_count(&state), 1);
    let diagnostic = error.diagnostic.as_ref().unwrap();
    assert_eq!(
        diagnostic.filename.as_deref(),
        Some(b"original/module.vibe".as_slice())
    );
    assert_eq!(diagnostic.position, Position { line: 2, column: 2 });
    assert_eq!(diagnostic.frames[1].filename, None);
    assert!(
        error
            .to_string()
            .contains("at fail (original/module.vibe:2:2)")
    );
}

#[test]
fn module_exports_share_private_state_and_reset_each_call() {
    let files = Files::new();
    files.write(
        "counter.vibe",
        r#"
x=0
private def hidden -> int;99;end
def add(n: int) -> int;x+=n;x;end
export def zero -> int;7;end
enum Status
 Ready
end
class Internal
end
"#,
    );
    let script = files
        .engine()
        .compile(
            r#"def run -> array<int | string>
 m=require("counter",as: "Counter")
 [m.add(2),Counter.add(3),add(4),m.zero,m.Status::Ready.name,Status::Ready.name]
end
def exports -> any
 require("counter")
end"#,
        )
        .unwrap();
    for _ in 0..2 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([2, 5, 9, 7, "Ready", "Ready"])
        );
    }
    // Scripts never enumerate a module, so the host reads its exports.
    let module = script
        .call("exports", &[], CallOptions::default())
        .unwrap()
        .value;
    let names: Vec<_> = module
        .as_hash()
        .unwrap()
        .iter()
        .map(|(name, _)| String::from_utf8_lossy(name.as_bytes().unwrap()).into_owned())
        .collect();
    assert_eq!(names, ["Status", "add", "zero"]);
}

#[test]
fn indexed_scoped_and_symbolic_calls_keep_the_module_target() {
    let files = Files::new();
    files.write(
        "calls.vibe",
        "x=0\ndef add(n: int = 1) -> int;x+=n;x;end\ndef apply(n: int, *, extra: int = 2, &block: int -> int) -> int;yield(n+extra);end",
    );
    let engine = files.engine();
    let output = engine
        .compile("m=require(\"calls\");[m.add(2),m.add]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([2, 3]));
    let output = engine
        .compile("m=require(\"calls\");m.apply(3,extra:4){|n|n*2}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(output.value.as_int(), Some(14));
    // A module is called only by name with a dot.
    for (expression, code) in [
        ("m::add(2)", "V0416"),
        ("m[\"add\"](2)", "V0112"),
        ("m.fetch(\"add\")(2)", "V0203"),
        ("m.send(:add,2)", "V0405"),
        ("m.public_send(:add,2)", "V0405"),
        ("m::apply(3,extra:4){|n|n*2}", "V0416"),
        ("m.public_send(:apply,3,extra:4){|n|n*2}", "V0405"),
    ] {
        let source = format!("m=require(\"calls\");{expression}");
        let error = engine.compile(&source).err().expect(expression);
        assert_eq!(common::codes(&error)[0], code, "{expression}: {error}");
    }
}

#[test]
fn exported_functions_cannot_be_extracted_stored_passed_or_returned() {
    let files = Files::new();
    files.write(
        "functions.vibe",
        "def fn(n: any) -> any;n;end\ndef zero -> int;7;end",
    );
    // Static types never index, enumerate, splat or scope a module, so each
    // escape is refused at compile time.
    let mut refusing = files.engine();
    refusing.register("effect", |_, _| panic!("effect ran"));
    for expression in [
        "m::fn",
        "m::zero",
        "effect(m::fn)",
        "m[\"fn\"]",
        "m[\"zero\"]",
        "m.fetch(\"fn\")",
        "m.dig(\"fn\")",
        "m.values",
        "(m.values)(effect())",
        "(m.fetch_values(\"fn\"))(effect())",
        "m.values_at(\"fn\")",
        "m.fetch_values(\"fn\")",
        "x=m[\"fn\"];x(1)",
        "[m[\"fn\"]]",
        "{value:m[\"fn\"]}",
        "effect(m[\"fn\"])",
        "m.each_value{|fn|effect(fn)}",
        "m.each{|key,fn|effect(fn)}",
        "m.each_value{effect()}",
        "for key,fn in m;effect(fn);end",
        "effect(**m)",
        "m[\"fn\"].call(1)",
    ] {
        let source = format!("m=require(\"functions\");{expression};effect()");
        let error = refusing.compile(&source).err().expect(expression);
        assert!(!common::codes(&error).is_empty(), "{expression}: {error:?}");
    }
}

#[test]
fn aliases_reuse_a_module_but_reject_conflicts_before_initialization() {
    let files = Files::new();
    files.write("module.vibe", "effect();def value -> int;7;end");
    let mut engine = files.engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let output = engine
        .compile("a=require(\"module\",as: \"M\");b=require(\"module\",as: \"M\");[a.value,b.value,M.value]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([7, 7, 7]));
    assert_eq!(effects.swap(0, Ordering::SeqCst), 1);
    for source in [
        "M: int? = nil;require(\"module\",as: \"M\")",
        "def M -> int;1;end;require(\"module\",as: \"M\")",
        "require(\"module\",as: \"Math\")",
        "require(\"module\",as: \"effect\")",
        "def run(M: int) -> any;require(\"module\",as: \"M\");end;run(1)",
        "module Scope;M=1;def self.load -> any;require(\"module\",as: \"M\");end;end;Scope.load",
    ] {
        let mut engine = files.engine();
        let captured = effects.clone();
        engine.register("effect", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        let error = engine.compile(source).err().unwrap();
        assert_eq!(common::codes(&error), ["V0209"], "{source}: {error:?}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{source}");
    }
}

#[test]
fn failed_initialization_is_retryable_and_does_not_publish_exports() {
    let files = Files::new();
    files.write("failure.vibe", "attempt();def exported -> int;7;end");
    let mut engine = files.engine();
    let attempts = Arc::new(AtomicUsize::new(0));
    let captured = attempts.clone();
    engine.register("attempt", move |_, _| {
        if captured.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(vibescript::Error::new(ErrorKind::Runtime, "first attempt"))
        } else {
            Ok(Value::nil())
        }
    });
    let output = engine
        .compile(
            r#"first=begin;require("failure",as: "M");rescue;true;end
missing=begin;exported();rescue;true;end
m=require("failure",as: "M")
[first,missing,m.exported,M.exported,exported()]"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(
        json(&output.value),
        serde_json::json!([true, true, 7, 7, 7])
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn host_transfers_cannot_admit_detached_exported_functions() {
    let files = Files::new();
    files.write("exports.vibe", "def fn(n: any);n;end");
    let module = files
        .engine()
        .compile("require(\"exports\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let function = module.as_hash().unwrap()[0].1.clone();
    let mut engine = Engine::new();
    let returned = function.clone();
    engine.register("detached", move |_, _| Ok(returned.clone()));
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let receiver = engine
        .compile("def run(m: any) -> any;effect();m;end")
        .unwrap();
    for value in [
        function.clone(),
        Value::array(vec![function.clone()]),
        Value::hash(vec![(b"fn".to_vec(), function)]),
    ] {
        let error = receiver
            .call("run", &[value], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
    // Static types never call a host's result, so calling the detached
    // function is refused at compile time.
    let mut refusing = vibescript::Engine::new();
    refusing.register("detached", |_, _| panic!("detached ran"));
    refusing.register("effect", |_, _| panic!("effect ran"));
    let error = refusing.compile("detached()(1);effect()").err().unwrap();
    assert_eq!(common::codes(&error), ["V0106"]);
    assert_eq!(error.diagnostics()[0].span.start, 0);
    for expression in ["detached()", "effect(detached())"] {
        let source = format!("{expression};effect()");
        let error = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{expression}: {error:?}");
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn failed_initialization_releases_unreachable_private_values() {
    let files = Files::new();
    for (name, source) in [
        ("locals", "payload=\"x\"*16384;raise \"failed\""),
        (
            "namespaces",
            "enum State;Ready;end;module Outer;PAYLOAD=\"x\"*16384;module Inner;VALUES=[1,2,3];end;end;raise \"failed\"",
        ),
        (
            "instances",
            "class Box;@payload: string;def initialize;@payload=\"x\"*16384;end;end;item=Box.new;raise \"failed\"",
        ),
    ] {
        files.write("failure.vibe", source);
        let script = files
            .engine()
            .compile("def run(n: int);n.times{begin;require(\"failure\");rescue;nil;end};nil;end")
            .unwrap();
        let counts: &[i64] = if name == "instances" {
            &[100, 1000, 5000]
        } else {
            &[100, 1000]
        };
        for &count in counts {
            let mut options = CallOptions::default();
            options.limits.steps = None;
            options.limits.memory_bytes = Some(1 << 20);
            let output = script
                .call("run", &[Value::int(count)], options)
                .unwrap_or_else(|error| panic!("{name}/{count}: {error:?}"));
            assert_eq!(output.stats.retained_memory_bytes, 0, "{name}/{count}");
        }
    }
}

#[test]
fn cached_compilation_preserves_each_scripts_registered_callbacks() {
    let files = Files::new();
    files.write("host.vibe", "def value -> int;host_value().as(int);end");
    let mut engine = files.engine();
    engine.register("host_value", |_, _| Ok(Value::int(1)));
    let first = engine.compile("require(\"host\").value").unwrap();
    engine.register("host_value", |_, _| Ok(Value::int(2)));
    let second = engine.compile("require(\"host\").value").unwrap();
    for (script, expected) in [(&second, 2), (&first, 1), (&second, 2), (&first, 1)] {
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(expected)
        );
    }
}

#[test]
fn cold_compilation_inherits_mixed_hosts_through_nested_requires() {
    let files = Files::new();
    files.write(
        "leaf.vibe",
        "def value -> array<int>;[alpha().as(int),middle(zeta()){ |n| n.as(int)+1 }.as(int),zeta().as(int)];end",
    );
    files.write(
        "parent.vibe",
        "def value -> array<int>;require(\"leaf\").value;end",
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let mut engine = files.engine();
    for (name, value) in [("zeta", 30), ("alpha", 10)] {
        let calls = calls.clone();
        engine.register(name, move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Value::int(value))
        });
    }
    engine.register_method(
        "middle",
        vibescript::HostMethod::new_with_block("middle", |call, args, _| call.call_block(args)),
    );
    let source = "require(\"parent\").value";
    let original = engine.compile(source).unwrap();
    engine.register("alpha", |_, _| Ok(Value::int(100)));
    engine.register("zeta", |_, _| Ok(Value::int(300)));
    let replacement = engine.compile(source).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for _ in 0..2 {
        assert_eq!(
            json(&original.run(CallOptions::default()).unwrap().value),
            serde_json::json!([10, 31, 30])
        );
        assert_eq!(
            json(&replacement.run(CallOptions::default()).unwrap().value),
            serde_json::json!([100, 301, 300])
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 6);
}

#[test]
fn alias_conflicts_are_rejected_without_running_initializers() {
    let files = Files::new();
    files.write(
        "rejected.vibe",
        "enum State;Ready;end;def value -> int;1;end;effect()",
    );
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let error = engine
        .compile("def taken -> int;7;end;def run(n: int) -> int;n.times{begin;require(\"rejected\",as: \"taken\");rescue;nil;end};7;end")
        .err()
        .unwrap();
    assert_eq!(common::codes(&error), ["V0209"]);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_parents_preserve_completed_dependencies_when_scope_slots_are_reused() {
    // A cycle does not compile, so the parent fails in a dependency's body.
    let files = Files::new();
    files.write(
        "stable.vibe",
        "effect();count=0;def add -> int;count+=1;count;end",
    );
    files.write(
        "a.vibe",
        "require(\"stable\");payload=\"x\"*256;require(\"b\")",
    );
    files.write("b.vibe", "raise \"failed\"");
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile(
            "200.times{begin;require(\"a\");rescue;nil;end};m=require(\"stable\");[m.add,m.add]",
        )
        .unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = None;
    options.limits.memory_bytes = Some(256 << 10);
    let output = script.run(options).unwrap();
    assert_eq!(json(&output.value), serde_json::json!([1, 2]));
    assert_eq!(effects.load(Ordering::SeqCst), 1);
}

#[test]
fn cache_modes_clear_and_pins_keep_per_call_compilation_consistent() {
    let files = Files::new();
    for development in [false, true] {
        files.write("cache.vibe", "def value -> int;1;end");
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![files.0.clone()],
                development,
                ..ModuleConfig::default()
            })
            .unwrap();
        let script = engine.compile("require(\"cache\").value").unwrap();
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(1)
        );
        files.write("cache.vibe", "def value -> int;222;end");
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(if development { 222 } else { 1 })
        );
        engine.clear_module_cache();
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(222)
        );
    }
    files.write("cache.vibe", "def value -> int;1;end");
    let mut engine = files.engine();
    let cache_file = files.0.join("cache.vibe");
    let holder = Arc::new(std::sync::OnceLock::<std::sync::Weak<Engine>>::new());
    let captured = holder.clone();
    engine.register("replace_source", move |_, _| {
        fs::write(&cache_file, "def value -> int;222;end").unwrap();
        captured
            .get()
            .unwrap()
            .upgrade()
            .unwrap()
            .clear_module_cache();
        Ok(Value::nil())
    });
    let engine = Arc::new(engine);
    holder.set(Arc::downgrade(&engine)).unwrap();
    let script = engine
        .compile(
            "a=require(\"cache\");replace_source();b=require(\"cache.vibe\");[a.value,b.value]",
        )
        .unwrap();
    assert_eq!(
        json(&script.run(CallOptions::default()).unwrap().value),
        serde_json::json!([1, 1])
    );
    assert_eq!(
        engine
            .compile("require(\"cache\").value")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(222)
    );
}

#[test]
fn relative_imports_policy_and_cycle_diagnostics_use_file_origins() {
    let files = Files::new();
    files.write(
        "package/main.vibe",
        "m=require(\"./child\");def value -> int;m.value;end",
    );
    files.write("package/child.vibe", "def value -> int;7;end");
    let engine = files.engine();
    assert_eq!(
        engine
            .compile("require(\"package/main\").value")
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    // A cycle does not compile; the diagnostic names the file that closes it.
    files.write("a.vibe", "require(\"b\")");
    files.write("b.vibe", "require(\"a\")");
    let error = engine.compile("require(\"a\")").err().unwrap();
    assert_eq!(common::codes(&error), ["V0201"]);
    assert_eq!(
        error.diagnostics()[0].file.as_deref(),
        Some(b"b.vibe".as_slice())
    );
    // A relative require the policy denies does not resolve, and the
    // diagnostic names the requiring file.
    let mut denied = Engine::new();
    denied
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            deny: vec!["package/child".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let error = denied.compile("require(\"package/main\")").err().unwrap();
    // The unresolved module is `any`, so reading its export is refused too.
    assert_eq!(common::codes(&error), ["V0201", "V0106"]);
    assert_eq!(
        error.diagnostics()[0].file.as_deref(),
        Some(b"package/main.vibe".as_slice())
    );
}

#[test]
fn exported_calls_observe_receiving_budgets_and_cancellation() {
    // A module held as `any` is never called, so the receiving script
    // requires the module whose function it calls.
    let files = Files::new();
    files.write(
        "work.vibe",
        "def work(n: int) -> int;a: array<int> = [];for i in 1..n;a.push(i);end;a.length;end",
    );
    let receiver = files
        .engine()
        .compile("def run -> int;require(\"work\").work(100);end")
        .unwrap();
    // Compare budgets after warming the shared compilation cache.
    receiver.call("run", &[], CallOptions::default()).unwrap();
    let baseline = receiver.call("run", &[], CallOptions::default()).unwrap();
    assert_eq!(baseline.value.as_int(), Some(100));
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    let mut options = CallOptions::default();
    options.limits.steps = Some(baseline.stats.steps);
    assert!(receiver.call("run", &[], options.clone()).is_ok());
    options.limits.steps = Some(baseline.stats.steps - 1);
    assert_eq!(
        receiver.call("run", &[], options).unwrap_err().kind,
        ErrorKind::Steps
    );
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
    assert_eq!(
        receiver.call("run", &[], options).unwrap_err().kind,
        ErrorKind::Memory
    );
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        receiver
            .call(
                "run",
                &[],
                CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::Cancelled
    );
}

#[test]
fn required_code_resolves_receiving_declarations_aliases_and_private_assignment() {
    // A required file is checked on its own, so the receiving script's
    // declarations, aliases and locals are unknown to it, and its functions
    // cannot assign a capitalized name.
    let files = Files::new();
    let reader = r#"
def values -> array<any>;[Root.answer,State::Ready.name,Parent.zero];end
def typed(value: Root) -> int;value.answer;end
def shadow -> int;Root=11;Root;end
def local -> int;secret;end
"#;
    files.write("reader.vibe", reader);
    files.write("peer.vibe", "def zero -> int;17;end");
    let error = files
        .engine()
        .compile(
            r#"
class Root
 def self.answer -> int;7;end
end
enum State
 Ready
end
def run -> any
 require("peer",as: "Parent")
 secret=123
 require("reader").values
end
"#,
        )
        .err()
        .unwrap();
    let found: Vec<_> = error
        .diagnostics()
        .iter()
        .map(|diagnostic| {
            assert_eq!(diagnostic.file.as_deref(), Some(b"reader.vibe".as_slice()));
            (
                diagnostic.code.to_string(),
                &reader[diagnostic.span.start..diagnostic.span.end],
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            ("V0201".into(), "Root"),
            ("V0201".into(), "State"),
            ("V0201".into(), "Parent"),
            ("V0116".into(), "Root"),
            ("V0102".into(), "Root"),
            ("V0201".into(), "secret"),
        ]
    );
}

#[test]
fn required_environments_and_root_aliases_preserve_captured_negative_indices() {
    // A function cannot read or assign a capitalized top-level name, so only
    // the file's own environment captures the rows.
    let files = Files::new();
    files.write(
        "rows.vibe",
        "rows=[[1]];def change -> [array<int>, array<array<int>>, array<array<int>>];before=rows;x=rows[-1].push((while true;rows.push([9]);break 2;end));[x,rows,before];end",
    );
    let engine = files.engine();
    let script = engine.compile("require(\"rows\").change").unwrap();
    for _ in 0..2 {
        let result = script.run(CallOptions::default()).unwrap();
        assert_eq!(
            json(&result.value),
            serde_json::json!([[1, 2], [[1, 2], [9]], [[1]]])
        );
    }
}

#[test]
fn required_files_read_receiving_globals_and_keep_private_assignments() {
    // The engine declares the globals, which a required file reads like the
    // requiring script, and the file's own top-level variable shadows one.
    let files = Files::new();
    files.write(
        "read.vibe",
        "def read -> array<any>;[payload,helper,Box,Math];end;def change -> array<int>;payload.push(2);payload;end",
    );
    files.write(
        "private.vibe",
        "payload=[7];def read -> array<int>;payload;end;def change;payload.push(8);end",
    );
    let mut engine = files.engine();
    for (name, ty) in [
        ("payload", "array<int>"),
        ("helper", "int"),
        ("Box", "int"),
        ("Math", "int"),
    ] {
        engine.declare_global(name, ty).unwrap();
    }
    let script = engine.compile("def helper -> int;99;end;class Box;end;m=require(\"read\");p=require(\"private\");before=m.read;m.change;p.change;[before,m.read,p.read,payload]").unwrap();
    let input = Value::array(vec![Value::int(1)]);
    let opts = CallOptions {
        globals: [
            ("payload", input.clone()),
            ("helper", Value::int(2)),
            ("Box", Value::int(3)),
            ("Math", Value::int(4)),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect(),
        ..CallOptions::default()
    };
    for _ in 0..2 {
        let result = script.run(opts.clone()).unwrap();
        assert_eq!(
            json(&result.value),
            serde_json::json!([[[1], 2, 3, 4], [[1, 2], 2, 3, 4], [7, 8], [1, 2]])
        );
        assert_eq!(json(&input), serde_json::json!([1]));
    }
}

#[test]
fn receiving_module_aliases_do_not_replace_foreign_static_call_targets() {
    // An instance of another script's class is `any` and never called, so a
    // required file's function and method call their own `helper`.
    let files = Files::new();
    files.write("empty.vibe", "nil");
    files.write(
        "calls.vibe",
        "private def helper -> int;7;end;class C;def run -> array<int>;[helper,helper(*[])];end;end;def make -> C;C.new;end;def run -> array<int>;[helper,helper(*[])];end",
    );
    let receiver = files
        .engine()
        .compile("def run -> array<array<int>>;m=require(\"calls\");require(\"empty\",as: \"helper\");[m.run,m.make.run];end")
        .unwrap();
    assert_eq!(
        json(
            &receiver
                .call("run", &[], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([[7, 7], [7, 7]])
    );
}

#[test]
fn dynamic_root_aliases_support_assignment_nested_writes_and_parameter_shadowing() {
    // A function cannot assign or read a capitalized top-level name, and a
    // hash field is not read with a dot, so a root alias is not rebound.
    let files = Files::new();
    files.write("empty.vibe", "1");
    let engine = files.engine();
    let source = r#"
def change -> int
 A={items:[1],n:2}
 A.items.push(3)
 A.n+=1
end
def read -> any;A;end
require("empty",as: "A")
[change,A]
"#;
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error)[0], "V0102");
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("A={").unwrap()
    );
    // A compound assignment cannot replace a capitalized root binding either.
    let source = "require(\"empty\",as: \"A\")\ndef bump -> int;A+=2;A;end\nbump";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error)[0], "V0102");
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("A+=").unwrap()
    );
}

#[test]
fn required_enums_resolve_global_and_alias_type_annotations() {
    // A required file's enum is a type in the requiring script; an alias
    // names its values but not the type.
    let files = Files::new();
    files.write("state.vibe", "enum State\n Ready\nend");
    let engine = files.engine();
    let output = engine
        .compile(
            r#"
def plain(value: State) -> string;value.name;end
require("state",as: "M")
[plain(State::Ready),plain(M.State::Ready)]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!(["Ready", "Ready"]));
    let source =
        "def namespaced(value: M::State) -> string;value.name;end\nrequire(\"state\",as: \"M\")";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0116"]);
}

#[test]
fn foreign_required_functions_can_find_new_receiving_hosts_and_root_functions() {
    // A required file sees the hosts its engine registers when the script
    // compiles, never the receiving script's functions.
    let files = Files::new();
    files.write(
        "foreign.vibe",
        "def run -> array<any>;[late_host(),shared(1),Math.sqrt(4)];end",
    );
    let mut engine = files.engine();
    engine.register("late_host", |_, _| Ok(Value::int(3)));
    let source = "def shared(n: int) -> int;5;end\nrequire(\"foreign\").run";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0201"]);
    assert_eq!(
        error.diagnostics()[0].file.as_deref(),
        Some(b"foreign.vibe".as_slice())
    );
    files.write(
        "foreign.vibe",
        "def run -> array<any>;[late_host(),Math.sqrt(4)];end",
    );
    let script = engine.compile(source).unwrap();
    engine.register("late_host", |_, _| Ok(Value::int(9)));
    assert_eq!(
        json(&script.run(CallOptions::default()).unwrap().value),
        serde_json::json!([3, 2])
    );
}

#[test]
fn exported_targets_are_selected_before_arguments_change_the_module() {
    // A module's exports are not fields, so no argument can replace one.
    let files = Files::new();
    files.write(
        "selection.vibe",
        "def fn(n: int) -> int;n+1;end\ndef push(n: int, *, extra: int = 0) -> int;n+extra+2;end",
    );
    let engine = files.engine();
    for (expression, at) in [
        ("m.fn(begin;m.fn=7;end)", "fn=7"),
        ("m.push(begin;m.push=7;end)", "push=7"),
        ("m.push(begin;m.push=7;end,extra:3)", "push=7"),
    ] {
        let source = format!("m=require(\"selection\");{expression}");
        let error = engine.compile(&source).err().expect(expression);
        assert_eq!(common::codes(&error), ["V0203"], "{expression}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{expression}"
        );
    }
    let output = engine
        .compile("m=require(\"selection\");[m.fn(7),m.push(7,extra:3)]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([8, 12]));
}

#[test]
fn auto_calls_in_mutation_paths_preserve_returned_collection_values() {
    let files = Files::new();
    files.write(
        "collections.vibe",
        "data=[1];def items -> array<int>;data;end",
    );
    let output = files
        .engine()
        .compile("m=require(\"collections\");m.items.push(2);m.items")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([1]));
}

#[test]
fn namespace_initializers_run_before_file_bodies_once_per_call() {
    let files = Files::new();
    files.write(
        "order.vibe",
        "class Hidden\n event(1)\nend\nevent(2)\ndef value -> int;3;end",
    );
    let mut engine = files.engine();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    engine.register("event", move |_, args| {
        captured.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    let script = engine
        .compile("a=require(\"order\");b=require(\"order\");[a.value,b.value]")
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            json(&script.run(CallOptions::default()).unwrap().value),
            serde_json::json!([3, 3])
        );
    }
    assert_eq!(*events.lock().unwrap(), [1, 2, 1, 2]);
}

#[test]
fn configured_cache_source_limits_and_mid_initializer_cancellation_are_enforced() {
    let files = Files::new();
    files.write("one.vibe", "def value -> int;1;end");
    files.write("two.vibe", "def value -> int;2;end");
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            cache_limit: 1,
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine.compile("require(\"one\");require(\"two\")").unwrap();
    let error = script.run(CallOptions::default()).unwrap_err();
    assert!(error.message.contains("cache limit reached"), "{error:?}");
    // A file grows past the source limit after the script compiles.
    files.write("one.vibe", "1");
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            source_limit: 4,
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine.compile("require(\"one\")").unwrap();
    files.write("one.vibe", "def value -> int;1;end");
    engine.clear_module_cache();
    let error = script.run(CallOptions::default()).unwrap_err();
    assert!(
        error.message.contains("source exceeds maximum size"),
        "{error:?}"
    );
    files.write("cancel.vibe", "stop();effect();def value -> int;1;end");
    let mut engine = files.engine();
    let token = CancellationToken::new();
    let captured = token.clone();
    engine.register("stop", move |_, _| {
        captured.cancel();
        Ok(Value::nil())
    });
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("begin;require(\"cancel\");rescue;effect();ensure;effect();end;effect()")
        .unwrap();
    let error = script
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn required_files_keep_host_block_control_and_error_source_locations() {
    let files = Files::new();
    files.write(
        "worker.vibe",
        "def work(n: int) -> int\n visit(n) { |x| raise \"from block\" if x==0; return x.as(int)+1 }\n 99\nend",
    );
    let captured = Arc::new(Mutex::new(None));
    let seen = captured.clone();
    let method = vibescript::HostMethod::new_with_block("visit", move |call, args, _| {
        let result = call.call_block(args);
        if let Err(error) = &result {
            if error.kind != ErrorKind::ControlFlow {
                *seen.lock().unwrap() = error.diagnostic.clone();
            }
        }
        result
    });
    let mut engine = files.engine();
    engine
        .declare_capability(&vibescript::Capability::from_value("visit", method.value()))
        .unwrap();
    let opts = CallOptions {
        allow_require: true,
        capabilities: vec![vibescript::Capability::new("visit", move |_| {
            Ok(method.value())
        })],
        ..CallOptions::default()
    };
    let script = engine
        .compile("def run(n: int) -> int; require(\"worker\").work(n); end")
        .unwrap();
    assert_eq!(
        script
            .call("run", &[Value::int(3)], opts.clone())
            .unwrap()
            .value
            .as_int(),
        Some(4)
    );
    let error = script.call("run", &[Value::int(0)], opts).unwrap_err();
    let diagnostic = error.diagnostic.unwrap();
    assert_eq!(
        diagnostic.filename.as_deref(),
        Some(b"worker.vibe".as_slice())
    );
    assert_eq!(diagnostic.position.line, 2);
    assert_eq!(captured.lock().unwrap().as_ref(), Some(&diagnostic));
}

#[test]
fn run_bindings_keep_required_modules_but_not_their_published_exports() {
    let files = Files::new();
    files.write(
        "counter.vibe",
        "total = 0\ndef add(n: int) -> int\n  total += n\n  total\nend\n",
    );
    let engine = files.engine();
    let (outcome, bindings) = engine
        .compile("counter = require(\"counter\")\nadd(2)")
        .unwrap()
        .run_bindings(CallOptions::default())
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(2));
    let names: Vec<_> = bindings.keys().map(String::as_str).collect();
    assert_eq!(names, ["counter"]);
    // A module passed back as a global is `any`, which the next input
    // compares but never calls.
    let mut engine = files.engine();
    engine.declare_global("counter", "").unwrap();
    let options = CallOptions {
        globals: bindings,
        ..CallOptions::default()
    };
    let (outcome, bindings) = engine
        .compile("counter != nil")
        .unwrap()
        .run_bindings(options)
        .unwrap();
    assert_eq!(json(&outcome.value), serde_json::json!(true));
    assert!(bindings["counter"].as_hash().is_some());
}

#[test]
fn same_name_calls_in_required_files_skip_the_file_scope() {
    // A call with arguments or a block that assigns a file-scope name of the
    // same name skips that scope and resolves in the receiving root.
    let files = Files::new();
    for (name, source) in [
        ("args", "def helper(x: int) -> int;x;end;helper=helper 2"),
        (
            "parens",
            "def helper(x: int) -> int;x;end\nhelper = helper(1)\n",
        ),
        (
            "block",
            "def helper(&block: () -> int) -> int;yield;end;helper=helper { 3 }",
        ),
        // Without arguments the name reads the nearest binding: the file's
        // function, or a parameter.
        (
            "bare",
            "def helper -> int;1;end;helper=helper;def peek -> int;helper;end",
        ),
        (
            "param",
            "def helper -> int;1;end;def f(helper: int) -> int;helper=helper;helper;end;def peek -> int;f(3);end",
        ),
        (
            "late",
            "def value -> int;1;end;def other -> int;value=value;value;end",
        ),
    ] {
        files.write(&format!("{name}.vibe"), source);
    }
    let engine = files.engine();
    let error = engine
        .compile("require(\"parens\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, "undefined variable helper");
    assert_eq!(
        error.diagnostic.as_ref().unwrap().position,
        Position {
            line: 2,
            column: 10
        }
    );
    for name in ["args", "block"] {
        let error = engine
            .compile(&format!("require(\"{name}\")"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, "undefined variable helper", "{name}");
    }
    let script = engine
        .compile(
            "def helper -> int;2;end\n[require(\"bare\").peek,require(\"param\").peek,require(\"late\").other]",
        )
        .unwrap();
    let value = script.run(CallOptions::default()).unwrap().value;
    assert_eq!(json(&value), serde_json::json!([1, 3, 1]));
}

#[test]
fn required_file_functions_call_before_reading_result_members() {
    let files = Files::new();
    for (name, source) in [
        ("top", "def helper -> int;1;end;x = helper.to_s"),
        ("safe", "def helper -> int;1;end;x = helper&.to_s"),
        (
            "func",
            "def helper -> int;1;end;def peek -> string;helper.to_s;end;x = peek",
        ),
        (
            "method",
            "def helper -> int;1;end;class K;def go -> string;helper.to_s;end;end;x = K.new.go",
        ),
    ] {
        files.write(
            &format!("{name}.vibe"),
            &format!("{source};def result -> string?;x;end"),
        );
    }
    let engine = files.engine();
    for name in ["top", "safe", "func", "method"] {
        let value = engine
            .compile(&format!("require(\"{name}\").result"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(json(&value), serde_json::json!("1"), "{name}");
    }
}

#[test]
fn writes_through_module_function_names_update_their_results() {
    let files = Files::new();
    files.write(
        "m.vibe",
        "def helper -> array<int>;[1];end;def peek -> array<int>;helper.push(2);end",
    );
    let engine = files.engine();
    for (source, expected) in [
        (
            "require(\"m\");helper[0] = 5;helper << 3;helper",
            serde_json::json!([1]),
        ),
        ("require(\"m\");helper.pop", serde_json::json!(1)),
        ("require(\"m\").peek", serde_json::json!([1, 2])),
    ] {
        let value = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(json(&value), expected, "{source}");
    }
}
