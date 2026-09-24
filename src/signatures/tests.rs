use super::{Item, Member, ParamKind, Replacement, Table, Type, renames, table};
use crate::{CallOptions, Engine, Value, builtin::Global, members::names::candidates::*};
use std::collections::{BTreeMap, BTreeSet};

/// Every construct of the signature format, in printed form.
const SAMPLE: &str = "# A sample table.

type ordered = int | string

# Prints.
def show(*values: array<any>)

def pick(from: int = 1, to?: int, strict: bool: = false, name: string:) -> int?

module Ns
  LIMIT: int
  def run<U: ordered>(input: { id: string, \"odd key\"?: U?, ... }, &block?: (U, *any) -> U | nil) -> array<U>
end

class hash<string, V?>
  getter size?: bool
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
    let pick = table.function("pick").unwrap();
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
    let Some(Member::Function(run)) = table.module("Ns").unwrap().member("run") else {
        panic!("run is a function");
    };
    let block = run.block.as_ref().unwrap();
    assert!(block.optional);
    assert_eq!(block.params, [Type::Var("U".into())]);
    assert_eq!(block.rest, Some(Type::name("any")));
    let Some(Item::Class(class)) = table.items.get(4) else {
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
            "class array<T>\n  def f\n  def f\nend",
            3,
            "duplicate member f",
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
        ("def f(x: \"a)", 1, "unterminated string"),
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

/// Whether a spelling the runtime serves on `receiver` is renamed to `name`,
/// which then has an implementation.
fn implemented_through_rename(receiver: &str, namespace: Option<&str>, name: &str) -> bool {
    renames().iter().any(|rename| {
        (rename.receiver == receiver || rename.receiver == "T")
            && rename.canonical() == Some((namespace, name))
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
            let count = own.and_then(|own| own.get(name)).copied().unwrap_or(0)
                + universal.get(name).copied().unwrap_or(0) * usize::from(*receiver != "T");
            match count {
                1 => {}
                0 if renamed(receiver, name) => {}
                0 => problems.push(format!("{receiver}.{name} has no signature or rename")),
                _ => problems.push(format!("{receiver}.{name} has {count} signatures")),
            }
        }
    }
    for (base, names) in &declared {
        for (name, &count) in names {
            if count > 1 {
                problems.push(format!("{base}.{name} is declared {count} times"));
            }
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
            if !served && !implemented_through_rename(&base, None, name) {
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
        if table.function(name).is_none() && !renamed("global", name) {
            problems.push(format!("{name} has no signature or rename"));
        }
    }
    for (namespace, members) in &namespaces {
        let module = table.module(namespace);
        for name in members {
            if module.and_then(|module| module.member(name)).is_none() && !renamed(namespace, name)
            {
                problems.push(format!("{namespace}.{name} has no signature or rename"));
            }
        }
    }
    let mut seen = BTreeSet::new();
    for item in &table.items {
        match item {
            Item::Function(function) => {
                if !seen.insert(function.name.clone()) {
                    problems.push(format!("{} is declared twice", function.name));
                }
                if !functions.contains(&function.name)
                    && !implemented_through_rename("global", None, &function.name)
                {
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
                    let renamed = namespaces.keys().any(|namespace| {
                        implemented_through_rename(namespace, Some(&module.name), name)
                    });
                    if !served && !renamed {
                        problems.push(format!("{}.{name} is not a runtime member", module.name));
                    }
                    if matches!(member, Member::Getter(_)) {
                        problems.push(format!("{}.{name} is a getter in a namespace", module.name));
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
            ("global", None) => table.function(canonical).is_some(),
            (_, Some(namespace)) => table
                .module(namespace)
                .is_some_and(|module| module.member(canonical).is_some()),
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
