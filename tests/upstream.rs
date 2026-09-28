use serde_json::Value as Json;
use std::{fs, path::Path};
use vibescript::{CallOptions, Engine, parse_json, stringify_json};

#[test]
fn original_vibescript_examples() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/upstream");
    let cases: Vec<Json> =
        serde_json::from_slice(&fs::read(root.join("cases.json")).unwrap()).unwrap();
    for case in cases {
        let path = case["path"].as_str().unwrap();
        let function = case["function"].as_str().unwrap();
        let source = fs::read_to_string(root.join(path)).unwrap();
        let script = Engine::new()
            .compile(&source)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let args: Vec<_> = case["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| {
                parse_json(&serde_json::to_vec(v).unwrap(), CallOptions::default())
                    .unwrap()
                    .value
            })
            .collect();
        let output = script
            .call(function, &args, CallOptions::default())
            .unwrap_or_else(|e| panic!("{path}::{function}: {e}"));
        let encoded = stringify_json(&output.value, CallOptions::default()).unwrap();
        let actual: Json = serde_json::from_slice(encoded.value.as_bytes().unwrap()).unwrap();
        assert_eq!(actual, case["expected"], "{path}::{function}({args:?})");
    }
}
