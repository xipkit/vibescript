mod common;

use serde_json::{Value as Json, json};
use vibescript::{CallOptions, CheckedOutcome, Engine, ErrorKind, HostMethod, Value};

#[path = "../examples/support/mod.rs"]
mod support;

#[test]
fn typed_results_preserve_exact_data_and_type_distinctions() {
    let cases: Vec<Json> = serde_json::from_str(include_str!("encoding-cases.json")).unwrap();
    for case in cases {
        let source = case["source"].as_str().unwrap();
        let Some(engine) = common::fixture_engine(case.get("static_error"), source, source) else {
            continue;
        };
        let result = engine
            .compile(source)
            .unwrap_or_else(|error| panic!("{source}: {error}"))
            .call("run", &[Value::nil()], CallOptions::default())
            .unwrap_or_else(|error| panic!("{source}: {error}"));
        let encoded = support::encode(&result.value, "typed", CallOptions::default()).unwrap();
        let actual: Json = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(actual, case["expected"], "{source}");
    }
    let nan = Value::float(f64::from_bits(0x7ff8000000000001));
    let encoded = support::encode(&nan, "typed", CallOptions::default()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Json>(&encoded).unwrap(),
        json!(["typed-v1", ["float", "7ff8000000000001"]])
    );
}

#[test]
fn typed_results_bound_traversal_and_reject_executable_values() {
    let method = HostMethod::new("hidden", |_, _, _| panic!("encoder invoked a method"));
    assert!(support::encode(&method.value(), "typed", CallOptions::default()).is_err());
    let mut value = Value::nil();
    for _ in 0..258 {
        value = Value::array(vec![value]);
    }
    assert!(support::encode(&value, "typed", CallOptions::default()).is_err());
    assert!(support::encode(&Value::nil(), "unknown", CallOptions::default()).is_err());
}

#[test]
fn notification_previews_use_explicit_grants_and_validate_inputs() {
    for (name, args, expected) in [
        (
            "sms",
            r#""number", "body""#,
            json!({"status":"preview", "to":"number", "body":"body"}),
        ),
        (
            "email",
            r#""address", "subject", "body""#,
            json!({"status":"preview", "to":"address", "subject":"subject", "body":"body"}),
        ),
    ] {
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        engine
            .declare_capability(&support::notification(name).unwrap())
            .unwrap();
        let script = engine.compile(&format!("{name}.send({args})")).unwrap();
        let options = CallOptions {
            capabilities: vec![support::notification(name).unwrap()],
            ..CallOptions::default()
        };
        let output = script.run(options.clone()).unwrap();
        let encoded = support::encode(&output.value, "json", CallOptions::default()).unwrap();
        assert_eq!(serde_json::from_slice::<Json>(&encoded).unwrap(), expected);
        // The engine declares the capability, so a call without the grant
        // fails when it starts.
        assert_eq!(
            script.run(CallOptions::default()).unwrap_err().kind,
            ErrorKind::Argument
        );
        options.cancellation.cancel();
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Cancelled);
        // Arguments that do not match the preview's signature are refused
        // before anything runs.
        let two_booleans: &[&str] = if name == "sms" {
            &["V0101", "V0101"]
        } else {
            &["V0301", "V0101", "V0101"]
        };
        for (invalid, codes) in [
            ("", &["V0301"][..]),
            ("1", &["V0301", "V0101"]),
            ("false, false", two_booleans),
            ("a: 1", &["V0301", "V0302"]),
        ] {
            let mut engine = vibescript::Engine::new();
            engine
                .declare_capability(&support::notification(name).unwrap())
                .unwrap();
            let source = format!("{name}.send({invalid})");
            let error = engine.compile(&source).err().unwrap();
            assert_eq!(common::codes(&error), codes, "{source}");
        }
    }
}

#[test]
fn unchanged_notification_examples_run_through_checked_template_grants() {
    for (name, source, expected) in [
        (
            "sms",
            include_str!("site/showcase/notifications/sms.vibe"),
            json!({"status":"preview", "to":"+12025550123", "body":"Order 1042 is on its way."}),
        ),
        (
            "email",
            include_str!("site/showcase/notifications/email.vibe"),
            json!({"status":"preview", "to":"alex@example.com", "subject":"Welcome, Alex!", "body":"Hi Alex,\n\nYour account is ready. Thanks for joining us."}),
        ),
    ] {
        let mut engine = common::gradual_engine();
        engine.set_strict_effects(true);
        let script = engine.compile(source).unwrap();
        let options = CallOptions {
            capabilities: vec![support::notification(name).unwrap()],
            ..CallOptions::default()
        };
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.is_clean(), "{name}: {report:?}");
        let CheckedOutcome::Executed(output) = script.checked_call("run", &[], options).unwrap()
        else {
            panic!("{name}: checked notification preview was rejected");
        };
        let encoded = support::encode(&output.value, "json", CallOptions::default()).unwrap();
        assert_eq!(serde_json::from_slice::<Json>(&encoded).unwrap(), expected);
    }
}
