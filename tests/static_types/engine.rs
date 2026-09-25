//! The static mode of the engine: compile errors carry the diagnostics, the
//! default mode is unchanged, host functions are typed by their signatures,
//! and the checker reports each member call's receiver type.

use super::support::errors_with;
use vibescript::{
    CallOptions, Engine, ErrorKind, HostMethod, Signature, SignatureParam, Value, diagnostic::Code,
};

#[test]
fn a_program_with_type_errors_does_not_compile_in_static_mode() {
    let source = "def run -> int\n  count = 1\n  count = \"one\"\n  count\nend\n";
    let mut engine = Engine::new();
    assert!(
        engine.compile(source).is_ok(),
        "the default mode still compiles it"
    );
    engine.set_static_types(true);
    let error = engine.compile(source).err().expect("a type error");
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(error.diagnostics().len(), 1);
    assert_eq!(error.diagnostics()[0].code, Code::LOCAL_TYPE_CHANGED);
    assert!(
        error.message.starts_with("error[V0102]: "),
        "{}",
        error.message
    );
    let position = error.diagnostic.as_ref().unwrap().position;
    assert_eq!((position.line, position.column), (3, 11));
}

#[test]
fn a_well_typed_program_compiles_and_runs_in_static_mode() {
    let mut engine = Engine::new();
    engine.set_static_types(true);
    let script = engine
        .compile("def add(a: int, b: int) -> int\n  a + b\nend\n")
        .unwrap();
    let outcome = script
        .call(
            "add",
            &[Value::int(20), Value::int(22)],
            CallOptions::default(),
        )
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(42));
}

#[test]
fn signed_host_functions_are_typed_and_unsigned_ones_take_any() {
    let mut engine = Engine::new();
    let charge = HostMethod::new("charge", |ctx, _, _| ctx.bytes(b"ok"))
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "cents".into(),
                ty: "int".into(),
                optional: false,
            }],
            result: "string".into(),
            accepts_block: false,
        })
        .unwrap();
    engine.register_method("charge", charge);
    engine.register("lookup", |_, _| Ok(Value::nil()));
    assert!(errors_with(&engine, "def run -> string\n  charge(5).upcase\nend\n").is_empty());
    let found = errors_with(&engine, "def run -> string\n  charge(\"5\")\nend\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, Code::TYPE_MISMATCH);
    assert!(errors_with(&engine, "def run -> any\n  lookup(1, \"x\")\nend\n").is_empty());
    let found = errors_with(&engine, "def run -> int\n  lookup(1).length\nend\n");
    assert_eq!(found[0].code, Code::ANY_USE);
}

#[test]
fn the_checker_reports_each_member_calls_receiver_type() {
    let source = "def f(h: hash<string, int>, t: time, xs: array<int>?) -> bool\n  n = t.day\n  h.key?(\"a\") && xs&.include?(n) == true\nend\n";
    let checked = Engine::new().type_check(source).unwrap();
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    let at = |name: &str| source.find(name).unwrap();
    let day = checked.calls.receiver_at(at("day")).unwrap();
    assert_eq!(day.name(), "time");
    assert!(day.is("time"));
    let key = checked.calls.receiver_at(at("key?")).unwrap();
    assert_eq!(key.name(), "hash<string, int>");
    assert_eq!(key.bases(), ["hash"]);
    let include = checked.calls.receiver_at(at("include?")).unwrap();
    assert_eq!(include.name(), "array<int>");
    assert!(include.is("array"));
    assert!(checked.calls.receiver_at(at("f(")).is_none());
}

#[test]
fn diagnostics_print_as_json() {
    let source = "x = 7 / 2\n";
    let checked = Engine::new().type_check(source).unwrap();
    let json = checked.diagnostics[0].to_json(source);
    assert!(
        json.starts_with(
            "{\"code\":\"V0109\",\"name\":\"integer-division\",\"severity\":\"error\""
        ),
        "{json}"
    );
    assert!(json.contains("\"replacement\":\"//\""), "{json}");
}
