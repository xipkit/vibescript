//! Globals and capabilities a host declares: each call must supply them as
//! declared, checked before any script code runs, and the prelude lists
//! them.

mod common;

use vibescript::{
    CallOptions, Capability, Engine, ErrorKind, HostMethod, Signature, SignatureParam, Value,
    signatures::Table,
};

fn signed(name: &str, ty: &str) -> HostMethod {
    HostMethod::new(name, |ctx, _, _| ctx.bytes(b"queued"))
        .with_signature(Signature {
            params: vec![SignatureParam {
                name: "message".into(),
                ty: ty.into(),
                optional: false,
            }],
            result: "string".into(),
            accepts_block: false,
        })
        .unwrap()
}

fn sms(send: &HostMethod, region: Value) -> Capability {
    Capability::from_value(
        "SMS",
        Value::object(vec![
            (b"send".to_vec(), send.value()),
            (b"region".to_vec(), region),
        ]),
    )
}

fn globals(entries: Vec<(&str, Value)>) -> CallOptions {
    CallOptions {
        globals: entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
        ..CallOptions::default()
    }
}

#[test]
fn declared_globals_are_checked_when_a_call_starts() {
    let mut engine = Engine::new();
    engine.declare_global("limit", "int").unwrap();
    let script = engine
        .compile("def doubled -> int\n  limit * 2\nend\n")
        .unwrap();
    let outcome = script
        .call("doubled", &[], globals(vec![("limit", Value::int(21))]))
        .unwrap();
    assert_eq!(outcome.value.as_int(), Some(42));

    let error = script
        .call("doubled", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert_eq!(
        error.message,
        "missing global limit, which the host declares"
    );

    let error = script
        .call("doubled", &[], globals(vec![("limit", Value::bytes("21"))]))
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(error.message, "global limit expected int, got string");
}

#[test]
fn a_global_declared_without_a_type_takes_any_value_but_must_be_supplied() {
    let mut engine = Engine::new();
    engine.declare_global("payload", "").unwrap();
    let script = engine.compile("payload").unwrap();
    let outcome = script
        .run(globals(vec![("payload", Value::bytes("x"))]))
        .unwrap();
    assert_eq!(outcome.value.as_bytes(), Some(b"x".as_slice()));
    let error = script.run(CallOptions::default()).unwrap_err();
    assert_eq!(
        error.message,
        "missing global payload, which the host declares"
    );
}

/// A global the host does not declare is not in scope, even though a call
/// could supply it, so the program does not compile.
#[test]
fn undeclared_globals_are_refused() {
    let mut engine = Engine::new();
    engine.declare_global("limit", "int").unwrap();
    let source = "limit + extra";
    let error = engine.compile(source).err().unwrap();
    assert_eq!(common::codes(&error), ["V0201"]);
    assert_eq!(
        error.diagnostics()[0].span.start,
        source.find("extra").unwrap()
    );
}

#[test]
fn declared_capabilities_must_provide_their_members() {
    let send = signed("SMS.send", "string");
    let declared = sms(&send, Value::bytes("eu"));
    let mut engine = Engine::new();
    engine.declare_capability(&declared).unwrap();
    let script = engine
        .compile("def notify -> string\n  SMS.send(SMS.region)\nend\n")
        .unwrap();
    let grant = |capability: Capability| CallOptions {
        capabilities: vec![capability],
        ..CallOptions::default()
    };
    let outcome = script.call("notify", &[], grant(declared.clone())).unwrap();
    assert_eq!(outcome.value.as_bytes(), Some(b"queued".as_slice()));

    // The same members from a factory pass too.
    let factory_send = send.clone();
    let factory = Capability::new("SMS", move |_| {
        Ok(Value::object(vec![
            (b"send".to_vec(), factory_send.value()),
            (b"region".to_vec(), Value::bytes("us")),
        ]))
    });
    assert!(script.call("notify", &[], grant(factory)).is_ok());

    let error = script
        .call("notify", &[], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert_eq!(
        error.message,
        "missing capability SMS, which the host declares"
    );

    let lacking =
        Capability::from_value("SMS", Value::object(vec![(b"send".to_vec(), send.value())]));
    let error = script.call("notify", &[], grant(lacking)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(
        error.message,
        "capability SMS lacks the member region the host declares"
    );

    let other = sms(&signed("SMS.send", "int"), Value::bytes("eu"));
    let error = script.call("notify", &[], grant(other)).unwrap_err();
    assert_eq!(
        error.message,
        "capability SMS member send does not have the signature the host declares for it"
    );

    let unsigned = sms(
        &HostMethod::new("SMS.send", |ctx, _, _| ctx.bytes(b"queued")),
        Value::bytes("eu"),
    );
    assert!(script.call("notify", &[], grant(unsigned)).is_err());

    let region = sms(&send, Value::int(1));
    let error = script.call("notify", &[], grant(region)).unwrap_err();
    assert_eq!(
        error.message,
        "capability SMS member region expected string, got int"
    );

    let data = Capability::from_value("SMS", Value::bytes("sms"));
    let error = script.call("notify", &[], grant(data)).unwrap_err();
    assert_eq!(
        error.message,
        "capability SMS must be an object with the members the host declares, got string"
    );
}

#[test]
fn the_last_grant_of_a_declared_capability_is_the_one_checked() {
    let send = signed("SMS.send", "string");
    let declared = sms(&send, Value::bytes("eu"));
    let mut engine = Engine::new();
    engine.declare_capability(&declared).unwrap();
    let script = engine.compile("SMS.region").unwrap();
    let wrong = Capability::from_value("SMS", Value::bytes("sms"));
    let options = CallOptions {
        capabilities: vec![wrong.clone(), declared.clone()],
        ..CallOptions::default()
    };
    let outcome = script.run(options).unwrap();
    assert_eq!(outcome.value.as_bytes(), Some(b"eu".as_slice()));
    let options = CallOptions {
        capabilities: vec![declared, wrong],
        ..CallOptions::default()
    };
    assert!(script.run(options).is_err());
}

#[test]
fn a_global_may_supply_a_declared_capability() {
    let send = signed("SMS.send", "string");
    let declared = sms(&send, Value::bytes("eu"));
    let mut engine = Engine::new();
    engine.declare_capability(&declared).unwrap();
    let script = engine.compile("SMS.region").unwrap();
    let value = Value::object(vec![
        (b"send".to_vec(), send.value()),
        (b"region".to_vec(), Value::bytes("ap")),
    ]);
    let outcome = script.run(globals(vec![("SMS", value)])).unwrap();
    assert_eq!(outcome.value.as_bytes(), Some(b"ap".as_slice()));
    let error = script
        .run(globals(vec![("SMS", Value::int(1))]))
        .unwrap_err();
    assert_eq!(
        error.message,
        "global SMS must be an object with the members the host declares, got int"
    );
}

#[test]
fn a_factory_capability_declares_its_name() {
    let clock = Capability::new("Clock", |ctx| ctx.bytes(b"tick"));
    let mut engine = Engine::new();
    engine.declare_capability(&clock).unwrap();
    let script = engine.compile("Clock").unwrap();
    let options = CallOptions {
        capabilities: vec![clock],
        ..CallOptions::default()
    };
    assert_eq!(
        script.run(options).unwrap().value.as_bytes(),
        Some(b"tick".as_slice())
    );
    assert!(script.run(CallOptions::default()).is_err());
}

#[test]
fn declarations_refuse_types_a_host_cannot_supply() {
    let mut engine = Engine::new();
    let error = engine.declare_global("limit", "array<").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Argument);
    assert!(
        error.message.starts_with("declared type of global limit: "),
        "{}",
        error.message
    );
    let error = engine
        .declare_global("invoice", "array<Invoice>")
        .unwrap_err();
    assert_eq!(
        error.message,
        "declared type of global invoice names Invoice, which a host cannot supply; declare it with builtin types"
    );
}

#[test]
fn a_later_declaration_replaces_an_earlier_one() {
    let mut engine = Engine::new();
    engine.declare_global("limit", "string").unwrap();
    engine.declare_global("limit", "int").unwrap();
    assert!(engine.compile("def run -> int\n  limit + 1\nend\n").is_ok());
}

#[test]
fn the_prelude_lists_declared_names() {
    let send = signed("SMS.send", "string");
    let mut engine = Engine::new();
    engine
        .declare_global("config", "{ region: string, limit: int }")
        .unwrap();
    engine.declare_global("payload", "").unwrap();
    engine
        .declare_capability(&sms(&send, Value::bytes("eu")))
        .unwrap();
    // A per-call value of a declared name does not change its declaration.
    let options = CallOptions {
        globals: [("config".to_owned(), Value::int(1))].into(),
        capabilities: vec![Capability::new("SMS", |ctx| ctx.bytes(b""))],
        ..CallOptions::default()
    };
    let prelude = engine.prelude(&options);
    let host = prelude
        .strip_prefix(&vibescript::signatures::prelude())
        .expect("the builtin prelude comes first");
    assert_eq!(
        host,
        "\n# A capability the host declares.\nmodule SMS\n  def send(message: string) -> string\n  region: string\nend\n\n# A global the host declares.\nconfig: { limit: int, region: string }\n\n# A global the host declares.\npayload: any\n"
    );
    assert!(Table::parse(&prelude).is_ok());
}

/// Every host feature at once, with every spelling a host may publish; the
/// prelude must parse whatever the host declares or grants.
#[test]
fn the_prelude_parses_with_every_host_feature() {
    let plain = || HostMethod::new("host.plain", |_, _, _| Ok(Value::nil()));
    let block = || HostMethod::new_with_block("host.block", |_, _, _| Ok(Value::nil()));
    let object = |extra: Vec<(Vec<u8>, Value)>| {
        let mut members = vec![
            (b"send".to_vec(), signed("host.send", "string").value()),
            (b"plain".to_vec(), plain().value()),
            (b"each".to_vec(), block().value()),
            (b"ok?".to_vec(), plain().value()),
            (b"save!".to_vec(), plain().value()),
            (b"nil".to_vec(), plain().value()),
            (b"end".to_vec(), plain().value()),
            (b"region".to_vec(), Value::bytes("eu")),
            (b"true".to_vec(), Value::int(1)),
            (b"def".to_vec(), Value::int(1)),
            (b"module".to_vec(), Value::int(1)),
            (b"end_at".to_vec(), Value::int(1)),
            (b"with space".to_vec(), Value::int(1)),
            (b"with-dash".to_vec(), Value::float(1.5)),
            (b"ready?".to_vec(), Value::boolean(true)),
            (b"".to_vec(), Value::nil()),
            ("é".as_bytes().to_vec(), Value::array(vec![Value::int(1)])),
            (
                b"nested".to_vec(),
                Value::object(vec![
                    (b"inner".to_vec(), plain().value()),
                    (
                        b"data".to_vec(),
                        Value::hash(vec![(b"k".to_vec(), Value::int(1))]),
                    ),
                ]),
            ),
        ];
        members.extend(extra);
        Value::object(members)
    };
    let mut engine = Engine::new();
    engine.register("callback", |_, _| Ok(Value::nil()));
    engine.register_with_keywords("keywords", |_, _, _| Ok(Value::nil()));
    engine.register_method("signed", signed("signed", "int"));
    engine.register_method("block", block());
    engine.register_method("ready?", plain());
    engine.register_method("é?", plain());
    engine.register_method("bad??", plain());
    engine.register_method("setter=", plain());
    for (name, ty) in [
        ("g_int", "int"),
        ("g_any", ""),
        (
            "g_shape",
            "{ items: array<int>, name: string?, \"odd key\": bool }",
        ),
        ("g_tuple", "[int, string]"),
        ("g_union", "int | string | nil"),
        ("g_hash", "hash<string, array<int>>"),
        ("é", "int"),
    ] {
        engine.declare_global(name, ty).unwrap();
    }
    // Names no script could read are refused, so they never reach the prelude.
    for name in ["with space", "", "3d", "end", "nil"] {
        let error = Engine::new().declare_global(name, "int").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument, "{name}: {error}");
    }
    engine
        .declare_capability(&Capability::from_value("Host", object(Vec::new())))
        .unwrap();
    engine
        .declare_capability(&Capability::from_value("ok?", plain().value()))
        .unwrap();
    engine
        .declare_capability(&Capability::from_value("DATA", Value::int(1)))
        .unwrap();
    engine
        .declare_capability(&Capability::new("factory", |_| Ok(Value::nil())))
        .unwrap();
    engine
        .declare_capability(&Capability::new("made?", |_| Ok(Value::nil())))
        .unwrap();
    // Grants no declaration names, which the prelude renders from their
    // values, including ones it must leave out.
    let options = CallOptions {
        capabilities: vec![
            Capability::from_value("Granted", object(Vec::new())),
            Capability::from_value("granted?", plain().value()),
            Capability::from_value(
                "BadMember",
                object(vec![(b"bad??".to_vec(), plain().value())]),
            ),
            Capability::new("granted_factory", |_| Ok(Value::nil())),
            Capability::new("bad??", |_| Ok(Value::nil())),
        ],
        globals: [
            ("data".to_owned(), object(Vec::new())),
            ("number".to_owned(), Value::int(1)),
            ("callable?".to_owned(), plain().value()),
            ("data?".to_owned(), Value::int(1)),
            ("bad??".to_owned(), plain().value()),
        ]
        .into(),
        ..CallOptions::default()
    };
    for options in [CallOptions::default(), options] {
        let prelude = engine.prelude(&options);
        let host = prelude
            .strip_prefix(&vibescript::signatures::prelude())
            .unwrap_or(&prelude);
        if let Err(error) = Table::parse(&prelude) {
            panic!("{error:?}\n{host}");
        }
        for line in [
            "def é?(",
            "é: int",
            "  def nil(",
            "  def end(",
            "  true: int",
            "  def: int",
            "  module: int",
            "  ready?: bool",
        ] {
            assert!(host.contains(line), "{line}\n{host}");
        }
        for name in ["with space", "with-dash", "bad??", "setter="] {
            assert!(!host.contains(name), "{name}\n{host}");
        }
    }
}
