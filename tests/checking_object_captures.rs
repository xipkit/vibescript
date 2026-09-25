mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{
    CallOptions, Capability, CheckReport, ErrorKind, HostMethod, Limits, Signature, Value,
};

fn object(whole: Value, captures: Vec<Value>, named: Value) -> Value {
    Value::object(vec![
        (b"to_s".to_vec(), whole),
        (b"captures".to_vec(), Value::array(captures)),
        (b"named_captures".to_vec(), named),
    ])
}

fn options(value: Value) -> CallOptions {
    CallOptions {
        capabilities: vec![Capability::from_value("cap", value)],
        ..CallOptions::default()
    }
}

fn clean(report: &CheckReport, source: &str) {
    assert!(report.is_clean(), "{source}: {report:?}");
    assert_eq!(report.stats.retained_memory_bytes, 0, "{source}");
}

fn same(actual: &Value, expected: &Value, source: &str) {
    assert_eq!(
        actual.type_name(),
        expected.type_name(),
        "{source}: {actual:?}"
    );
    match expected.type_name() {
        "int" => assert_eq!(actual.as_int(), expected.as_int(), "{source}"),
        "string" | "symbol" => assert_eq!(actual.as_bytes(), expected.as_bytes(), "{source}"),
        "nil" => (),
        "array" => {
            let actual = actual.as_array().unwrap();
            let expected = expected.as_array().unwrap();
            assert_eq!(actual.len(), expected.len(), "{source}");
            for (a, b) in actual.iter().zip(expected) {
                same(a, b, source);
            }
        }
        other => panic!("unsupported witness {other}"),
    }
}

fn returns(template: Value, expression: &str, annotation: &str, expected: Value) {
    let source = format!("def run -> {annotation};{expression};end");
    let script = common::gradual_engine().compile(&source).unwrap();
    let options = options(template);
    clean(&script.check_function("run", &options).unwrap(), &source);
    clean(&script.check_call("run", &[], &options).unwrap(), &source);
    same(
        &script.call("run", &[], options).unwrap().value,
        &expected,
        &source,
    );
}

#[test]
fn numeric_capture_indexes_preserve_whole_and_exact_positions() {
    let template = object(
        Value::bytes(b"whole".to_vec()),
        vec![
            Value::int(7),
            Value::nil(),
            Value::array(vec![Value::int(9)]),
        ],
        Value::nil(),
    );
    for (index, annotation, expected) in [
        ("0", "string", Value::bytes(b"whole".to_vec())),
        ("1", "int", Value::int(7)),
        ("2", "nil", Value::nil()),
        ("3", "array<int>", Value::array(vec![Value::int(9)])),
        ("4", "nil", Value::nil()),
        ("-1", "array<int>", Value::array(vec![Value::int(9)])),
        ("-2", "nil", Value::nil()),
        ("-3", "int", Value::int(7)),
        ("-4", "string", Value::bytes(b"whole".to_vec())),
        ("-5", "nil", Value::nil()),
        ("1.9", "int", Value::int(7)),
        ("-3.9", "int", Value::int(7)),
        ("-0.9", "string", Value::bytes(b"whole".to_vec())),
        ("9223372036854775807", "nil", Value::nil()),
        ("-9223372036854775808", "nil", Value::nil()),
    ] {
        returns(
            template.clone(),
            &format!("cap[{index}]"),
            annotation,
            expected,
        );
    }
    for whole in [Value::nil(), Value::int(17)] {
        for index in ["0", "-1"] {
            returns(
                object(whole.clone(), vec![], Value::nil()),
                &format!("cap[{index}]"),
                whole.type_name(),
                whole.clone(),
            );
        }
    }
}

#[test]
fn named_capture_fallback_uses_stored_bytes_and_stored_nil_shadows_it() {
    let named = Value::hash(vec![
        (b"name".to_vec(), Value::int(17)),
        (b"shadow".to_vec(), Value::int(19)),
        (b"to_s".to_vec(), Value::int(23)),
        (vec![b'_', 255], Value::int(29)),
    ]);
    let template = Value::object(vec![
        (b"to_s".to_vec(), Value::nil()),
        (b"shadow".to_vec(), Value::nil()),
        (b"named_captures".to_vec(), named),
    ]);
    for index in ["'name'", ":name"] {
        returns(
            template.clone(),
            &format!("cap[{index}]"),
            "int",
            Value::int(17),
        );
    }
    for index in ["'shadow'", ":shadow", ":to_s", ":missing"] {
        returns(
            template.clone(),
            &format!("cap[{index}]"),
            "nil",
            Value::nil(),
        );
    }
    returns(template, "cap[\"_\\xff\"]", "int", Value::int(29));
}

#[test]
fn named_capture_fallback_does_not_recurse_or_apply_to_plain_hashes() {
    let nested = object(
        Value::nil(),
        vec![],
        Value::hash(vec![(b"hidden".to_vec(), Value::int(7))]),
    );
    let template = object(Value::nil(), vec![], nested);
    returns(template, "cap[:hidden]", "nil", Value::nil());
    for named in [Value::nil(), Value::int(7), Value::array(vec![])] {
        returns(
            object(Value::nil(), vec![], named),
            "cap[:missing]",
            "nil",
            Value::nil(),
        );
    }
    for template in [
        Value::object(vec![(
            b"named_captures".to_vec(),
            Value::hash(vec![(b"name".to_vec(), Value::int(7))]),
        )]),
        Value::hash(vec![
            (b"to_s".to_vec(), Value::nil()),
            (
                b"named_captures".to_vec(),
                Value::hash(vec![(b"name".to_vec(), Value::int(7))]),
            ),
        ]),
    ] {
        returns(template, "cap[:name]", "nil", Value::nil());
    }
}

#[test]
fn invalid_numeric_capture_access_is_catchable_and_stops_the_success_path() {
    let valid = object(Value::int(7), vec![], Value::nil());
    let mut cases = Vec::new();
    for index in [
        "true",
        "nil",
        "[]",
        "{}",
        "1..2",
        "0,1",
        "9223372036854775808.0",
    ] {
        cases.push((valid.clone(), index));
    }
    for template in [
        Value::object(vec![(b"to_s".to_vec(), Value::int(7))]),
        Value::object(vec![(b"captures".to_vec(), Value::array(vec![]))]),
        Value::object(vec![
            (b"to_s".to_vec(), Value::int(7)),
            (b"captures".to_vec(), Value::nil()),
        ]),
        Value::hash(vec![
            (b"to_s".to_vec(), Value::int(7)),
            (b"captures".to_vec(), Value::array(vec![])),
        ]),
    ] {
        cases.push((template, "0"));
    }
    for (template, index) in cases {
        let source = format!(
            "def run -> int;begin;cap[{index}];'unreachable';rescue RuntimeError;7;end;end"
        );
        let script = common::gradual_engine().compile(&source).unwrap();
        let options = options(template);
        for report in [
            script.check_function("run", &options).unwrap(),
            script.check_call("run", &[], &options).unwrap(),
        ] {
            assert!(report.incomplete.is_empty(), "{source}: {report:?}");
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|d| d.message.starts_with("Cannot index ")),
                "{source}: {report:?}"
            );
            assert!(
                !report
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("Return value:")),
                "{source}: {report:?}"
            );
        }
        assert_eq!(
            script.call("run", &[], options).unwrap().value.as_int(),
            Some(7),
            "{source}"
        );
    }
}

#[test]
fn nonfinite_host_selectors_reject_before_the_following_expression() {
    let script = common::gradual_engine()
        .compile("def run(i:float) -> int;begin;cap[i];'bad';rescue RuntimeError;7;end;end")
        .unwrap();
    let options = options(object(Value::int(7), vec![], Value::nil()));
    for number in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        9_223_372_036_854_775_808.0,
    ] {
        let args = [Value::float(number)];
        let report = script.check_call("run", &args, &options).unwrap();
        assert!(report.incomplete.is_empty(), "{report:?}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.starts_with("Cannot index ")),
            "{report:?}"
        );
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Return value:")),
            "{report:?}"
        );
        assert_eq!(
            script
                .call("run", &args, options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(7)
        );
    }
}

#[test]
fn structural_contracts_keep_possible_index_failures_in_rescue_flow() {
    for annotation in [
        "{to_s:int,captures:array<int>}",
        "{to_s?:int,captures:array<int>}",
        "{to_s:int,captures?:array<int>}",
        "hash<string,any>",
    ] {
        let source = format!(
            "def run(h:{annotation}) -> int;begin;h[0];0;rescue RuntimeError;'bad';end;end"
        );
        let script = common::gradual_engine().compile(&source).unwrap();
        let options = CallOptions::default();
        let report = script.check_function("run", &options).unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Return value: expected int")),
            "{source}: {report:?}"
        );
        for plain in [false, true] {
            let fields = vec![
                (b"to_s".to_vec(), Value::int(7)),
                (b"captures".to_vec(), Value::array(vec![])),
            ];
            let value = if plain {
                Value::hash(fields)
            } else {
                Value::object(fields)
            };
            let result = script.call("run", &[value], options.clone());
            if plain {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Type, "{source}");
            } else {
                assert_eq!(result.unwrap().value.as_int(), Some(0), "{source}");
            }
        }
    }
}

#[test]
fn generic_array_captures_keep_the_last_item_present() {
    for index in ["0", "-1", "1", "-2"] {
        let annotation = if matches!(index, "0" | "-1") {
            "int"
        } else {
            "int?"
        };
        let source =
            format!("def run(h:{{to_s:int,captures:array<int>}}) -> {annotation};h[{index}];end");
        let script = common::gradual_engine().compile(&source).unwrap();
        let options = CallOptions::default();
        clean(&script.check_function("run", &options).unwrap(), &source);
        for captures in [
            vec![],
            vec![Value::int(11)],
            vec![Value::int(11), Value::int(13)],
        ] {
            let template = Value::object(vec![
                (b"to_s".to_vec(), Value::int(7)),
                (b"captures".to_vec(), Value::array(captures)),
            ]);
            script.call("run", &[template], options.clone()).unwrap();
        }
    }
}

#[test]
fn broad_selectors_keep_valid_results_and_invalid_kind_failures() {
    for annotation in ["int", "float", "any"] {
        let source = format!(
            "def run(i:{annotation}) -> int;begin;cap[i];0;rescue RuntimeError;'bad';end;end"
        );
        let script = common::gradual_engine().compile(&source).unwrap();
        let options = options(object(Value::int(7), vec![], Value::nil()));
        let report = script.check_function("run", &options).unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Return value: expected int")),
            "{source}: {report:?}"
        );
        let good = if annotation == "float" {
            Value::float(0.0)
        } else {
            Value::int(0)
        };
        assert_eq!(
            script
                .call("run", &[good], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(0)
        );
        if annotation != "int" {
            let bad = if annotation == "float" {
                Value::float(f64::INFINITY)
            } else {
                Value::nil()
            };
            assert_eq!(
                script.call("run", &[bad], options).unwrap_err().kind,
                ErrorKind::Type
            );
        }
    }
}

#[test]
fn large_integer_selectors_keep_the_rescue_reachable() {
    let source =
        "def run -> int;begin;cap[9223372036854775808];0;rescue RuntimeError;'bad';end;end";
    let script = common::gradual_engine().compile(source).unwrap();
    let options = options(object(Value::int(7), vec![], Value::nil()));
    let report = script.check_call("run", &[], &options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Return value: expected int")),
        "{report:?}"
    );
    assert_eq!(
        script.call("run", &[], options).unwrap_err().kind,
        ErrorKind::Type
    );
}

#[test]
fn rooted_numeric_capture_mutations_fail_before_argument_effects() {
    let source = "def run -> int;n=0;begin;cap[1].push(begin;n=1;8;end);rescue RuntimeError;if n==0;'bad';else;0;end;end;end";
    let script = common::gradual_engine().compile(source).unwrap();
    let options = options(object(
        Value::nil(),
        vec![Value::array(vec![Value::int(7)])],
        Value::nil(),
    ));
    let report = script.check_call("run", &[], &options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Return value: expected int")),
        "{report:?}"
    );
    assert_eq!(
        script.call("run", &[], options).unwrap_err().kind,
        ErrorKind::Type
    );
}

#[test]
fn extracted_capture_arrays_are_value_snapshots() {
    for selection in ["cap[1]", "cap[:name]"] {
        let template = object(
            Value::nil(),
            vec![Value::array(vec![Value::int(7)])],
            Value::hash(vec![(b"name".to_vec(), Value::array(vec![Value::int(7)]))]),
        );
        let expression = format!("old={selection};old.push(8);{selection}");
        returns(
            template,
            &expression,
            "array<int>",
            Value::array(vec![Value::int(7)]),
        );
    }
}

#[test]
fn optional_root_fields_preserve_plain_hash_and_capture_fallback_results() {
    let source = "def run(h:{to_s:int,named_captures:{name:int},name?:string}) -> int|string|nil;h[:name];end";
    let script = common::gradual_engine().compile(source).unwrap();
    let options = CallOptions::default();
    clean(&script.check_function("run", &options).unwrap(), source);
    for plain in [false, true] {
        for shadow in [false, true] {
            let mut fields = vec![
                (b"to_s".to_vec(), Value::int(23)),
                (
                    b"named_captures".to_vec(),
                    Value::hash(vec![(b"name".to_vec(), Value::int(7))]),
                ),
            ];
            if shadow {
                fields.push((b"name".to_vec(), Value::bytes(b"root".to_vec())));
            }
            let value = if plain {
                Value::hash(fields)
            } else {
                Value::object(fields)
            };
            clean(
                &script
                    .check_call("run", std::slice::from_ref(&value), &options)
                    .unwrap(),
                source,
            );
            let expected = if shadow {
                Value::bytes(b"root".to_vec())
            } else if plain {
                Value::nil()
            } else {
                Value::int(7)
            };
            same(
                &script.call("run", &[value], options.clone()).unwrap().value,
                &expected,
                source,
            );
        }
    }
    let narrow = common::gradual_engine()
        .compile(&source.replace("int|string|nil", "int"))
        .unwrap();
    let report = narrow.check_function("run", &options).unwrap();
    assert!(report.incomplete.is_empty(), "{report:?}");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Return value: expected int")),
        "{report:?}"
    );
}

#[test]
fn ordinary_capture_objects_keep_their_stored_fields_mutable() {
    let template = object(
        Value::nil(),
        vec![Value::array(vec![Value::int(7)])],
        Value::hash(vec![(b"name".to_vec(), Value::array(vec![Value::int(7)]))]),
    );
    for (expression, expected) in [
        ("cap[:name].push(8);cap[:name]", vec![Value::int(7)]),
        (
            "cap[:named_captures][:name].push(8);cap[:name]",
            vec![Value::int(7), Value::int(8)],
        ),
        (
            "old=cap[:name];cap[:named_captures][:name].push(8);old",
            vec![Value::int(7)],
        ),
        (
            "cap[:captures][0].push(8);cap[1]",
            vec![Value::int(7), Value::int(8)],
        ),
    ] {
        returns(
            template.clone(),
            expression,
            "array<int>",
            Value::array(expected),
        );
    }
}

#[test]
fn checking_capture_protocol_never_invokes_to_s_or_host_callbacks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observer = calls.clone();
    let method = HostMethod::new("cap.to_s", move |_, _, _| {
        observer.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(99))
    });
    let template = object(
        method.value(),
        vec![Value::int(7)],
        Value::hash(vec![(b"name".to_vec(), Value::int(11))]),
    );
    returns(template.clone(), "cap[1]", "int", Value::int(7));
    returns(template, "cap[:name]", "int", Value::int(11));
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn host_methods_found_through_captures_remain_attached() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observer = calls.clone();
    let method = HostMethod::new("cap.answer", move |_, _, _| {
        observer.fetch_add(1, Ordering::Relaxed);
        Ok(Value::int(99))
    })
    .with_signature(Signature {
        params: vec![],
        result: "int".into(),
        accepts_block: false,
    })
    .unwrap();
    let options = options(object(
        method.value(),
        vec![],
        Value::hash(vec![(b"answer".to_vec(), method.value())]),
    ));
    for (i, selection) in ["cap[0]", "cap[:answer]"].into_iter().enumerate() {
        let source = format!("def run -> int;{selection}();end");
        let script = common::gradual_engine().compile(&source).unwrap();
        clean(&script.check_call("run", &[], &options).unwrap(), &source);
        assert_eq!(calls.load(Ordering::Relaxed), i);
        assert_eq!(
            script
                .call("run", &[], options.clone())
                .unwrap()
                .value
                .as_int(),
            Some(99)
        );
        let source = format!("def run;f={selection};f();end");
        let script = common::gradual_engine().compile(&source).unwrap();
        let report = script.check_call("run", &[], &options).unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
        assert_eq!(
            script.call("run", &[], options.clone()).unwrap_err().kind,
            ErrorKind::Type
        );
        assert_eq!(calls.load(Ordering::Relaxed), i + 1);
    }
}

#[test]
fn capture_analysis_obeys_accounting_and_cancellation() {
    let options = options(object(
        Value::int(7),
        (0..96).map(Value::int).collect(),
        Value::hash(vec![(b"name".to_vec(), Value::int(11))]),
    ));
    let script = common::gradual_engine()
        .compile("def run -> int;cap[-1]+cap[:name];end")
        .unwrap();
    let baseline = script.check_call("run", &[], &options).unwrap();
    clean(&baseline, "quota baseline");
    let stats = baseline.stats;
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
                .map(|e| e.kind),
            expected
        );
    }
    let mut cancelled = options.clone();
    cancelled.cancellation = vibescript::CancellationToken::new();
    cancelled.cancellation.cancel();
    assert_eq!(
        script.check_call("run", &[], &cancelled).unwrap_err().kind,
        ErrorKind::Cancelled
    );
    for _ in 0..4 {
        clean(
            &script.check_call("run", &[], &options).unwrap(),
            "after failed analysis",
        );
    }
}
