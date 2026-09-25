use super::{Item, Member, ParamKind, Replacement, Table, Type, renames, table};
use crate::{CallOptions, Engine, Value, builtin::Global, members::names::candidates::*};
use std::collections::{BTreeMap, BTreeSet};

/// Every construct of the signature format, in printed form.
const SAMPLE: &str = "# A sample table.

type ordered = int | string

# Prints.
def show(*values: array<any>)

def pick(from: int = 1, to?: int, strict: bool: = false, name: string:) -> int?

# An overload, selected by its required block.
def pick(&block: [int, string] -> int) -> [int, int?]

module Ns
  LIMIT: int
  def run<U: ordered>(input: { id: string, \"odd key\"?: U?, ... }, &block?: (U, *any) -> U | nil) -> array<U>
end

class hash<string, V?>
  def size? -> bool
  def compact -> hash<string, V>
  def each(&block: ()) -> :done
end

class T
  def end(group: (int | string)?) -> T
end
";

#[test]
fn signature_files_round_trip() {
    let table = Table::parse(SAMPLE).unwrap();
    assert_eq!(table.to_string(), SAMPLE);
    assert_eq!(table.header, ["A sample table."]);
    let mut picks = table.functions("pick");
    let pick = picks.next().unwrap();
    assert_eq!(
        picks.next().unwrap().result,
        Some(Type::Tuple(vec![
            Type::name("int"),
            Type::Optional(Box::new(Type::name("int")))
        ]))
    );
    let kinds: Vec<_> = pick
        .params
        .iter()
        .map(|param| (param.kind, param.optional))
        .collect();
    assert_eq!(
        kinds,
        [
            (ParamKind::Positional, true),
            (ParamKind::Positional, true),
            (ParamKind::Keyword, true),
            (ParamKind::Keyword, false),
        ]
    );
    assert_eq!(pick.params[0].default.as_deref(), Some("1"));
    let Some(Member::Function(run)) = table.module("Ns").unwrap().named("run").next() else {
        panic!("run is a function");
    };
    let block = run.block.as_ref().unwrap();
    assert!(block.optional);
    assert_eq!(block.params, [Type::Var("U".into())]);
    assert_eq!(block.rest, Some(Type::name("any")));
    let Some(Item::Class(class)) = table.items.get(5) else {
        panic!("a class");
    };
    assert_eq!(class.base(), "hash");
    assert_eq!(
        class.receiver,
        Type::Name(
            "hash".into(),
            vec![
                Type::name("string"),
                Type::Optional(Box::new(Type::Var("V".into())))
            ]
        )
    );
}

#[test]
fn overloads_differ_in_arity_keywords_or_blocks() {
    let source = "def f(a: int:)\n\ndef f(b: int:)\n\n\
        class hash<string, V>\n  def each(&block: (string, V))\n  def each(&block: [string, V])\n  \
        def first -> V?\n  def first(count: int) -> array<V>\n  def sub(p: string, r: string)\n  \
        def sub(p: string, &block: string -> string)\nend\n";
    let table = Table::parse(source).unwrap();
    assert_eq!(table.functions("f").count(), 2);
    assert_eq!(table.to_string(), source);
}

#[test]
fn signature_files_report_malformed_declarations() {
    for (source, line, message) in [
        (
            "def f(a?: int, b: int)",
            1,
            "required parameter b follows an optional one",
        ),
        ("def f(k: int:, a: int)", 1, "parameter a is out of order"),
        (
            "def f(*a: array<int>, *b: array<int>)",
            1,
            "parameter b is out of order",
        ),
        ("def f(a: int, a: int)", 1, "duplicate parameter a"),
        (
            "def f(a?: int = 1)",
            1,
            "a parameter with a default is written `name: T = value`",
        ),
        (
            "def f<t>(a: t)",
            1,
            "a type variable is a single capital letter",
        ),
        (
            "module M\n  A: int\n  def A -> int\nend",
            3,
            "duplicate member A",
        ),
        (
            "def f\n# floating\n\ndef g",
            2,
            "a comment must directly precede a declaration",
        ),
        (
            "def f -> int # trailing",
            1,
            "comments go on their own line",
        ),
        ("module M\n  getter x: int\nend", 2, "expected `:`"),
        (
            "class time\n  getter year: int\nend",
            2,
            "expected a member declaration or `end`",
        ),
        ("def f(x: \"a)", 1, "unterminated string"),
        (
            "def f(a: int)\ndef f(b: string)",
            2,
            "overloads of f could accept the same call",
        ),
        (
            "def f(a?: int)\ndef f",
            2,
            "overloads of f could accept the same call",
        ),
        (
            "def f(**k: hash<string, any>)\ndef f(a: int:)",
            2,
            "overloads of f could accept the same call",
        ),
        (
            "class hash<string, V>\n  def each(&block: (string, V))\n  def each(&block?: (V, V))\nend",
            3,
            "overloads of each could accept the same call",
        ),
        (
            "class array<T>\n  def f\nend\n\nclass array<T?>\n  def f -> T\nend",
            6,
            "overloads of f could accept the same call",
        ),
        (
            "class array<T>\n  NAME: int\nend",
            2,
            "expected a member declaration or `end`",
        ),
    ] {
        let error = Table::parse(source).unwrap_err();
        assert_eq!(
            (error.line, error.message.as_str()),
            (line, message),
            "{source}"
        );
    }
}

#[test]
fn the_builtin_table_round_trips() {
    let text = table().to_string();
    let reparsed = Table::parse(&text).unwrap_or_else(|error| panic!("{error}\n{text}"));
    assert_eq!(&reparsed, table());
    assert_eq!(reparsed.to_string(), text);
}

#[test]
fn the_builtin_file_is_in_printed_form() {
    assert!(
        table().to_string() == super::BUILTINS,
        "builtins.vibe differs from its printed form; replace it with `vibes prelude`"
    );
}

/// The member names the runtime serves on each receiver, keyed by the class
/// name the table uses, with `T` for the members of every value.
fn runtime_members() -> BTreeMap<&'static str, BTreeSet<String>> {
    let mut members: BTreeMap<&str, BTreeSet<String>> = [
        ("string", STRING),
        ("symbol", SYMBOL),
        ("array", ARRAY),
        ("hash", HASH),
        ("int", INT),
        ("float", FLOAT),
        ("money", MONEY),
        ("duration", DURATION),
        ("time", TIME),
        ("range", RANGE),
        ("nil", NIL),
        ("bool", BOOL),
        ("regex", REGEX),
        ("enum_type", ENUM),
        ("enum_value", ENUM_MEMBER),
        ("T", UNIVERSAL),
    ]
    .into_iter()
    .map(|(receiver, names)| {
        (
            receiver,
            names.iter().map(|&name| name.to_owned()).collect(),
        )
    })
    .collect();
    // The VM serves the checked cast `as` for every value, as it serves
    // `is_type?`, outside the member tables that suggestions draw from.
    members.get_mut("T").unwrap().insert("as".to_owned());
    // Match data and rescued errors are records whose fields are their members.
    for (receiver, source) in [
        ("match_data", "\"abc\".match(\"b\").keys"),
        (
            "error",
            "begin\n  raise \"bad\"\nrescue => error\n  error.keys\nend",
        ),
    ] {
        let script = Engine::new().compile(source).unwrap();
        let keys = script.run(CallOptions::default()).unwrap().value;
        let keys = keys.as_array().unwrap().iter().map(text);
        members.insert(receiver, keys.collect());
    }
    members
}

/// The runtime's global functions and the members of each namespace.
fn runtime_globals() -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    let mut functions = BTreeSet::new();
    let mut namespaces = BTreeMap::new();
    for global in Global::ALL {
        match global.value().as_hash() {
            Some(fields) => {
                let names = fields.iter().map(|(key, _)| text(key)).collect();
                namespaces.insert(global.name().to_owned(), names);
            }
            None => {
                functions.insert(global.name().to_owned());
            }
        }
    }
    (functions, namespaces)
}

fn text(value: &Value) -> String {
    String::from_utf8(value.as_bytes().unwrap().to_vec()).unwrap()
}

/// The table's member names for each class base, each with how many classes
/// declare it.
fn table_members() -> BTreeMap<String, BTreeMap<String, usize>> {
    let mut members: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for item in &table().items {
        if let Item::Class(class) = item {
            let names = members.entry(class.base().to_owned()).or_default();
            for member in &class.members {
                *names.entry(member.name().to_owned()).or_default() += 1;
            }
        }
    }
    members
}

fn renamed(receiver: &str, name: &str) -> bool {
    renames().iter().any(|rename| {
        (rename.receiver == receiver || rename.receiver == "T") && rename.name == name
    })
}

#[test]
fn every_runtime_member_has_one_signature_or_a_rename() {
    let runtime = runtime_members();
    let declared = table_members();
    let universal = &declared["T"];
    let mut problems = Vec::new();
    for (receiver, names) in &runtime {
        let own = declared.get(*receiver);
        for name in names {
            let own = own.and_then(|own| own.get(name)).copied().unwrap_or(0);
            let shared = universal.get(name).copied().unwrap_or(0) * usize::from(*receiver != "T");
            match (own, shared) {
                (0, 0) if renamed(receiver, name) => {}
                (0, 0) => problems.push(format!("{receiver}.{name} has no signature or rename")),
                (_, 0) | (0, _) => {}
                _ => problems.push(format!(
                    "{receiver}.{name} is declared for it and every type"
                )),
            }
        }
    }
    for (base, names) in &declared {
        for name in names.keys() {
            if base != "T" && universal.contains_key(name) {
                problems.push(format!("{base}.{name} repeats a member of every type"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_signature_names_a_runtime_member() {
    let runtime = runtime_members();
    let mut problems = Vec::new();
    for (base, names) in table_members() {
        for name in names.keys() {
            let served = if base == "T" {
                // Records such as match data serve the members of hashes.
                let kinds = runtime
                    .iter()
                    .filter(|(receiver, _)| !matches!(**receiver, "T" | "match_data" | "error"));
                runtime["T"].contains(name) || kinds.clone().all(|(_, names)| names.contains(name))
            } else {
                runtime
                    .get(base.as_str())
                    .is_some_and(|names| names.contains(name))
            };
            if !served {
                problems.push(format!("{base}.{name} is not a runtime member"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_builtin_global_has_one_signature_or_a_rename() {
    let (functions, namespaces) = runtime_globals();
    let table = table();
    let mut problems = Vec::new();
    for name in &functions {
        if table.functions(name).next().is_none() && !renamed("global", name) {
            problems.push(format!("{name} has no signature or rename"));
        }
    }
    for (namespace, members) in &namespaces {
        let module = table.module(namespace);
        for name in members {
            if module.is_none_or(|module| module.named(name).next().is_none())
                && !renamed(namespace, name)
            {
                problems.push(format!("{namespace}.{name} has no signature or rename"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for item in &table.items {
        match item {
            Item::Function(function) => {
                if !functions.contains(&function.name) {
                    problems.push(format!("{} is not a runtime global", function.name));
                }
            }
            Item::Module(module) => {
                if !seen.insert(module.name.clone()) {
                    problems.push(format!("{} is declared twice", module.name));
                }
                for member in &module.members {
                    let name = member.name();
                    let served = namespaces
                        .get(&module.name)
                        .is_some_and(|members| members.contains(name));
                    if !served {
                        problems.push(format!("{}.{name} is not a runtime member", module.name));
                    }
                }
            }
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn every_rename_leaves_a_runtime_spelling_for_a_canonical_one() {
    let runtime = runtime_members();
    let (functions, namespaces) = runtime_globals();
    let table = table();
    let declared = table_members();
    let mut problems = Vec::new();
    for rename in renames() {
        let receiver = rename.receiver.as_str();
        let name = rename.name.as_str();
        let served = match receiver {
            "*" => true,
            "global" => functions.contains(name),
            "type" => crate::types::builtin_name(name).is_some(),
            "T" => runtime.values().any(|names| names.contains(name)),
            _ => match runtime.get(receiver) {
                Some(names) => names.contains(name) || runtime["T"].contains(name),
                None => namespaces
                    .get(receiver)
                    .is_some_and(|members| members.contains(name)),
            },
        };
        if !served {
            problems.push(format!("{receiver}.{name} is not a runtime spelling"));
        }
        if !matches!(rename.replacement, Replacement::Rewrite(_)) {
            continue;
        }
        let Some((namespace, canonical)) = rename.canonical() else {
            continue;
        };
        let declared = match (receiver, namespace) {
            ("global", None) => table.functions(canonical).next().is_some(),
            (_, Some(namespace)) => table
                .module(namespace)
                .is_some_and(|module| module.named(canonical).next().is_some()),
            (receiver, None) => {
                let has = |base: &str| {
                    [base, "T"].iter().any(|base| {
                        declared
                            .get(*base)
                            .is_some_and(|names| names.contains_key(canonical))
                    })
                };
                if receiver == "T" {
                    let mut serving = runtime
                        .iter()
                        .filter(|(base, names)| **base != "T" && names.contains(name));
                    serving.all(|(base, _)| has(base))
                } else {
                    has(receiver)
                }
            }
        };
        if !declared {
            problems.push(format!(
                "{receiver}.{name} becomes {canonical}, which has no signature"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_engine_prelude_adds_host_declarations_after_the_builtins() {
    use crate::{Capability, HostMethod, Signature, SignatureParam};
    let param = |name: &str, ty: &str, optional| SignatureParam {
        name: name.into(),
        ty: ty.into(),
        optional,
    };
    let signed = HostMethod::new_with_block("notify", |_, _, _| Ok(Value::nil()))
        .with_signature(Signature {
            params: vec![
                param("to", "{ name: string, email?: string? }", false),
                param("", "Array<INT>", true),
            ],
            result: "object".into(),
            accepts_block: true,
        })
        .unwrap();
    let mut engine = Engine::new();
    engine.register("plain", |_, _| Ok(Value::nil()));
    engine.register_with_keywords("flexible", |_, _, _| Ok(Value::nil()));
    engine.register_method("notify", signed.clone());
    engine.register_method(
        "visit",
        HostMethod::new_with_block("visit", |_, _, _| Ok(Value::nil())),
    );
    let send = HostMethod::new("SMS.send", |_, _, _| Ok(Value::nil()))
        .with_signature(Signature {
            params: vec![param("message", "string", false)],
            result: "string".into(),
            accepts_block: false,
        })
        .unwrap();
    let options = CallOptions {
        capabilities: vec![
            Capability::from_value(
                "SMS",
                Value::object(vec![
                    (b"send".to_vec(), send.value()),
                    (b"limit".to_vec(), Value::int(3)),
                    (
                        b"tags".to_vec(),
                        Value::array(vec![Value::bytes("a"), Value::int(1)]),
                    ),
                ]),
            ),
            Capability::new("Clock", |_| Ok(Value::nil())),
            Capability::new("config", |_| Ok(Value::nil())),
        ],
        globals: [
            (
                "config".to_owned(),
                Value::hash(vec![(b"a".to_vec(), Value::int(1))]),
            ),
            ("hook".to_owned(), signed.value()),
        ]
        .into_iter()
        .collect(),
        ..CallOptions::default()
    };
    let prelude = engine.prelude(&options);
    let builtins = super::prelude();
    let host = prelude
        .strip_prefix(&builtins)
        .expect("the builtin prelude comes first");
    assert_eq!(
        host,
        "\n# A host function.\n\
         def flexible(*args: array<any>, **keywords: hash<string, any>) -> any\n\
         \n# A host function.\n\
         def notify(to: { email?: string?, name: string }, arg2?: array<int>, &block?: (*any) -> any) -> hash<string, any>\n\
         \n# A host function.\n\
         def plain(*args: array<any>) -> any\n\
         \n# A host function.\n\
         def visit(*args: array<any>, **keywords: hash<string, any>, &block?: (*any) -> any) -> any\n\
         \n# A capability bound when each call starts, so its members are not known here.\n\
         Clock: any\n\
         \n# A capability.\n\
         module SMS\n  def send(message: string) -> string\n  limit: int\n  tags: array<string | int>\nend\n\
         \n# A global.\n\
         config: any\n\
         \n# A global.\n\
         def hook(to: { email?: string?, name: string }, arg2?: array<int>, &block?: (*any) -> any) -> hash<string, any>\n"
    );
    let table = Table::parse(&prelude).unwrap();
    assert_eq!(table.to_string(), prelude);
    assert_eq!(Engine::new().prelude(&CallOptions::default()), builtins);
}
