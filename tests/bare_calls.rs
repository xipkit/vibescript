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
