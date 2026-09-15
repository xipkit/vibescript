use std::sync::{Arc, Mutex};
use vibescript::{
    CallOptions, CancellationToken, Engine, ErrorKind, Limits, Value, stringify_json,
};

type Capture = Option<Arc<Mutex<Vec<u8>>>>;

fn engine(case: &serde_json::Value) -> (Engine, [Capture; 2]) {
    let mut engine = Engine::new();
    if let Some(byte) = case.get("entropy_byte") {
        let byte = u8::try_from(byte.as_u64().unwrap()).unwrap();
        engine.set_random_source(move |_, output| {
            output.fill(byte);
            Ok(output.len())
        });
    }
    let output = ["stdout", "stderr"].map(|name| {
        case[name]
            .as_bool()
            .unwrap_or(false)
            .then(|| Arc::new(Mutex::new(Vec::new())))
    });
    if let Some(buffer) = &output[0] {
        let buffer = buffer.clone();
        engine.set_output_writer(move |_, bytes| {
            buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        });
    }
    if let Some(buffer) = &output[1] {
        let buffer = buffer.clone();
        engine.set_error_writer(move |_, bytes| {
            buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        });
    }
    (engine, output)
}

fn call(
    script: &vibescript::Script,
    case: &serde_json::Value,
    options: CallOptions,
) -> vibescript::Result<vibescript::Outcome> {
    let function = case["function"].as_str().unwrap_or("run");
    if function == "__main__" {
        script.run(options)
    } else {
        script.call(function, &[Value::nil()], options)
    }
}

#[test]
fn language_conformance() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("language.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let source = case["source"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("def run(input)\n{}\nend", case["body"].as_str().unwrap()));
        let (engine, output) = engine(case);
        let script = engine
            .compile(&source)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut options = CallOptions::default();
        if let Some(steps) = case["steps"].as_u64() {
            options.limits.steps = Some(steps);
        }
        let result = call(&script, case, options).unwrap_or_else(|e| panic!("{name}: {e}"));
        let encoded = stringify_json(&result.value, CallOptions::default()).unwrap();
        let actual: serde_json::Value =
            serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
        assert_eq!(actual, case["expected"], "{name}");
        for (field, buffer) in ["stdout_hex", "stderr_hex"].into_iter().zip(output) {
            if let Some(buffer) = buffer {
                let expected = case[field].as_str().unwrap();
                assert_eq!(expected.len() % 2, 0, "{name} {field}");
                let expected: Vec<_> = expected
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                assert_eq!(*buffer.lock().unwrap(), expected, "{name} {field}");
            }
        }
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
        let script = engine(case)
            .0
            .compile(&source)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(
            call(&script, case, CallOptions::default()).is_err(),
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
