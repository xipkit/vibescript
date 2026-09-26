mod common;

use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use vibescript::{
    CallOptions, Capability, CheckReport, CheckedOutcome, Engine, ErrorKind, HostMethod, Limits,
    ModuleConfig, Script, Signature, SignatureParam, Value,
};

fn signature(parameter: &str, result: &str) -> Signature {
    Signature {
        params: vec![SignatureParam {
            name: "message".into(),
            ty: parameter.into(),
            optional: false,
        }],
        result: result.into(),
        accepts_block: false,
    }
}

/// A signed `SMS.send` template that counts every validator and callback boundary it crosses.
fn sms(effects: &Arc<AtomicUsize>) -> Value {
    let callback = effects.clone();
    let validator = effects.clone();
    let send = HostMethod::new("SMS.send", move |ctx, args, _| {
        callback.fetch_add(1, Ordering::Relaxed);
        let mut receipt = b"queued:".to_vec();
        receipt.extend_from_slice(args[0].as_bytes().unwrap());
        ctx.bytes(&receipt)
    })
    .with_contract(
        move |_, _, _| {
            validator.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
        |_, _| Ok(()),
    )
    .with_signature(signature("string", "string"))
    .unwrap();
    Value::object(vec![(b"send".to_vec(), send.value())])
}

fn granted(template: Value) -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::from_value("SMS", template)],
        ..CallOptions::default()
    }
}

fn strict(source: &str) -> Script {
    let mut engine = common::gradual_engine();
    engine.set_strict_effects(true);
    engine.compile(source).unwrap()
}

fn assert_report(report: &CheckReport, clean: bool, context: &str) {
    assert!(report.incomplete.is_empty(), "{context}: {report:?}");
    assert_eq!(report.is_clean(), clean, "{context}: {report:?}");
}

fn echo() -> HostMethod {
    HostMethod::new("sms.deliver", |ctx, args, keywords| {
        let mut values = args.to_vec();
        values.extend(keywords.iter().map(|(_, value)| value.clone()));
        ctx.array(&values)
    })
}

fn deliverer() -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::from_value(
            "sms",
            Value::object(vec![(b"deliver".to_vec(), echo().value())]),
        )],
        ..CallOptions::default()
    }
}

#[test]
fn value_templates_check_cleanly_in_every_scope_without_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let options = granted(sms(&effects));
    for (source, clean) in [
        ("def run -> string; SMS.send(\"hello\"); end", true),
        ("def run; SMS.send(1); end", false),
        ("def run -> int; SMS.send(\"hello\"); end", false),
    ] {
        let script = strict(source);
        assert_report(
            &script.check_call("run", &[], &options).unwrap(),
            clean,
            source,
        );
        assert_report(
            &script.check_function("run", &options).unwrap(),
            clean,
            source,
        );
        assert_report(&script.check(&options).unwrap(), clean, source);
        assert_eq!(effects.load(Ordering::Relaxed), 0, "{source}");
    }
    for (source, clean) in [("SMS.send(\"hello\")", true), ("SMS.send(1)", false)] {
        let script = strict(source);
        assert_report(
            &script.check_call("__main__", &[], &options).unwrap(),
            clean,
            source,
        );
        assert_report(&script.check(&options).unwrap(), clean, source);
    }
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let script = strict("def run -> string; SMS.send(\"hello\"); end");
    let CheckedOutcome::Executed(outcome) =
        script.checked_call("run", &[], options.clone()).unwrap()
    else {
        panic!("clean template call was rejected");
    };
    assert_eq!(outcome.value.as_bytes(), Some(b"queued:hello".as_slice()));
    assert_eq!(effects.load(Ordering::Relaxed), 2);
    let CheckedOutcome::Rejected(report) = strict("def run; SMS.send(1); end")
        .checked_call("run", &[], options)
        .unwrap()
    else {
        panic!("bad argument executed host code");
    };
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("string")),
        "{report:?}"
    );
    assert_eq!(effects.load(Ordering::Relaxed), 2);
}

#[test]
fn opaque_factories_keep_reports_incomplete_beside_value_templates() {
    let effects = Arc::new(AtomicUsize::new(0));
    let script = strict("def run -> string; SMS.send(\"hello\"); end");
    for factory_first in [false, true] {
        let mut capabilities = vec![
            Capability::from_value("SMS", sms(&effects)),
            Capability::new("clock", |_| panic!("checker invoked a factory")),
        ];
        if factory_first {
            capabilities.reverse();
        }
        let options = CallOptions {
            capabilities,
            ..CallOptions::default()
        };
        for report in [
            script.check_call("run", &[], &options).unwrap(),
            script.check_function("run", &options).unwrap(),
            script.check(&options).unwrap(),
        ] {
            assert!(report.diagnostics.is_empty(), "{report:?}");
            assert_eq!(report.incomplete.len(), 1, "{report:?}");
            let message = &report.incomplete[0].message;
            assert!(
                message.contains("clock") && message.contains("factory"),
                "{report:?}"
            );
        }
        let CheckedOutcome::Rejected(_) = script.checked_call("run", &[], options).unwrap() else {
            panic!("factory-backed call executed");
        };
    }
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

#[cfg(feature = "tokio")]
#[test]
fn value_templates_never_construct_async_futures_during_checks() {
    let constructed = Arc::new(AtomicUsize::new(0));
    let count = constructed.clone();
    let visit = HostMethod::new_async("Jobs.visit", move |call, _, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Box::pin(async move {
            tokio::task::yield_now().await;
            call.context()?.charge(1)?;
            Ok(Value::int(7))
        })
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: false,
    })
    .unwrap();
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "Jobs",
            Value::object(vec![(b"visit".to_vec(), visit.value())]),
        )],
        ..CallOptions::default()
    };
    for source in [
        "def run -> int; Jobs.visit(); end",
        "def run -> string; Jobs.visit(); end",
    ] {
        let script = strict(source);
        let clean = source.contains("-> int");
        assert_report(
            &script.check_call("run", &[], &options).unwrap(),
            clean,
            source,
        );
        assert_report(
            &script.check_function("run", &options).unwrap(),
            clean,
            source,
        );
        assert_report(&script.check(&options).unwrap(), clean, source);
    }
    assert_eq!(constructed.load(Ordering::Relaxed), 0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let runner = vibescript::asynchronous::Runner::new(1).unwrap();
    let output = runtime
        .block_on(runner.call(
            strict("def run -> int; Jobs.visit(); end"),
            "run".into(),
            vec![],
            options,
        ))
        .unwrap();
    assert_eq!(output.value.as_int(), Some(7));
    assert_eq!(constructed.load(Ordering::Relaxed), 1);
}

#[test]
fn value_templates_keep_call_forms_attachment_and_repeated_grants() {
    let mut declared = Engine::new();
    declared
        .declare_capability(&deliverer().capabilities[0])
        .unwrap();
    declared.register("identity", |_, _| panic!("detached method reached host"));
    for source in [
        "sms.deliver(1, 2)",
        "sms.deliver 1, 2",
        "sms::deliver(1, 2)",
        "sms.deliver(*[1], last: 2)",
        "local = sms; local.deliver(1, 2)",
        "[sms].fetch(0).deliver(1, 2)",
    ] {
        // `::` is refused with static types (V0416) but still runs without.
        declared.set_static_types(vibescript::STATIC_TYPES_BY_DEFAULT && !source.contains("::"));
        let script = declared.compile(source).unwrap();
        let options = deliverer();
        for _ in 0..3 {
            let output = script
                .run(options.clone())
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert_eq!(output.value.to_string(), "[1, 2]", "{source}");
        }
    }
    let result = declared
        .compile("sms.deliver")
        .unwrap()
        .run(deliverer())
        .unwrap();
    assert_eq!(result.value.to_string(), "[]");
    // A namespace is not indexed and a local is never called, so the other
    // forms do not compile.
    let mut refusing = Engine::new();
    refusing
        .declare_capability(&deliverer().capabilities[0])
        .unwrap();
    refusing.register("identity", |_, _| panic!("detached method reached host"));
    for (source, code, at) in [
        ("sms[\"deliver\"](1, 2)", "V0112", "sms["),
        ("sms[\"deliver\"]", "V0112", "sms["),
        ("a=sms::deliver; a(1)", "V0416", "::"),
        ("[sms[\"deliver\"]]", "V0112", "sms["),
        ("identity(sms[\"deliver\"])", "V0112", "sms["),
    ] {
        let error = refusing.compile(source).err().unwrap();
        assert_eq!(common::codes(&error)[0], code, "{source}");
        assert_eq!(
            error.diagnostics()[0].span.start,
            source.find(at).unwrap(),
            "{source}"
        );
    }
    let script = declared
        .compile("[sms.deliver(1), sms.deliver(2)]")
        .unwrap();
    let options = deliverer();
    let retained = script
        .run(options.clone())
        .unwrap()
        .stats
        .retained_memory_bytes;
    assert!(retained > 0);
    common::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..3 {
                    let output = script.run(options.clone()).unwrap();
                    assert_eq!(output.value.to_string(), "[[1], [2]]");
                    assert_eq!(output.stats.retained_memory_bytes, retained);
                }
            });
        }
    });
}

#[test]
fn value_templates_preserve_precedence_expired_grants_and_strict_validation() {
    let template = Value::object(vec![(b"deliver".to_vec(), echo().value())]);
    let shadowed = CallOptions {
        globals: [("sms".into(), Value::int(7))].into(),
        ..deliverer()
    };
    let script = common::gradual_engine().compile("sms").unwrap();
    assert_eq!(
        script.run(shadowed.clone()).unwrap().value.as_int(),
        Some(7)
    );
    assert!(
        script
            .check_call("__main__", &[], &shadowed)
            .unwrap()
            .is_clean()
    );
    let script = common::gradual_engine().compile("sms.deliver(1)").unwrap();
    for later_wins in [true, false] {
        let mut capabilities = vec![
            Capability::from_value("sms", Value::int(1)),
            Capability::from_value("sms", template.clone()),
        ];
        if !later_wins {
            capabilities.reverse();
        }
        let options = CallOptions {
            capabilities,
            ..CallOptions::default()
        };
        let report = script.check_call("__main__", &[], &options).unwrap();
        assert_eq!(report.is_clean(), later_wins, "{report:?}");
        let result = script.run(options);
        assert_eq!(result.is_ok(), later_wins, "{result:?}");
        if later_wins {
            assert_eq!(result.unwrap().value.to_string(), "[1]");
        }
    }
    let script = common::gradual_engine()
        .compile("def run(sms); sms; end")
        .unwrap();
    assert!(
        script
            .check_call("run", &[Value::int(7)], &deliverer())
            .unwrap()
            .is_clean()
    );
    assert_eq!(
        script
            .call("run", &[Value::int(7)], deliverer())
            .unwrap()
            .value
            .as_int(),
        Some(7)
    );
    let saver = common::gradual_engine()
        .compile("def save; sms; end")
        .unwrap();
    let saved = saver.call("save", &[], deliverer()).unwrap().value;
    let script = common::gradual_engine()
        .compile("def use; sms.deliver(); end")
        .unwrap();
    let expired = CallOptions {
        capabilities: vec![Capability::from_value("sms", saved)],
        ..CallOptions::default()
    };
    let report = script.check_call("use", &[], &expired).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("earlier invocation")),
        "{report:?}"
    );
    let error = script.call("use", &[], expired).unwrap_err();
    assert!(
        error.message.contains("was not granted to this call"),
        "{error}"
    );
    let effects = Arc::new(AtomicUsize::new(0));
    let script = strict("def run; 7; end");
    let poisoned = CallOptions {
        globals: [(
            "hidden".to_owned(),
            Value::object(vec![(b"f".to_vec(), echo().value())]),
        )]
        .into(),
        ..granted(sms(&effects))
    };
    assert_eq!(
        script.check_call("run", &[], &poisoned).unwrap_err().kind,
        ErrorKind::Runtime
    );
    let error = script.call("run", &[], poisoned).unwrap_err();
    assert!(error.message.contains("data-only"), "{error}");
    assert_eq!(effects.load(Ordering::Relaxed), 0);
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Files(PathBuf);

impl Files {
    fn new() -> Self {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".cache/tmp");
        fs::create_dir_all(&base).unwrap();
        loop {
            let path = base.join(format!(
                "checking-capabilities-{}-{}",
                common::process_id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => panic!("{error}"),
            }
        }
    }

    fn write(&self, name: &str, source: &str) {
        fs::write(self.0.join(name), source).unwrap();
    }

    fn engine(&self) -> Engine {
        let mut engine = common::gradual_engine();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![self.0.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
        engine.set_strict_effects(true);
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

#[test]
fn value_templates_bind_eagerly_and_serve_required_files() {
    let effects = Arc::new(AtomicUsize::new(0));
    let count = effects.clone();
    let mut engine = common::gradual_engine();
    engine.register("effect", move |_, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::nil())
    });
    let script = engine.compile("module M; effect(); end; 7").unwrap();
    let mut options = CallOptions {
        capabilities: vec![Capability::from_value(
            "blob",
            Value::bytes(vec![b'x'; 100_000]),
        )],
        ..CallOptions::default()
    };
    options.limits.memory_bytes = Some(32_768);
    assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Memory);
    let options = CallOptions {
        capabilities: vec![Capability::from_value(
            "loose",
            Value::array(vec![echo().value()]),
        )],
        ..CallOptions::default()
    };
    let error = script.run(options).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert!(
        error.message.contains("cannot be used as a value"),
        "{error}"
    );
    assert_eq!(effects.load(Ordering::Relaxed), 0);

    let files = Files::new();
    files.write("notify.vibe", "def notify(n); sms.deliver(n); end");
    let script = files
        .engine()
        .compile("def run -> int; require(:notify).notify(21); end")
        .unwrap();
    let count = effects.clone();
    let deliver = HostMethod::new("sms.deliver", move |_, args, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(args[0].as_int().unwrap() * 2))
    })
    .with_signature(signature("int", "int"))
    .unwrap();
    let options = CallOptions {
        allow_require: true,
        capabilities: vec![Capability::from_value(
            "sms",
            Value::object(vec![(b"deliver".to_vec(), deliver.value())]),
        )],
        ..CallOptions::default()
    };
    for _ in 0..2 {
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(effects.load(Ordering::Relaxed), 0);
    }
    for round in 1..=2 {
        assert_eq!(
            script
                .call("run", &[], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(42)
        );
        assert_eq!(effects.load(Ordering::Relaxed), round);
    }
    let ungranted = CallOptions {
        allow_require: true,
        ..CallOptions::default()
    };
    assert!(
        !script
            .check_call("run", &[], &ungranted)
            .unwrap()
            .is_clean()
    );
    assert_eq!(
        script.call("run", &[], ungranted).unwrap_err().kind,
        ErrorKind::Name
    );
}

#[test]
fn value_template_reports_obey_quotas_cancellation_and_deadlines() {
    let effects = Arc::new(AtomicUsize::new(0));
    let options = granted(sms(&effects));
    let script = strict("def run -> string; SMS.send(\"hello\"); end");
    let baseline = script.check_call("run", &[], &options).unwrap();
    assert!(baseline.is_clean(), "{baseline:?}");
    assert_eq!(baseline.stats.retained_memory_bytes, 0);
    let stats = baseline.stats;
    drop(baseline);
    for (memory, steps, expected) in [
        (stats.peak_memory_bytes, stats.steps, None),
        (
            stats.peak_memory_bytes - 1,
            stats.steps,
            Some(ErrorKind::Memory),
        ),
        (
            stats.peak_memory_bytes,
            stats.steps - 1,
            Some(ErrorKind::Steps),
        ),
    ] {
        let limited = CallOptions {
            limits: Limits {
                memory_bytes: Some(memory),
                steps: Some(steps),
                ..Limits::default()
            },
            ..options.clone()
        };
        assert_eq!(
            script
                .check_call("run", &[], &limited)
                .err()
                .map(|error| error.kind),
            expected
        );
    }
    for sample in 0..16 {
        for memory in [false, true] {
            let mut limited = options.clone();
            let kind = if memory {
                limited.limits.memory_bytes = Some(stats.peak_memory_bytes * sample / 16);
                ErrorKind::Memory
            } else {
                limited.limits.steps = Some(stats.steps * sample as u64 / 16);
                ErrorKind::Steps
            };
            assert_eq!(
                script.check_call("run", &[], &limited).unwrap_err().kind,
                kind
            );
        }
    }
    for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
        let mut interrupted = granted(sms(&effects));
        if kind == ErrorKind::Cancelled {
            interrupted.cancellation.cancel();
        } else {
            interrupted.deadline = Some(std::time::Instant::now());
        }
        assert_eq!(
            script
                .check_call("run", &[], &interrupted)
                .unwrap_err()
                .kind,
            kind
        );
        assert_eq!(
            script.check_function("run", &interrupted).unwrap_err().kind,
            kind
        );
        assert_eq!(
            script
                .checked_call("run", &[], interrupted)
                .unwrap_err()
                .kind,
            kind
        );
    }
    assert_eq!(effects.load(Ordering::Relaxed), 0);
    let report = strict("def run; SMS.send(1); end")
        .check_call("run", &[], &options)
        .unwrap();
    assert!(!report.diagnostics.is_empty());
    assert!(report.stats.retained_memory_bytes > 0);
    assert!(report.stats.peak_memory_bytes >= report.stats.retained_memory_bytes);
}
