use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

#[test]
fn language_conformance() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("language.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let source = case["source"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("def run(input)\n{}\nend", case["body"].as_str().unwrap()));
        let script = Engine::new()
            .compile(&source)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut options = CallOptions::default();
        if let Some(steps) = case["steps"].as_u64() {
            options.limits.steps = Some(steps);
        }
        let result = script
            .call("run", &[Value::nil()], options)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
        let actual: serde_json::Value =
            serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
        assert_eq!(actual, case["expected"], "{name}");
    }
}

#[test]
fn range_errors_and_expansion_limits() {
    for source in [
        "(1..).to_a",
        "(..2).first",
        "(1..).last",
        "(..2).last(0)",
        "(1..).size",
        "(1..3).first(-1)",
        "(1..3).last(1.5)",
        "(nil..3).to_a",
        "(1.5..3).to_a",
        "[1].slice(\"1\")",
        "[1].slice(0,nil)",
    ] {
        let script = Engine::new().compile(source).unwrap();
        assert!(script.run(CallOptions::default()).is_err(), "{source}");
    }
    for source in ["(1..1000000000).to_a", "(1..).first(1000000000)"] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions {
            limits: Limits {
                steps: Some(100),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.run(options).unwrap_err().kind,
            ErrorKind::Steps,
            "{source}"
        );
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(4096),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.run(options).unwrap_err().kind,
            ErrorKind::Memory,
            "{source}"
        );
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            script
                .run(CallOptions {
                    cancellation: token,
                    ..CallOptions::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
    }
}

#[test]
fn byte_slices_preserve_partial_and_invalid_utf8() {
    let script = Engine::new()
        .compile("def run(input)\ninput.byteslice(1,2)\nend")
        .unwrap();
    let result = script
        .call(
            "run",
            &[Value::bytes(vec![b'a', 0xff, 0xc3, 0xa9])],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some([0xff, 0xc3].as_slice()));
}

#[test]
fn language_runtime_rejections() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("language-errors.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let source = case["source"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("def run(input)\n{}\nend", case["body"].as_str().unwrap()));
        let script = Engine::new()
            .compile(&source)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            script
                .call("run", &[Value::nil()], CallOptions::default())
                .is_err(),
            "{name}"
        );
    }
}

#[test]
fn collection_expansion_and_temporary_storage_are_accounted() {
    for source in [
        "a=(1..100).to_a\na.window(50)",
        "a=(1..100).to_a\na.zip(a,a,a,a,a,a,a,a,a)",
        "s=\"a\"*4096\ns.reverse",
        "s=\"a\"*4096\ns.chars",
        "a=[1]\ni=0\nwhile i<20\na=[a,a]\ni+=1\nend\na.flatten",
        "a=[1]\ni=0\nwhile i<20\na=[a,a]\ni+=1\nend\na.to_s",
    ] {
        let script = Engine::new().compile(source).unwrap();
        let options = CallOptions {
            limits: Limits {
                memory_bytes: Some(12000),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        assert_eq!(
            script.run(options).unwrap_err().kind,
            ErrorKind::Memory,
            "{source}"
        );
    }
    let script = Engine::new().compile("(1..400).to_a.uniq").unwrap();
    let options = CallOptions {
        limits: Limits {
            steps: Some(4000),
            ..Limits::default()
        },
        ..CallOptions::default()
    };
    assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Steps);
}
