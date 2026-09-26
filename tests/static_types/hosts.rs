//! Globals and capabilities a host declares: the checker types each name by
//! its declaration, and a bare name no declaration or scope explains is an
//! error.

use super::support::{describe, error, errors_with};
use vibescript::{
    Capability, Engine, HostMethod, Signature, SignatureParam, Value, diagnostic::Diagnostic,
};

/// The codes of the errors `engine` finds in `source`.
#[track_caller]
fn codes_with(engine: &Engine, source: &str, expected: &[&str]) -> Vec<Diagnostic> {
    let found = errors_with(engine, source);
    let actual: Vec<String> = found.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(
        actual,
        expected,
        "in\n{source}\nfound:\n{}",
        describe(source, &found)
    );
    found
}

fn signed(name: &str, params: &[(&str, &str)], result: &str) -> HostMethod {
    HostMethod::new(name, |ctx, _, _| ctx.bytes(b"ok"))
        .with_signature(Signature {
            params: params
                .iter()
                .map(|(name, ty)| SignatureParam {
                    name: (*name).into(),
                    ty: (*ty).into(),
                    optional: false,
                })
                .collect(),
            result: result.into(),
            accepts_block: false,
        })
        .unwrap()
}

/// An engine declaring a typed `config` global and an `SMS` capability with
/// a signed `send`, an unsigned `log` and a `region` datum.
fn engine() -> Engine {
    let mut engine = Engine::new();
    engine
        .declare_global("config", "{ region: string, limit: int }")
        .unwrap();
    engine.declare_global("payload", "").unwrap();
    let send = signed("SMS.send", &[("message", "string")], "string");
    let log = HostMethod::new("SMS.log", |_, _, _| Ok(Value::nil()));
    engine
        .declare_capability(&Capability::from_value(
            "SMS",
            Value::object(vec![
                (b"send".to_vec(), send.value()),
                (b"log".to_vec(), log.value()),
                (b"region".to_vec(), Value::bytes("eu")),
            ]),
        ))
        .unwrap();
    engine
}

#[test]
fn an_undeclared_bare_name_is_an_error() {
    error(
        "def run -> any\n  config\nend\n",
        "V0201",
        "the host declares no global or capability of that name",
    );
    error(
        "def run -> any\n  Config\nend\n",
        "V0201",
        "declares no global",
    );
    // A removed namespace is left to its removed-spelling diagnostics.
    super::support::clean("def run -> any\n  Regexp\nend\n");
}

#[test]
fn a_declared_global_has_its_type() {
    let engine = engine();
    codes_with(
        &engine,
        "def run -> int\n  config[\"limit\"] * 2\nend\n",
        &[],
    );
    codes_with(
        &engine,
        "def run -> string\n  config[\"region\"].upcase\nend\n",
        &[],
    );
    codes_with(
        &engine,
        "def run -> string\n  config[\"limit\"]\nend\n",
        &["V0101"],
    );
    codes_with(
        &engine,
        "def run -> any\n  config[\"zone\"]\nend\n",
        &["V0110"],
    );
}

#[test]
fn supplied_globals_shadow_namespace_declarations() {
    let mut engine = Engine::new();
    engine.declare_global("Math", "int").unwrap();
    engine.declare_global("Box", "array<int>").unwrap();
    let source = "class Box\nend\nMath + Box.length\n";
    codes_with(&engine, source, &[]);
    codes_with(&engine, "class Box\nend\nBox.new\n", &["V0203"]);
    let result = engine
        .compile(source)
        .unwrap()
        .run(vibescript::CallOptions {
            globals: [
                ("Math".into(), Value::int(7)),
                ("Box".into(), Value::array(vec![Value::int(1)])),
            ]
            .into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(result.value.as_int(), Some(8));
}

#[test]
fn capability_data_is_readable_but_cannot_take_arguments() {
    let engine = engine();
    codes_with(&engine, "SMS.region\n", &[]);
    codes_with(&engine, "SMS.region(1)\n", &["V0310"]);
}

#[test]
fn a_global_declared_without_a_type_is_any() {
    let engine = engine();
    codes_with(&engine, "def run -> any\n  payload\nend\n", &[]);
    codes_with(
        &engine,
        "def run -> int\n  payload.length\nend\n",
        &["V0106"],
    );
    codes_with(
        &engine,
        "def run -> int\n  payload.as(array<int>).length\nend\n",
        &[],
    );
}

#[test]
fn a_declared_global_is_not_a_function() {
    let found = codes_with(&engine(), "def run -> any\n  config(1)\nend\n", &["V0310"]);
    assert!(
        found[0]
            .message
            .contains("the host declares, not a function"),
        "{}",
        found[0].message
    );
}

#[test]
fn a_declared_capability_is_typed_by_its_members() {
    let engine = engine();
    codes_with(
        &engine,
        "def run -> string\n  SMS.send(\"hello\")\nend\n",
        &[],
    );
    codes_with(&engine, "def run -> string\n  SMS.region\nend\n", &[]);
    codes_with(&engine, "def run -> any\n  SMS.log(1, :a, [2])\nend\n", &[]);
    codes_with(
        &engine,
        "def run -> string\n  SMS.send(1)\nend\n",
        &["V0101"],
    );
    codes_with(
        &engine,
        "def run -> int\n  SMS.send(\"a\")\nend\n",
        &["V0101"],
    );
    codes_with(
        &engine,
        "def run -> any\n  SMS.fax(\"a\")\nend\n",
        &["V0203"],
    );
    // An unsigned method's result is `any`.
    codes_with(
        &engine,
        "def run -> int\n  SMS.log.length\nend\n",
        &["V0106"],
    );
}

#[test]
fn a_capability_named_like_a_removed_member_keeps_its_methods() {
    let mut engine = Engine::new();
    let send = HostMethod::new("Mail.send", |_, _, _| Ok(Value::nil()));
    let size = HostMethod::new("Mail.size", |_, _, _| Ok(Value::int(1)));
    let list = HostMethod::new("Mail.list", |_, _, _| Ok(Value::nil()));
    engine
        .declare_capability(&Capability::from_value(
            "Mail",
            Value::object(vec![
                (b"send".to_vec(), send.value()),
                (b"size".to_vec(), size.value()),
                (b"list".to_vec(), list.value()),
            ]),
        ))
        .unwrap();
    codes_with(
        &engine,
        "def run\n  Mail.send(\"to\", \"body\")\n  Mail.size()\n  Mail.list()\nend\n",
        &[],
    );
}

#[test]
fn a_callable_capability_is_a_function() {
    let mut engine = Engine::new();
    let notify = signed("notify", &[("message", "string")], "string");
    engine
        .declare_capability(&Capability::from_value("notify", notify.value()))
        .unwrap();
    codes_with(
        &engine,
        "def run -> string\n  notify(\"hello\")\nend\n",
        &[],
    );
    codes_with(&engine, "def run -> string\n  notify(1)\nend\n", &["V0101"]);
}

#[test]
fn a_factory_capability_is_any() {
    let mut engine = Engine::new();
    engine
        .declare_capability(&Capability::new("Clock", |ctx| ctx.bytes(b"now")))
        .unwrap();
    codes_with(&engine, "def run -> any\n  Clock\nend\n", &[]);
    codes_with(&engine, "def run -> any\n  Clock.tick\nend\n", &["V0106"]);
}

#[test]
fn a_capability_of_data_has_the_type_its_template_shows() {
    let mut engine = Engine::new();
    engine
        .declare_capability(&Capability::from_value(
            "Settings",
            Value::hash(vec![(b"retries".to_vec(), Value::int(3))]),
        ))
        .unwrap();
    codes_with(
        &engine,
        "def run -> int\n  Settings[\"retries\"] + 1\nend\n",
        &[],
    );
    codes_with(
        &engine,
        "def run -> any\n  Settings[\"other\"]\nend\n",
        &["V0110"],
    );
}

#[test]
fn locals_shadow_declared_names_and_globals_precede_functions() {
    let engine = engine();
    codes_with(
        &engine,
        "def run -> int\n  config = 3\n  config + 1\nend\n",
        &[],
    );
    codes_with(
        &engine,
        "def config -> int\n  1\nend\ndef run -> int\n  config + 1\nend\n",
        &["V0108"],
    );
}

#[test]
fn required_files_see_the_declared_names() {
    let limits = "def doubled -> int\n  config[\"limit\"] * 2\nend\n";
    let (mut engine, directory) = super::modules::engine(&[("limits.vibe", limits)]);
    let source = "def run -> int\n  require(\"limits\")\n  doubled\nend\n";
    let found = codes_with(&engine, source, &["V0201"]);
    assert_eq!(found[0].file.as_deref(), Some(b"limits.vibe".as_slice()));
    engine
        .declare_global("config", "{ region: string, limit: int }")
        .unwrap();
    codes_with(&engine, source, &[]);
    let script = engine.compile(source).unwrap();
    let options = vibescript::CallOptions {
        globals: [(
            "config".to_owned(),
            Value::hash(vec![
                (b"region".to_vec(), Value::bytes("eu")),
                (b"limit".to_vec(), Value::int(21)),
            ]),
        )]
        .into(),
        ..vibescript::CallOptions::default()
    };
    let outcome = script.call("run", &[], options).unwrap();
    assert_eq!(outcome.value.as_int(), Some(42));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn declared_capability_methods_keep_their_contracts_and_names() {
    let mut engine = Engine::new();
    let cap = Capability::from_value(
        "host",
        Value::object(vec![
            (
                b"send".to_vec(),
                signed("send", &[("message", "string")], "string").value(),
            ),
            (
                b"clone".to_vec(),
                signed("clone", &[("message", "string")], "string").value(),
            ),
        ]),
    );
    engine.declare_capability(&cap).unwrap();
    codes_with(&engine, "host.send(\"x\")\nhost.clone(\"x\")\n", &[]);
    codes_with(&engine, "host.send(1)\n", &["V0101"]);
    codes_with(&engine, "host.clone(1)\n", &["V0101"]);
}

#[test]
fn nested_method_objects_are_capability_namespaces() {
    let mut engine = Engine::new();
    let cap = Capability::from_value(
        "cap",
        Value::object(vec![(
            b"inner".to_vec(),
            Value::object(vec![(
                b"m".to_vec(),
                signed("m", &[("message", "string")], "string").value(),
            )]),
        )]),
    );
    engine.declare_capability(&cap).unwrap();
    codes_with(&engine, "cap.inner.m(\"x\")\n", &[]);
    codes_with(&engine, "cap.inner.m(1)\n", &["V0101"]);
    codes_with(&engine, "cap.send(1)\n", &["V0203"]);
    codes_with(&engine, "cap.clone\n", &["V0203"]);
    let result = engine
        .compile("cap.inner.m(\"x\")\n")
        .unwrap()
        .run(vibescript::CallOptions {
            capabilities: vec![cap],
            ..vibescript::CallOptions::default()
        })
        .unwrap();
    assert_eq!(result.value.as_bytes(), Some(b"ok".as_slice()));
    let prelude = engine.prelude(&vibescript::CallOptions::default());
    assert!(prelude.contains("  module inner\n    def m(message: string) -> string\n  end\n"));
    assert_eq!(
        vibescript::signatures::Table::parse(&prelude)
            .unwrap()
            .to_string(),
        prelude
    );
}
