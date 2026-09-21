use vibescript::{CallOptions, Engine, ErrorKind, Value};

#[test]
fn hash_lookup_order_keeps_direct_scoped_and_forwarded_failures_distinct() {
    for annotation in ["hash<string,int>", "{a:int}", "{a:int,...}"] {
        for (call, early) in [
            ("h.missing((begin;n=7;9;end))", true),
            ("h.push((begin;n=7;9;end))", true),
            ("h::missing((begin;n=7;9;end))", false),
            ("h::size((begin;n=7;9;end))", false),
            ("h.size((begin;n=7;9;end))", false),
            ("h.send(:missing,(begin;n=7;9;end))", false),
        ] {
            let source = format!(
                "def run(h:{annotation})->int;n=0;begin;{call};rescue RuntimeError;if n==0;'bad';else;0;end;end;end"
            );
            let script = Engine::new().compile(&source).unwrap();
            let options = CallOptions::default();
            let report = script.check_function("run", &options).unwrap();
            assert!(report.incomplete.is_empty(), "{source}: {report:?}");
            let returns_bad_type = report
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Return value: expected int"));
            assert_eq!(returns_bad_type, early, "{source}: {report:?}");
            let argument = Value::hash(vec![(b"a".to_vec(), Value::int(7))]);
            let outcome = script.call("run", &[argument], options);
            if early {
                assert_eq!(outcome.unwrap_err().kind, ErrorKind::Type, "{source}");
            } else {
                assert_eq!(outcome.unwrap().value.as_int(), Some(0), "{source}");
            }
        }
    }
}

#[test]
fn optional_hash_fields_can_fail_before_argument_effects() {
    for annotation in ["{a:int,missing?:int}", "{a:int,missing?:int,...}"] {
        let source = format!(
            "def run(h:{annotation})->int;n=0;begin;h.missing((begin;n=7;9;end));rescue RuntimeError;if n==0;'bad';else;0;end;end;end"
        );
        let script = Engine::new().compile(&source).unwrap();
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
        let absent = Value::hash(vec![(b"a".to_vec(), Value::int(1))]);
        assert_eq!(
            script
                .call("run", &[absent], options.clone())
                .unwrap_err()
                .kind,
            ErrorKind::Type,
            "{source}"
        );
        let present = Value::hash(vec![
            (b"a".to_vec(), Value::int(1)),
            (b"missing".to_vec(), Value::int(7)),
        ]);
        assert_eq!(
            script
                .call("run", &[present], options)
                .unwrap()
                .value
                .as_int(),
            Some(0),
            "{source}"
        );
    }
}

#[test]
fn general_hash_member_reads_preserve_declared_value_types() {
    for (value_type, value) in [
        ("int", Value::int(7)),
        ("string", Value::bytes(b"bad".to_vec())),
    ] {
        for member in ["a", "a.to_s"] {
            let source = format!("def run(h:hash<string,{value_type}>)->bool;h.{member};end");
            let script = Engine::new().compile(&source).unwrap();
            let options = CallOptions::default();
            let report = script.check_function("run", &options).unwrap();
            assert!(report.incomplete.is_empty(), "{source}: {report:?}");
            assert!(
                report
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("Return value: expected bool")),
                "{source}: {report:?}"
            );
            for input in [
                Value::hash(vec![(b"a".to_vec(), value.clone())]),
                Value::object(vec![(b"a".to_vec(), value.clone())]),
            ] {
                assert_eq!(
                    script
                        .call("run", &[input], options.clone())
                        .unwrap_err()
                        .kind,
                    ErrorKind::Type,
                    "{source}"
                );
            }
        }
    }
}

#[test]
fn general_hash_forwarded_field_calls_keep_catchable_failures() {
    for annotation in ["hash<string,int>", "{a?:int}", "{a:int,...}"] {
        for method in ["send", "public_send"] {
            let source = format!(
                "def run(h:{annotation})->int;begin;h.{method}(:a);rescue RuntimeError;7;end;end"
            );
            let script = Engine::new().compile(&source).unwrap();
            let options = CallOptions::default();
            let report = script.check_function("run", &options).unwrap();
            assert!(report.incomplete.is_empty(), "{source}: {report:?}");
            assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
            for input in [
                Value::hash(vec![(b"a".to_vec(), Value::int(1))]),
                Value::object(vec![(b"a".to_vec(), Value::int(1))]),
            ] {
                assert_eq!(
                    script
                        .call("run", &[input], options.clone())
                        .unwrap()
                        .value
                        .as_int(),
                    Some(7),
                    "{source}"
                );
            }
        }
    }
}

#[test]
fn noncallable_hash_fields_without_native_fallbacks_remain_diagnostics() {
    for call in [
        "h.a()",
        "h.a(7)",
        "h.a {|v|v}",
        "h.send(:a)",
        "h.public_send(:a)",
    ] {
        let source = format!("def run(h:hash<string,int>);{call};end");
        let script = Engine::new().compile(&source).unwrap();
        let options = CallOptions::default();
        let report = script.check_function("run", &options).unwrap();
        assert!(report.incomplete.is_empty(), "{source}: {report:?}");
        assert!(!report.diagnostics.is_empty(), "{source}: {report:?}");
        for input in [
            Value::hash(Vec::new()),
            Value::object(Vec::new()),
            Value::hash(vec![(b"a".to_vec(), Value::int(1))]),
            Value::object(vec![(b"a".to_vec(), Value::int(1))]),
        ] {
            let error = script.call("run", &[input], options.clone()).unwrap_err();
            assert!(
                matches!(error.kind, ErrorKind::Type | ErrorKind::Name),
                "{source}: {error}"
            );
        }
    }
}

#[test]
fn contracts_admitting_protected_hashes_do_not_approve_mutation_paths() {
    let engine = Engine::new();
    for (constructor, annotations, operations) in [
        (
            "\"a\".match(\"(a)\")",
            &[
                "hash",
                "{captures:array<string?>,...}",
                "{begin:any,captures:array<string?>,end:any,named_captures:hash,post_match:string,pre_match:string,to_s:string}",
            ][..],
            &[
                "h.clear",
                "h.captures.push('x')",
                "h.dup.clear",
                "h.send(:clear)",
                "h[:x]=1",
                "h[:captures].push('x')",
                "h[:captures].push(begin;h={};'x';end)",
            ][..],
        ),
        (
            "begin; raise 'bad'; rescue => e; e; end",
            &[
                "hash",
                "{backtrace:array<string>,...}",
                "{type:string,class:string,message:string,to_s:string,code_frame:string,backtrace:array<string>}",
            ][..],
            &[
                "h.clear",
                "h.backtrace.push('x')",
                "h.dup.clear",
                "h.send(:clear)",
                "h[:x]=1",
                "h[:backtrace].push('x')",
                "h[:backtrace].push(begin;h={};'x';end)",
            ][..],
        ),
    ] {
        let input = engine
            .compile(constructor)
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        for annotation in annotations {
            for operation in operations {
                let source = format!(
                    "def run(h:{annotation})->int;begin;{operation};0;rescue RuntimeError;'bad';end;end"
                );
                let script = engine.compile(&source).unwrap();
                let options = CallOptions::default();
                let general = script.check_function("run", &options).unwrap();
                assert!(general.incomplete.is_empty(), "{source}: {general:?}");
                assert!(
                    general.diagnostics.iter().any(|diagnostic| diagnostic
                        .message
                        .contains("Return value: expected int")),
                    "{source}: {general:?}"
                );
                let exact = script
                    .check_call("run", std::slice::from_ref(&input), &options)
                    .unwrap();
                assert!(exact.incomplete.is_empty(), "{source}: {exact:?}");
                assert!(!exact.diagnostics.is_empty(), "{source}: {exact:?}");
                let error = script
                    .call("run", std::slice::from_ref(&input), options)
                    .unwrap_err();
                assert_eq!(error.kind, ErrorKind::Type, "{source}: {error}");
                assert!(
                    error.message.contains("return value for run expected int"),
                    "{source}: {error}"
                );
            }
        }
    }
}
