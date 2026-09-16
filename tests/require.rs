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
                std::process::id(),
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
        let mut engine = Engine::new();
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
fn required_files_use_the_receiving_calls_capability_grants() {
    let files = Files::new();
    files.write("notify.vibe", "def notify(n); sms.deliver(n); end");
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    let script = engine
        .compile("def run; require(:notify).notify(21); end")
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let method = vibescript::HostMethod::new("sms.deliver", move |_, args, _| {
        observed.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(args[0].as_int().unwrap() * 2))
    });
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
    assert_eq!(error.kind, ErrorKind::Name);
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
fn cold_compilation_obeys_work_limits_without_publishing_or_initializing() {
    let files = Files::new();
    let source = format!(
        "initialized();def unused;{}end;def value;42;end",
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
        .compile("begin;require(:answer).value;rescue;unwound();ensure;unwound();end")
        .unwrap();
    let probe = engine.compile("require(:answer).value").unwrap();
    let mut unlimited = CallOptions::default();
    unlimited.limits.steps = None;
    let cold = script.run(unlimited.clone()).unwrap();
    let warm = script.run(unlimited.clone()).unwrap();
    assert_eq!(cold.value.as_int(), Some(42));
    assert_eq!(warm.value.as_int(), Some(42));
    assert!(cold.stats.steps > warm.stats.steps * 4);
    engine.clear_module_cache();
    initialized.store(0, Ordering::SeqCst);
    unwound.store(0, Ordering::SeqCst);
    let mut limited = unlimited.clone();
    limited.limits.steps = Some(cold.stats.steps / 2);
    assert_eq!(script.run(limited).unwrap_err().kind, ErrorKind::Steps);
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
    let retried = script.run(exact).unwrap();
    assert_eq!(retried.value.as_int(), Some(42));
    assert_eq!(retried.stats.steps, cold.stats.steps);
    assert_eq!(initialized.load(Ordering::SeqCst), 1);
    assert_eq!(unwound.load(Ordering::SeqCst), 1);
    let mut cached = unlimited;
    cached.limits.steps = Some(warm.stats.steps);
    assert_eq!(script.run(cached).unwrap().value.as_int(), Some(42));
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
    let script = engine
        .compile("begin;require(:input);rescue;effect();ensure;effect();end")
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
    files.write(
        "deep.vibe",
        &format!("schema={literal};def value;schema;end"),
    );
    let script = files.engine().compile("require(:deep).value").unwrap();
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
    let script = engine.compile("require(:deep)").unwrap();
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
            &format!("effect();{}1{}", prefix.repeat(300), suffix.repeat(300)),
        );
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
    files.write("answer.vibe", "effect();def value;42;end");
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let source = "def run(add:0);require(:answer).value+add;end";
    let permissive = engine.compile(source).unwrap();
    assert_eq!(
        permissive
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(42)
    );
    fs::remove_file(files.0.join("answer.vibe")).unwrap();
    engine.set_strict_effects(true);
    let restricted = engine.compile(source).unwrap();
    engine.set_strict_effects(false);
    let later = engine.compile(source).unwrap();
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
    std::thread::scope(|scope| {
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
    files.write("blocked.vibe", "effect();def value;1;end");
    files.write("allowed.vibe", "effect();def value;2;end");
    files.write("invalid.vibe", "def");
    fs::write(files.0.join("bytes.vibe"), [0xff]).unwrap();
    fs::create_dir(files.0.join("directory.vibe")).unwrap();
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
        for expression in [
            "require(:blocked)",
            "require(:invalid)",
            "require(:bytes)",
            "require(:directory)",
            "require(:missing)",
            "require(\"../escape\")",
            "require()",
            "require(1)",
            "require(:blocked,:allowed)",
            "require(:blocked,as:123)",
            "require(:blocked,unknown:true)",
            "require(:blocked){effect()}",
        ] {
            let error = engine
                .compile(expression)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            require_denied(&error);
            assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
        }
        let result = engine
            .compile("require(:allowed).value")
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
    let files = Files::new();
    files.write("answer.vibe", "effect();def value;42;end");
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    for (name, label, result) in [
        ("module_name", "name", b"answer".as_slice()),
        ("alias_name", "alias", b"Answer".as_slice()),
    ] {
        let recorded = events.clone();
        engine.register(name, move |ctx, _| {
            recorded.lock().unwrap().push(label);
            ctx.bytes(result)
        });
    }
    for name in ["effect", "rescued", "ensured"] {
        let recorded = events.clone();
        engine.register(name, move |_, _| {
            recorded.lock().unwrap().push(name);
            Ok(Value::nil())
        });
    }
    let script = engine.compile("def run\nbegin\nrequire(module_name(),as:alias_name()){effect()}\nrescue=>e\nrescued();[e.type,e.message]\nensure\nensured()\nend\nend").unwrap();
    for allow in [false, true] {
        events.lock().unwrap().clear();
        let output = script
            .call(
                "run",
                &[],
                CallOptions {
                    allow_require: allow,
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            ["name", "alias", "rescued", "ensured"]
        );
        assert_eq!(
            json(&output.value),
            serde_json::json!([
                if allow {
                    "ArgumentError"
                } else {
                    "RuntimeError"
                },
                if allow {
                    "require does not accept blocks"
                } else {
                    "strict effects: require is disabled without CallOptions.allow_require"
                },
            ])
        );
    }
}

#[test]
fn imported_code_uses_the_receivers_require_permission() {
    let files = Files::new();
    files.write(
        "pkg/main.vibe",
        r#"
def child;require("./child").value;end
def answer;7;end
class Reader
 def value;require("./child").value;end
end
def reader;Reader.new;end
module Tools
 def self.value;require("./child").value;end
end
def tools;Tools;end
"#,
    );
    files.write("pkg/child.vibe", "visited();def value;42;end");
    for producer_strict in [false, true] {
        let mut producer = files.engine();
        producer.set_strict_effects(producer_strict);
        let module = producer
            .compile("require(\"pkg/main\")")
            .unwrap()
            .run(CallOptions {
                allow_require: true,
                ..CallOptions::default()
            })
            .unwrap()
            .value;
        drop(producer);
        for receiver_strict in [false, true] {
            let mut engine = Engine::new();
            engine.set_strict_effects(receiver_strict);
            let visits = Arc::new(AtomicUsize::new(0));
            let captured = visits.clone();
            engine.register("visited", move |_, _| {
                captured.fetch_add(1, Ordering::SeqCst);
                Ok(Value::nil())
            });
            let supplied = module.clone();
            engine.register("provide", move |_, _| Ok(supplied.clone()));
            let read = engine.compile("def run(m);m.answer;end").unwrap();
            assert_eq!(
                read.call("run", std::slice::from_ref(&module), CallOptions::default())
                    .unwrap()
                    .value
                    .as_int(),
                Some(7)
            );
            for expression in [
                "m.child",
                "m.reader.value",
                "m.tools.value",
                "provide().child",
            ] {
                let host = expression.starts_with("provide");
                let source = format!("def run{};{expression};end", if host { "" } else { "(m)" });
                let script = engine.compile(&source).unwrap();
                let args = if host {
                    &[][..]
                } else {
                    std::slice::from_ref(&module)
                };
                for allow in [false, true, false] {
                    let before = visits.load(Ordering::SeqCst);
                    let result = script.call(
                        "run",
                        args,
                        CallOptions {
                            allow_require: allow,
                            ..CallOptions::default()
                        },
                    );
                    if receiver_strict && !allow {
                        let error = result.unwrap_err();
                        require_denied(&error);
                        assert_eq!(
                            error.diagnostic.unwrap().filename.as_deref(),
                            Some(b"pkg/main.vibe".as_slice())
                        );
                        assert_eq!(visits.load(Ordering::SeqCst), before);
                    } else {
                        assert_eq!(result.unwrap().value.as_int(), Some(42), "{expression}");
                        assert_eq!(visits.load(Ordering::SeqCst), before + 1);
                    }
                }
            }
        }
    }
}

#[test]
fn require_permission_does_not_override_module_roots_or_policy() {
    let files = Files::new();
    files.write("answer.vibe", "def value;42;end");
    for (config, message) in [
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
        (
            ModuleConfig {
                paths: vec![files.0.clone()],
                source_limit: 4,
                ..ModuleConfig::default()
            },
            "source exceeds maximum size",
        ),
    ] {
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        engine.set_module_config(config).unwrap();
        let error = engine
            .compile("require(:answer).value")
            .unwrap()
            .run(CallOptions {
                allow_require: true,
                ..CallOptions::default()
            })
            .unwrap_err();
        assert!(error.message.contains(message), "{error}");
    }
}

#[test]
fn strict_effects_preserves_explicit_host_and_script_overrides() {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine.register("require", |_, _| Ok(Value::int(77)));
    let registered = engine.compile("require(:ignored)").unwrap();
    for allow in [false, true] {
        assert_eq!(
            registered
                .run(CallOptions {
                    allow_require: allow,
                    ..CallOptions::default()
                })
                .unwrap()
                .value
                .as_int(),
            Some(77)
        );
    }
    let script = engine
        .compile("def require(n);n+1;end;require(41)")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap().value.as_int(),
        Some(42)
    );
}

#[test]
fn repeated_require_denials_release_memory_and_obey_execution_limits() {
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    let script = engine.compile("def run(n);i=0;while i<n;begin;require(:disabled);rescue=>e;raise \"wrong failure\" unless e.message.start_with?(\"strict effects:\");end;i+=1;end;42;end").unwrap();
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
    for cancel in [false, true] {
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        let token = CancellationToken::new();
        let signal = token.clone();
        engine.register("stop", move |ctx, _| {
            let name = ctx.bytes(b"disabled")?;
            if cancel {
                signal.cancel();
            } else {
                let _ = ctx.charge(u64::MAX);
            }
            Ok(name)
        });
        let effects = Arc::new(AtomicUsize::new(0));
        let captured = effects.clone();
        engine.register("effect", move |_, _| {
            captured.fetch_add(1, Ordering::SeqCst);
            Ok(Value::nil())
        });
        let script = engine
            .compile("begin;require(stop());rescue;effect();ensure;effect();end;effect()")
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
    files.write("answer.vibe", "def value;42;end");
    let mut engine = files.engine();
    engine.set_strict_effects(true);
    let script = engine
        .compile("def run(add:0);require(:answer).value+add;end")
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
        "inner=require(\"./inner\")\ndef fail\n inner.fail()\nend",
    );
    files.write("pkg/inner.vibe", "def fail\n 1/0\nend");
    let script = files
        .engine()
        .compile("def run\n require(\"pkg/main\").fail()\nend")
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
        "  --> pkg/inner.vibe:2:3\n 2 |  1/0\n   |   ^"
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
    files.write("pkg/broken.vibe", "def fail\n $\nend");
    let engine = files.engine();
    let script = engine.compile("require(\"pkg/main\")").unwrap();
    let error = script.run(CallOptions::default()).unwrap_err();
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

    for source in ["1/0", "class C\n X=1/0\nend", "module C\n X=1/0\nend"] {
        files.write("pkg/broken.vibe", source);
        engine.clear_module_cache();
        let error = script.run(CallOptions::default()).unwrap_err();
        let diagnostic = error.diagnostic.as_ref().unwrap();
        assert_eq!(error.kind, ErrorKind::Arithmetic);
        assert_eq!(
            diagnostic.filename.as_deref(),
            Some(b"pkg/broken.vibe".as_slice())
        );
        assert_eq!(error.offset, Some(source.find('/').unwrap()));
        assert_eq!(diagnostic.frames[0].filename, diagnostic.filename);
        assert!(!error.to_string().contains("__main__"));
    }
}

#[test]
fn required_parse_errors_are_rescuable_without_caching_failed_sources() {
    let files = Files::new();
    files.write("broken.vibe", "def answer(");
    let engine = files.engine();
    let script = engine
        .compile("def run;events=[];value=begin;require(:broken).answer;rescue=>e;events.push(e.type);7;ensure;events.push(\"ensure\");end;[value,events];end")
        .unwrap();
    for _ in 0..2 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([7, ["RuntimeError", "ensure"]])
        );
    }
    files.write("broken.vibe", "def answer;42;end");
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
    files.write("broken.vibe", "def answer(");
    let engine = files.engine();
    assert_eq!(engine.compile("def answer(").err().unwrap().class(), None);
    let error = engine
        .compile("begin;require(:broken);rescue;raise;end")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Syntax);
    assert_eq!(error.class(), Some(ErrorClass::Runtime));
    assert_eq!(
        error.diagnostic.as_ref().unwrap().filename.as_deref(),
        Some(b"broken.vibe".as_slice())
    );
    assert!(error.diagnostic.as_ref().unwrap().frames.is_empty());
    let script = engine
        .compile("begin;require(:broken);rescue RuntimeError=>e;[e.type,e.code_frame.include?(\"broken.vibe\"),e.backtrace.empty?];end")
        .unwrap();
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
        "def typed(n:int);n;end\ndef default(n=1/0);n;end\ndef invoke;yield;end\ndef returned -> int;\"bad\";end",
    );
    let engine = files.engine();
    for (expression, file, line) in [
        ("m.typed(\"bad\")", None, 3),
        ("m.returned()", None, 3),
        ("m.default()", Some(b"calls.vibe".as_slice()), 2),
        ("m.invoke{1/0}", None, 3),
    ] {
        let source = format!("def run\n m=require(:calls)\n {expression}\nend");
        let error = engine
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
}

#[test]
fn rescued_required_errors_keep_named_snippets_and_traces_through_reraise() {
    let files = Files::new();
    files.write("pkg/failure.vibe", "def fail\n 1/0\nend");
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
            "  --> pkg/failure.vibe:2:3\n 2 |  1/0\n   |   ^",
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
    let files = Files::new();
    files.write("original/module.vibe", "def fail\n host()\nend");
    let state = Arc::new(AtomicUsize::new(0));
    let captured = state.clone();
    let mut engine = files.engine();
    engine.register("host", move |_, _| {
        captured.fetch_add(1, Ordering::Relaxed);
        Err(vibescript::Error::new(ErrorKind::Host, "host failed"))
    });
    let module = engine
        .compile("require(\"original/module\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    drop(engine);
    let receiver = Engine::new().compile("def run(m)\n m.fail()\nend").unwrap();
    let error = receiver
        .call("run", &[module], CallOptions::default())
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
private def hidden;99;end
def add(n);x+=n;x;end
export def zero;7;end
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
            r#"def run
 m=require("counter",as: :Counter)
 [m.add(2),Counter.add(3),add(4),m.zero,m.Status::Ready.name,Status::Ready.name,m.keys]
end"#,
        )
        .unwrap();
    for _ in 0..2 {
        let output = script.call("run", &[], CallOptions::default()).unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([2, 5, 9, 7, "Ready", "Ready", ["Status", "add", "zero"]])
        );
    }
}

#[test]
fn indexed_scoped_and_symbolic_calls_keep_the_module_target() {
    let files = Files::new();
    files.write(
        "calls.vibe",
        "x=0\ndef add(n=1);x+=n;x;end\ndef apply(n,extra:2);yield(n+extra);end",
    );
    let engine = files.engine();
    for expression in [
        "m.add(2)",
        "m::add(2)",
        "m[:add](2)",
        "m.fetch(:add)(2)",
        "m.send(:add,2)",
        "m.public_send(:add,2)",
    ] {
        let source = format!("m=require(:calls);[{expression},m.add()]");
        let output = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(
            json(&output.value),
            serde_json::json!([2, 3]),
            "{expression}"
        );
    }
    for expression in [
        "m.apply(3,extra:4){|n|n*2}",
        "m[:apply](3,extra:4){|n|n*2}",
        "m::apply(3,extra:4){|n|n*2}",
        "m.public_send(:apply,3,extra:4){|n|n*2}",
    ] {
        let source = format!("m=require(:calls);{expression}");
        let output = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(output.value.as_int(), Some(14), "{expression}");
    }
}

#[test]
fn exported_functions_cannot_be_extracted_stored_passed_or_returned() {
    let files = Files::new();
    files.write("functions.vibe", "def fn(n);n;end\ndef zero;7;end");
    let mut engine = files.engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    for expression in [
        "m[:fn]",
        "m::fn",
        "m[:zero]",
        "m::zero",
        "m.fetch(:fn)",
        "m.dig(:fn)",
        "m.values",
        "(m.values())(effect())",
        "(m.fetch_values(:fn))(effect())",
        "m.values_at(:fn)",
        "m.fetch_values(:fn)",
        "x=m[:fn];x(1)",
        "[m[:fn]]",
        "{value:m[:fn]}",
        "effect(m[:fn])",
        "effect(m::fn)",
        "m.each_value{|fn|effect(fn)}",
        "m.each{|key,fn|effect(fn)}",
        "m.each_value{effect()}",
        "for key,fn in m;effect(fn);end",
        "effect(**m)",
        "m[:fn].call(1)",
    ] {
        let source = format!("m=require(:functions);{expression};effect()");
        let error = engine
            .compile(&source)
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"))
            .run(CallOptions::default())
            .expect_err(expression);
        assert_eq!(error.kind, ErrorKind::Type, "{expression}: {error:?}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}: {error:?}");
        assert!(
            error.message.contains("cannot be used as a value"),
            "{expression}: {error:?}"
        );
    }
}

#[test]
fn aliases_reuse_a_module_but_reject_conflicts_before_initialization() {
    let files = Files::new();
    files.write("module.vibe", "effect();def value;7;end");
    let mut engine = files.engine();
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let output = engine
        .compile("a=require(:module,as: :M);b=require(:module,as: :M);[a.value,b.value,M.value]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([7, 7, 7]));
    assert_eq!(effects.swap(0, Ordering::SeqCst), 1);
    for source in [
        "M=nil;require(:module,as: :M)",
        "def M;1;end;require(:module,as: :M)",
        "require(:module,as: :Math)",
        "require(:module,as: :effect)",
        "def run(M);require(:module,as: :M);end;run(1)",
        "module Scope;M=1;def self.load;require(:module,as: :M);end;end;Scope.load",
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{source}: {error:?}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{source}");
    }
}

#[test]
fn retained_modules_are_isolated_at_call_boundaries() {
    let files = Files::new();
    files.write("counter.vibe", "x=0;def add(n);x+=n;x;end");
    let maker = files.engine().compile("require(:counter)").unwrap();
    let module = maker.run(CallOptions::default()).unwrap().value;
    let receiver = Engine::new()
        .compile("def run(m);[m.add(2),m.add(3)];end")
        .unwrap();
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&module), CallOptions::default())
            .unwrap();
        assert_eq!(json(&output.value), serde_json::json!([2, 5]));
        assert!(output.stats.retained_memory_bytes < 1024);
    }
}

#[test]
fn failed_initialization_is_retryable_and_does_not_publish_exports() {
    let files = Files::new();
    files.write("failure.vibe", "attempt();def exported;7;end");
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
            r#"first=begin;require(:failure,as: :M);rescue;true;end
missing=begin;exported();rescue;true;end
m=require(:failure,as: :M)
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
    files.write("exports.vibe", "def fn(n);n;end");
    let module = files
        .engine()
        .compile("require(:exports)")
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
    let receiver = engine.compile("def run(m);effect();m;end").unwrap();
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
    for expression in ["detached()", "detached()(1)", "effect(detached())"] {
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
            "class Box;def initialize;@payload=\"x\"*16384;end;end;item=Box.new;raise \"failed\"",
        ),
    ] {
        files.write("failure.vibe", source);
        let script = files
            .engine()
            .compile("def run(n);n.times{begin;require(:failure);rescue;nil;end};nil;end")
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
fn escaped_failed_file_state_survives_collection_and_remains_isolated() {
    let files = Files::new();
    files.write("failure.vibe", "payload=\"x\"*256;raise \"failed\"");
    files.write(
        "retained.vibe",
        r#"items=[7]
module Counter
  VALUES=[2]
  module Nested
    def self.read;[3];end
  end
  def self.values;items;end
  def self.add(n);items.push(n);items;end
  def self.identity;Counter;end
end
save(Counter)
raise "failed""#,
    );
    let saved = Arc::new(Mutex::new(None));
    let mut engine = files.engine();
    let captured = saved.clone();
    engine.register("save", move |_, args| {
        *captured.lock().unwrap() = Some(args[0].clone());
        Ok(Value::nil())
    });
    let captured = saved.clone();
    engine.register("take", move |_, _| {
        Ok(captured.lock().unwrap().take().unwrap())
    });
    let collect = "100.times{begin;require(:failure);rescue;nil;end}";
    let source = format!("begin;require(:retained);rescue;nil;end;{collect};m=take();m.add(8);m");
    let module = engine
        .compile(&source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    assert!(saved.lock().unwrap().is_none());
    let receiver = Engine::new()
        .compile("def run(m);[m.values,m.add(9),m::Nested.read,m.identity==m];end;def identity(m);m.identity;end")
        .unwrap();
    let module = receiver
        .call("identity", &[module], CallOptions::default())
        .unwrap()
        .value;
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&module), CallOptions::default())
            .unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([[7, 8], [7, 8, 9], [3], true])
        );
    }
    let mut consumer = files.engine();
    consumer.register("provide", move |_, _| Ok(module.clone()));
    for (expression, expected) in [
        (
            format!("provide().add(begin;{collect};9;end)"),
            serde_json::json!([7, 8, 9]),
        ),
        (
            format!("provide().VALUES[0]+=begin;{collect};5;end"),
            serde_json::json!(7),
        ),
        (
            format!("provide().VALUES.push(begin;{collect};9;end)"),
            serde_json::json!([2, 9]),
        ),
    ] {
        let output = consumer
            .compile(&expression)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(json(&output.value), expected);
    }
}

#[test]
fn cached_compilation_preserves_each_scripts_registered_callbacks() {
    let files = Files::new();
    files.write("host.vibe", "def value;host_value();end");
    let mut engine = files.engine();
    engine.register("host_value", |_, _| Ok(Value::int(1)));
    let first = engine.compile("require(:host).value").unwrap();
    engine.register("host_value", |_, _| Ok(Value::int(2)));
    let second = engine.compile("require(:host).value").unwrap();
    for (script, expected) in [(&second, 2), (&first, 1), (&second, 2), (&first, 1)] {
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(expected)
        );
    }
    let module = engine
        .compile("require(:host)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut receiving = Engine::new();
    receiving.register("host_value", |_, _| Ok(Value::int(99)));
    let receiver = receiving.compile("def run(m);m.value;end").unwrap();
    assert_eq!(
        receiver
            .call("run", &[module], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(2)
    );
}

#[test]
fn alias_rejections_release_unpublished_scopes_without_running_initializers() {
    let files = Files::new();
    files.write(
        "rejected.vibe",
        "enum State;Ready;end;def value;1;end;effect()",
    );
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("def taken;7;end;def run(n);n.times{begin;require(:rejected,as: :taken);rescue;nil;end};taken;end")
        .unwrap();
    for count in [100, 1000] {
        let mut options = CallOptions::default();
        options.limits.steps = None;
        options.limits.memory_bytes = Some(64 << 10);
        let output = script.call("run", &[Value::int(count)], options).unwrap();
        assert_eq!(output.value.as_int(), Some(7));
        assert_eq!(output.stats.retained_memory_bytes, 0);
    }
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_parents_preserve_completed_dependencies_when_scope_slots_are_reused() {
    let files = Files::new();
    files.write("stable.vibe", "effect();count=0;def add;count+=1;count;end");
    files.write("a.vibe", "require(:stable);payload=\"x\"*256;require(:b)");
    files.write("b.vibe", "require(:a)");
    let effects = Arc::new(AtomicUsize::new(0));
    let captured = effects.clone();
    let mut engine = files.engine();
    engine.register("effect", move |_, _| {
        captured.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script = engine
        .compile("200.times{begin;require(:a);rescue;nil;end};m=require(:stable);[m.add,m.add]")
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
        files.write("cache.vibe", "def value;1;end");
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![files.0.clone()],
                development,
                ..ModuleConfig::default()
            })
            .unwrap();
        let script = engine.compile("require(:cache).value").unwrap();
        assert_eq!(
            script.run(CallOptions::default()).unwrap().value.as_int(),
            Some(1)
        );
        files.write("cache.vibe", "def value;222;end");
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
    files.write("cache.vibe", "def value;1;end");
    let mut engine = files.engine();
    let cache_file = files.0.join("cache.vibe");
    let holder = Arc::new(std::sync::OnceLock::<std::sync::Weak<Engine>>::new());
    let captured = holder.clone();
    engine.register("replace_source", move |_, _| {
        fs::write(&cache_file, "def value;222;end").unwrap();
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
        .compile("a=require(:cache);replace_source();b=require(\"cache.vibe\");[a.value,b.value]")
        .unwrap();
    assert_eq!(
        json(&script.run(CallOptions::default()).unwrap().value),
        serde_json::json!([1, 1])
    );
    assert_eq!(
        engine
            .compile("require(:cache).value")
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
        "m=require(\"./child\");def value;m.value;end",
    );
    files.write("package/child.vibe", "def value;7;end");
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
    files.write("a.vibe", "require(:b)");
    files.write("b.vibe", "require(:a)");
    let error = engine
        .compile("require(:a)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Runtime);
    assert!(
        error.message.contains("a.vibe -> b.vibe -> a.vibe"),
        "{error:?}"
    );
    let mut denied = Engine::new();
    denied
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            deny: vec!["package/child".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let error = denied
        .compile("require(\"package/main\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("denied by policy"), "{error:?}");
}

#[test]
fn exported_calls_observe_receiving_budgets_and_cancellation() {
    let files = Files::new();
    files.write(
        "work.vibe",
        "def work(n);a=[];for i in 1..n;a.push(i);end;a.length;end",
    );
    let module = files
        .engine()
        .compile("require(:work)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let receiver = Engine::new().compile("def run(m);m.work(100);end").unwrap();
    let baseline = receiver
        .call("run", std::slice::from_ref(&module), CallOptions::default())
        .unwrap();
    assert_eq!(baseline.value.as_int(), Some(100));
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    let mut options = CallOptions::default();
    options.limits.steps = Some(baseline.stats.steps);
    assert!(
        receiver
            .call("run", std::slice::from_ref(&module), options.clone())
            .is_ok()
    );
    options.limits.steps = Some(baseline.stats.steps - 1);
    assert_eq!(
        receiver
            .call("run", std::slice::from_ref(&module), options)
            .unwrap_err()
            .kind,
        ErrorKind::Steps
    );
    let mut options = CallOptions::default();
    options.limits.memory_bytes = Some(baseline.stats.peak_memory_bytes - 1);
    assert_eq!(
        receiver
            .call("run", std::slice::from_ref(&module), options)
            .unwrap_err()
            .kind,
        ErrorKind::Memory
    );
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        receiver
            .call(
                "run",
                &[module],
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
    let files = Files::new();
    files.write(
        "reader.vibe",
        r#"
def values;[Root.answer,State::Ready.name,Math.PI,Parent.zero];end
def typed(value: Root);value.answer;end
def optional;before=Parent.zero;Parent={zero:19};[before,Parent.zero];end
def shadow;Root=11;Root;end
def local;secret;end
"#,
    );
    files.write("peer.vibe", "def zero;17;end");
    let module = files
        .engine()
        .compile("require(:reader)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let receiver = files
        .engine()
        .compile(
            r#"
class Root
 def self.answer;7;end
 def answer;9;end
end
module Math
 PI=22
end
enum State
 Ready
end
def run(m)
 require(:peer,as: :Parent)
 secret=123
 hidden=begin;m.local;rescue;"hidden";end
 [m.values,m.typed(Root.new),m.optional,Parent.zero,m.shadow,Root.answer,hidden]
end
"#,
        )
        .unwrap();
    for _ in 0..2 {
        let output = receiver
            .call("run", std::slice::from_ref(&module), CallOptions::default())
            .unwrap();
        assert_eq!(
            json(&output.value),
            serde_json::json!([[7, "Ready", 22, 17], 9, [17, 19], 17, 11, 7, "hidden"])
        );
    }
}

#[test]
fn required_environments_and_root_aliases_preserve_captured_negative_indices() {
    let files = Files::new();
    files.write("empty.vibe", "nil");
    files.write(
        "rows.vibe",
        "rows=[[1]];def change;before=rows;x=rows[-1].push((while true;rows.push([9]);break 2;end));[x,rows,before];end",
    );
    let engine = files.engine();
    for source in [
        "require(:rows).change",
        "require(:empty,as: :Rows);Rows=[[1]];def change;before=Rows;x=Rows[-1].push((while true;Rows.push([9]);break 2;end));[x,Rows,before];end;change",
    ] {
        let script = engine.compile(source).unwrap();
        for _ in 0..2 {
            let result = script.run(CallOptions::default()).unwrap();
            assert_eq!(
                json(&result.value),
                serde_json::json!([[1, 2], [[1, 2], [9]], [[1]]]),
                "{source}"
            );
        }
    }
}

#[test]
fn required_files_read_receiving_globals_and_keep_private_assignments() {
    let files = Files::new();
    files.write(
        "read.vibe",
        "def read;[payload,helper,Box,Math];end;def change;payload.push(2);payload;end",
    );
    files.write(
        "private.vibe",
        "payload=[7];def read;payload;end;def change;payload.push(8);end",
    );
    let script = files.engine().compile("def helper;99;end;class Box;end;m=require(:read);p=require(:private);before=m.read;m.change;p.change;[before,m.read,p.read,payload]").unwrap();
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
    let files = Files::new();
    files.write("empty.vibe", "nil");
    let original = Engine::new().compile("def helper;7;end;class C;def run;[helper,helper(),helper(*[]),helper{1},(helper)()];end;end;C.new").unwrap().run(CallOptions::default()).unwrap().value;
    let receiver = files
        .engine()
        .compile("def run(value);require(:empty,as: :helper);value.run;end")
        .unwrap();
    assert_eq!(
        json(
            &receiver
                .call("run", &[original], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([7, 7, 7, 7, 7])
    );
}

#[test]
fn dynamic_root_aliases_support_assignment_nested_writes_and_parameter_shadowing() {
    let files = Files::new();
    files.write("empty.vibe", "1");
    let engine = files.engine();
    let output = engine
        .compile(
            r#"
def change
 A={items:[1],n:2}
 A.items.push(3)
 A.n+=1
end
def shadow(A);A.n=8;A.n;end
def read;A;end
require(:empty,as: :A)
[change(),A.n,read.items,shadow({n:4}),A.n]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([3, 3, [1, 3], 8, 3]));
    let output = engine
        .compile(
            r#"
require(:empty,as: :A)
A=1
def bump;A+=2;A;end
[bump(),A]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([3, 3]));
}

#[test]
fn required_enums_resolve_global_and_alias_type_annotations() {
    let files = Files::new();
    files.write("state.vibe", "enum State\n Ready\nend");
    let output = files
        .engine()
        .compile(
            r#"
def plain(value: State);value.name;end
def namespaced(value: M.State);value.name;end
require(:state,as: :M)
[plain(State::Ready),namespaced(M.State::Ready)]
"#,
        )
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!(["Ready", "Ready"]));
}

#[test]
fn foreign_required_functions_can_find_new_receiving_hosts_and_root_functions() {
    let files = Files::new();
    files.write("foreign.vibe", "def run;[late_host(),shared(),Math()];end");
    let module = files
        .engine()
        .compile("require(:foreign)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut engine = Engine::new();
    engine.register("late_host", |_, _| Ok(Value::int(3)));
    let receiver = engine
        .compile("def shared;5;end\ndef Math;7;end\ndef run(m);m.run;end")
        .unwrap();
    assert_eq!(
        json(
            &receiver
                .call("run", &[module], CallOptions::default())
                .unwrap()
                .value
        ),
        serde_json::json!([3, 5, 7])
    );
}

#[test]
fn exported_targets_are_selected_before_arguments_change_the_module() {
    let files = Files::new();
    files.write(
        "selection.vibe",
        "def fn(n);n+1;end\ndef push(n,extra:0);n+extra+2;end",
    );
    let engine = files.engine();
    for (expression, expected) in [
        ("m.fn(begin;m.fn=7;end)", 8),
        ("m[:fn](begin;m.fn=7;end)", 8),
        ("m::fn(begin;m.fn=7;end)", 8),
        ("m.push(begin;m.push=7;end)", 9),
        ("m.push(begin;m.push=7;end,extra:3)", 12),
        ("m.public_send(:push,begin;m.push=7;end)", 9),
    ] {
        let source = format!("m=require(:selection);{expression}");
        let result = engine
            .compile(&source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
        assert_eq!(result.value.as_int(), Some(expected), "{expression}");
    }
}

#[test]
fn auto_calls_in_mutation_paths_preserve_returned_collection_values() {
    let files = Files::new();
    files.write("collections.vibe", "data=[1];def items;data;end");
    let output = files
        .engine()
        .compile("m=require(:collections);m.items.push(2);[m.items,m.keys]")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    assert_eq!(json(&output.value), serde_json::json!([[1], ["items"]]));
}

#[test]
fn namespace_initializers_run_before_file_bodies_once_per_call() {
    let files = Files::new();
    files.write(
        "order.vibe",
        "class Hidden\n event(1)\nend\nevent(2)\ndef value;3;end",
    );
    let mut engine = files.engine();
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = events.clone();
    engine.register("event", move |_, args| {
        captured.lock().unwrap().push(args[0].as_int().unwrap());
        Ok(Value::nil())
    });
    let script = engine
        .compile("a=require(:order);b=require(:order);[a.value,b.value,a.keys]")
        .unwrap();
    for _ in 0..2 {
        assert_eq!(
            json(&script.run(CallOptions::default()).unwrap().value),
            serde_json::json!([3, 3, ["value"]])
        );
    }
    assert_eq!(*events.lock().unwrap(), [1, 2, 1, 2]);
}

#[test]
fn receiving_policy_controls_relative_require_from_retained_functions() {
    let files = Files::new();
    files.write(
        "package/main.vibe",
        "def child;require(\"./child\").value;end",
    );
    files.write("package/child.vibe", "def value;7;end");
    let module = files
        .engine()
        .compile("require(\"package/main\")")
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
        .value;
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            deny: vec!["package/child".into()],
            ..ModuleConfig::default()
        })
        .unwrap();
    let denied = engine.compile("def run(m);m.child;end").unwrap();
    let error = denied
        .call("run", std::slice::from_ref(&module), CallOptions::default())
        .unwrap_err();
    assert!(error.message.contains("denied by policy"), "{error:?}");
    let receiver = Engine::new().compile("def run(m);m.child;end").unwrap();
    assert_eq!(
        receiver
            .call("run", &[module], CallOptions::default())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
}

#[test]
fn configured_cache_source_limits_and_mid_initializer_cancellation_are_enforced() {
    let files = Files::new();
    files.write("one.vibe", "def value;1;end");
    files.write("two.vibe", "def value;2;end");
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            cache_limit: 1,
            ..ModuleConfig::default()
        })
        .unwrap();
    let script = engine.compile("require(:one);require(:two)").unwrap();
    let error = script.run(CallOptions::default()).unwrap_err();
    assert!(error.message.contains("cache limit reached"), "{error:?}");
    engine
        .set_module_config(ModuleConfig {
            paths: vec![files.0.clone()],
            source_limit: 4,
            ..ModuleConfig::default()
        })
        .unwrap();
    let error = engine
        .compile("require(:one)")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert!(
        error.message.contains("source exceeds maximum size"),
        "{error:?}"
    );
    files.write("cancel.vibe", "stop();effect();def value;1;end");
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
        .compile("begin;require(:cancel);rescue;effect();ensure;effect();end;effect()")
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
        "def work(n)\n visit(n) { |x| raise \"from block\" if x==0; return x+1 }\n 99\nend",
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
    let opts = CallOptions {
        allow_require: true,
        capabilities: vec![vibescript::Capability::new("visit", move |_| {
            Ok(method.value())
        })],
        ..CallOptions::default()
    };
    let script = files
        .engine()
        .compile("def run(n); require(:worker).work(n); end")
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
