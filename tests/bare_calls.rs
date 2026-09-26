//! Bare calls follow ADR-008 for script and host functions alike.

mod common;
use vibescript::{CallOptions, Capability, HostMethod, Signature, Value};

#[test]
fn optional_script_parameters_are_bound_by_a_bare_call() {
    let mut engine = common::static_engine();
    let source = "def g(x: int = 4) -> int\n  x\nend\ng + 1\n";
    for static_types in [false, true] {
        engine.set_static_types(static_types);
        let result = engine
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap();
        assert_eq!(result.value.as_int(), Some(5));
    }
    assert!(
        engine
            .compile("def g(x: int) -> int\n  x\nend\ng\n")
            .is_err()
    );
}

#[test]
fn required_file_bare_calls_can_receive_a_member_call() {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".cache/tmp")
        .join(format!("bare-calls-{}", common::process_id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("worker.vibe"),
        "def value(x: int = 7) -> int\n x\nend\ndef label -> string\n value.to_s\nend\n",
    )
    .unwrap();
    let mut engine = common::static_engine();
    engine
        .set_module_config(vibescript::ModuleConfig {
            paths: vec![dir.clone()],
            ..Default::default()
        })
        .unwrap();
    let script = engine.compile("require(\"worker\").label\n").unwrap();
    let result = script.run(CallOptions::default());
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(result.unwrap().value.as_bytes(), Some(b"7".as_slice()));
}

#[test]
fn host_functions_and_capability_methods_run_without_parentheses() {
    let tick = HostMethod::new("tick", |_, _, _| Ok(Value::int(7)))
        .with_signature(Signature {
            result: "int".into(),
            ..Signature::default()
        })
        .unwrap();
    let cap = Capability::from_value("cap", Value::object(vec![(b"tick".to_vec(), tick.value())]));
    let mut engine = common::static_engine();
    engine.register_method("tick", tick);
    engine.declare_capability(&cap).unwrap();
    let source = "tick + cap.tick\n";
    assert!(engine.type_check(source).unwrap().diagnostics.is_empty());
    let result = engine
        .compile(source)
        .unwrap()
        .run(CallOptions {
            capabilities: vec![cap],
            ..CallOptions::default()
        })
        .unwrap();
    assert_eq!(result.value.as_int(), Some(14));
    assert!(engine.compile("tick(1)\n").is_err());
}
