use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

fn run(source: &str) -> vibescript::Outcome {
    Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap()
}

#[test]
fn host_money_keeps_values_compact_and_formats_the_full_cent_range() {
    assert_eq!(std::mem::size_of::<Value>(), 16);
    for (cents, expected) in [
        (0, "0.00 USD"),
        (1, "0.01 USD"),
        (-1, "-0.01 USD"),
        (12345, "123.45 USD"),
        (i64::MIN, "-92233720368547758.08 USD"),
        (i64::MAX, "92233720368547758.07 USD"),
    ] {
        let value = Value::money(cents, "uSd").unwrap();
        assert_eq!(value.as_money(), Some((cents, "USD")));
        assert_eq!(value.type_name(), "money");
        assert_eq!(value.to_string(), expected);
    }
    for currency in ["", "US", "USDD", " USD", "US$", "éA", "K", "U\0D"] {
        assert_eq!(
            Value::money(0, currency).unwrap_err().kind,
            ErrorKind::Argument,
            "{currency:?}"
        );
    }
    assert_eq!(Value::money(100, "zzz").unwrap().to_string(), "1.00 ZZZ");
    assert_eq!(Value::int(100).as_money(), None);
}

#[test]
fn host_imports_and_arithmetic_do_not_retain_money_storage_or_mutate_inputs() {
    let input = Value::money(100, "usd").unwrap();
    let mut engine = Engine::new();
    engine.register("fee", |_, _| Value::money(25, "USD"));
    let script = engine
        .compile("def run(input)\ncopy=input;input+=fee();input\nend")
        .unwrap();
    let result = script
        .call("run", std::slice::from_ref(&input), CallOptions::default())
        .unwrap();
    assert_eq!(input.as_money(), Some((100, "USD")));
    assert_eq!(result.value.as_money(), Some((125, "USD")));
    assert_eq!(result.stats.retained_memory_bytes, 0);
    let identity = engine.compile("def run(input)\ninput\nend").unwrap();
    let options = CallOptions::default();
    let money = identity.call("run", &[input], options.clone()).unwrap();
    let integer = identity.call("run", &[Value::int(100)], options).unwrap();
    assert_eq!(
        money.stats.peak_memory_bytes,
        integer.stats.peak_memory_bytes
    );
    assert_eq!(money.stats.retained_memory_bytes, 0);
}

#[test]
fn money_arrays_and_formatted_strings_keep_their_storage_charged() {
    let result = run("(1..1024).map {|n|money_cents(n,\"USD\")}");
    let values = result.value.as_array().unwrap();
    assert_eq!(values.len(), 1024);
    assert_eq!(values[0].as_money(), Some((1, "USD")));
    assert_eq!(values[1023].as_money(), Some((1024, "USD")));
    assert!(result.stats.retained_memory_bytes >= 1024 * std::mem::size_of::<Value>());
    let error = Engine::new()
        .compile("(1..1024).map {|n|money_cents(n,\"USD\")}")
        .unwrap()
        .run(CallOptions {
            limits: Limits {
                memory_bytes: Some(4096),
                ..Limits::default()
            },
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Memory);
    for (member, expected) in [
        ("currency", "USD"),
        ("amount", "12.34 USD"),
        ("format", "12.34 USD"),
        ("to_s", "12.34 USD"),
        ("string", "12.34 USD"),
        ("inspect", "12.34 USD"),
    ] {
        let result = run(&format!("money_cents(1234,\"USD\").{member}"));
        assert_eq!(result.value.as_bytes(), Some(expected.as_bytes()));
        assert!(result.stats.retained_memory_bytes >= expected.len());
    }
    assert_eq!(
        stringify_json(&Value::money(100, "USD").unwrap(), CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Json
    );
}

#[test]
fn long_money_literals_bound_work_and_release_imported_text() {
    let script = Engine::new()
        .compile("def run(input)\nmoney(input)\nend")
        .unwrap();
    for text in [
        format!("{}1.23 USD", "0".repeat(131072)),
        format!(
            "{}1.23{}USD{}",
            "\u{2003}".repeat(32768),
            "\t".repeat(32768),
            " ".repeat(32768)
        ),
    ] {
        let capacity = text.capacity();
        let input = Value::bytes(text.into_bytes());
        let result = script
            .call("run", std::slice::from_ref(&input), CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_money(), Some((123, "USD")));
        assert_eq!(result.stats.retained_memory_bytes, 0);
        assert!(
            result.stats.peak_memory_bytes < capacity + 12000,
            "{:?}",
            result.stats
        );
        let error = script
            .call(
                "run",
                &[input],
                CallOptions {
                    limits: Limits {
                        steps: Some(32),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
    }
}

#[test]
fn invalid_money_operations_stop_before_later_host_effects() {
    let effects = Arc::new(AtomicUsize::new(0));
    let observed = effects.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "money(\"92233720368547758.08 USD\")",
        "money(\"-92233720368547758.09 USD\")",
        "money(\"1.234 USD\")",
        "money_cents(1e309,\"USD\")",
        "money_cents(0.0/0.0,\"USD\")",
        "money_cents(2**63,\"USD\")",
        "money_cents(2**63-1,\"USD\")+money_cents(1,\"USD\")",
        "money_cents(-2**63,\"USD\")-money_cents(1,\"USD\")",
        "money_cents(2**62,\"USD\")*2",
        "money_cents(-2**63,\"USD\")/-1",
        "money_cents(1,\"USD\")/0",
        "money_cents(0,\"USD\")*(2**100)",
        "money_cents(1,\"USD\")*2.0",
        "money(\"1 USD\")+money(\"1 EUR\")",
        "money(\"1 USD\")<money(\"1 EUR\")",
        "money(\"1 USD\").cents()",
        "money(\"1 USD\").to_s {effect()}",
        "money(\"1 USD\").inspect(x:1)",
        "money(\"1 USD\").nil? {effect()}",
        "JSON.stringify(money(\"1 USD\"))",
    ] {
        let script = engine.compile(&format!("{expression};effect()")).unwrap();
        assert!(script.run(CallOptions::default()).is_err(), "{expression}");
        assert_eq!(effects.load(Ordering::SeqCst), 0, "{expression}");
    }
}

#[test]
fn constructors_and_format_evaluate_arguments_without_invoking_ignored_blocks() {
    let arguments = Arc::new(AtomicUsize::new(0));
    let blocks = Arc::new(AtomicUsize::new(0));
    let mut engine = Engine::new();
    let observed = arguments.clone();
    engine.register("argument", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(7))
    });
    let observed = blocks.clone();
    engine.register("effect", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for source in [
        "money(\"1 USD\",x:argument()) {effect()}",
        "money_cents(100,\"USD\",x:argument()) {effect()}",
        "money(\"1 USD\").format(argument()) {effect()}",
        "money(\"1 USD\").format(x:argument()) {effect()}",
    ] {
        let result = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.to_string(), "1.00 USD");
    }
    assert_eq!(arguments.load(Ordering::SeqCst), 4);
    assert_eq!(blocks.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_and_ignored_quota_errors_prevent_money_results() {
    let mut engine = Engine::new();
    engine.register("cancel", |ctx, _| {
        ctx.cancellation().cancel();
        Ok(Value::bytes("1 USD"))
    });
    engine.register("ignore", |ctx, _| {
        let _ = ctx.charge(u64::MAX);
        Ok(Value::int(1))
    });
    for (source, expected) in [
        ("money(cancel())", ErrorKind::Cancelled),
        ("money(\"1 USD\").format(cancel())", ErrorKind::Cancelled),
        ("money_cents(ignore(),\"USD\")", ErrorKind::Steps),
        ("money(\"1 USD\").format(ignore())", ErrorKind::Steps),
        ("money(\"1 USD\",x:ignore())", ErrorKind::Steps),
    ] {
        let error = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, expected, "{source}");
    }
}

#[test]
fn unchanged_upstream_money_example_returns_typed_host_values() {
    let result = Engine::new()
        .compile(include_str!("site/upstream/money/operations.vibe"))
        .unwrap()
        .call("run", &[], CallOptions::default())
        .unwrap();
    let values = result.value.as_hash().unwrap();
    assert_eq!(values.len(), 3);
    assert_eq!(values[0].0.as_bytes(), Some(b"added".as_slice()));
    assert_eq!(values[0].1.as_money(), Some((6250, "USD")));
    assert_eq!(values[1].0.as_bytes(), Some(b"net".as_slice()));
    assert_eq!(values[1].1.as_money(), Some((4825, "USD")));
    assert_eq!(values[2].0.as_bytes(), Some(b"exceeds".as_slice()));
    assert_eq!(values[2].1.to_string(), "true");
}
