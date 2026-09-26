use serde_json::Value as Json;
use std::{fs, path::Path};
use vibescript::{CallOptions, Limits};

mod common;

#[path = "../examples/support/mod.rs"]
mod support;

#[test]
fn unchanged_site_examples_match_go_results() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/site");
    let cases: Vec<Json> =
        serde_json::from_slice(&fs::read(root.join("cases.json")).unwrap()).unwrap();
    let settings: Json =
        serde_json::from_slice(&fs::read(root.join("harness.json")).unwrap()).unwrap();
    for case in cases {
        let path = case["path"].as_str().unwrap();
        let source = fs::read_to_string(root.join(path)).unwrap();
        let Some(mut engine) =
            common::fixture_engine(settings[path].get("static_error"), &source, path)
        else {
            continue;
        };
        if let Some(byte) = case.get("entropy_byte") {
            let byte = u8::try_from(byte.as_u64().unwrap()).unwrap();
            engine.set_random_source(move |_, output| {
                output.fill(byte);
                Ok(output.len())
            });
        }
        let capabilities: Vec<_> = settings[path]["notifications"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|name| support::notification(name.as_str().unwrap()).unwrap())
            .collect();
        for capability in &capabilities {
            engine.declare_capability(capability).unwrap();
        }
        let script = engine
            .compile(&source)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let options = CallOptions {
            capabilities,
            limits: Limits {
                steps: Some(5_000_000),
                memory_bytes: Some(64 << 20),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let output = script
            .call("run", &[], options.clone())
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let encoded = support::encode(
            &output.value,
            settings[path]["result_encoding"].as_str().unwrap_or(""),
            options,
        )
        .unwrap();
        let actual: Json = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(actual, case["expected"], "{path}");
    }
}
