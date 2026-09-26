//! Calls every builtin signature on representative receivers and arguments of
//! its declared types, and holds the runtime to the declared result and block
//! argument types.
//!
//! A declared parameter type may be narrower than what the runtime accepts,
//! but a call that satisfies the declaration must not fail with a type or
//! argument error, return a value outside the declared result type, or pass
//! its block arguments outside the declared block parameter types.

mod common;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
use vibescript::{
    CallOptions, Engine, Error, ErrorKind, Limits, Value,
    signatures::{self, Block, Field, Function, Item, Member, Param, ParamKind, Type, TypeParam},
};

const PRELUDE: &str = "enum Status\n  Draft\n  Published\nend\n\
    def failure -> error\n  begin\n    raise \"bad\"\n  rescue => error\n    error\n  end\nend\n";

type Bindings = BTreeMap<String, Type>;

/// A block literal with the parameter and result types it must be called with.
type Body = (Vec<Type>, Option<Type>, String);

fn name(name: &str) -> Type {
    Type::name(name)
}

fn generic(name: &str, args: Vec<Type>) -> Type {
    Type::Name(name.to_owned(), args)
}

fn aliases() -> BTreeMap<String, Type> {
    signatures::table()
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Alias(alias) => Some((alias.name.clone(), alias.ty.clone())),
            _ => None,
        })
        .collect()
}

fn substitute(ty: &Type, bindings: &Bindings) -> Type {
    match ty {
        Type::Var(var) => bindings
            .get(var)
            .cloned()
            .unwrap_or_else(|| panic!("unbound type variable {var}")),
        Type::Name(name, args) => Type::Name(
            name.clone(),
            args.iter().map(|arg| substitute(arg, bindings)).collect(),
        ),
        Type::Optional(inner) => Type::Optional(Box::new(substitute(inner, bindings))),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute(element, bindings))
                .collect(),
        ),
        Type::Union(arms) => {
            Type::Union(arms.iter().map(|arm| substitute(arm, bindings)).collect())
        }
        Type::Shape(fields, open) => Type::Shape(
            fields
                .iter()
                .map(|field| Field {
                    ty: substitute(&field.ty, bindings),
                    ..field.clone()
                })
                .collect(),
            *open,
        ),
        ty => ty.clone(),
    }
}

/// Whether a concrete type is assignable to a bound, for choosing type
/// arguments.
fn assignable(ty: &Type, bound: &Type) -> bool {
    match bound {
        Type::Name(alias, args) if args.is_empty() && aliases().contains_key(alias) => {
            assignable(ty, &aliases()[alias])
        }
        Type::Union(arms) => arms.iter().any(|arm| assignable(ty, arm)),
        Type::Name(bound, _) if bound == "number" => {
            matches!(ty, Type::Name(name, _) if name == "int" || name == "float" || name == "number")
        }
        bound => ty == bound,
    }
}

/// Values of a type written as source, for arguments. Receivers use
/// [`receivers`], which adds empty and edge values.
fn samples(ty: &Type) -> Vec<String> {
    let aliases = aliases();
    match ty {
        Type::Name(alias, args) if args.is_empty() && aliases.contains_key(alias) => {
            samples(&aliases[alias])
        }
        Type::Name(base, args) => match (base.as_str(), args.as_slice()) {
            ("int", _) => vec!["2".into(), "1".into()],
            ("float", _) => vec!["1.5".into()],
            ("number", _) => vec!["2".into(), "1.5".into()],
            ("string", _) => vec!["\"lo\"".into()],
            ("symbol", _) => vec![":ok".into()],
            ("bool", _) => vec!["true".into(), "false".into()],
            ("nil", _) => vec!["nil".into()],
            ("any", _) => vec!["1".into(), "\"x\"".into()],
            ("duration", _) => vec!["30.minutes".into()],
            ("time", _) => vec!["Time.at(60)".into()],
            ("money", _) => vec!["money(\"1.00 USD\")".into()],
            ("range", _) => vec!["(0..1)".into()],
            ("regex", _) => vec!["/l/".into()],
            ("array", [element]) => {
                let element = samples(element);
                let second = element.get(1).unwrap_or(&element[0]);
                vec![format!("[{}, {second}]", element[0]), "[]".into()]
            }
            // Casts give hash and tuple literals their declared type
            // wherever it would otherwise be inferred from the literal.
            ("hash", [_, value]) => {
                let value = samples(value);
                vec![
                    format!("({{ a: {} }}).as({ty})", value[0]),
                    format!("({{}}).as({ty})"),
                ]
            }
            _ => panic!("no samples for {ty}"),
        },
        Type::Optional(inner) => {
            let mut values = samples(inner);
            values.push("nil".into());
            values
        }
        Type::Union(arms) => arms.iter().map(|arm| samples(arm).remove(0)).collect(),
        Type::Shape(fields, _) => {
            let fields: Vec<String> = fields
                .iter()
                .map(|field| format!("{}: {}", field.name, samples(&field.ty)[0]))
                .collect();
            vec![format!("{{ {} }}", fields.join(", "))]
        }
        Type::Symbol(symbol) => vec![format!(":{symbol}")],
        Type::Tuple(elements) => {
            let elements: Vec<String> = elements
                .iter()
                .map(|element| samples(element).remove(0))
                .collect();
            vec![format!("([{}]).as({ty})", elements.join(", "))]
        }
        _ => panic!("no samples for {ty}"),
    }
}

/// Receivers of a concrete type, including empty collections and edge values.
fn receivers(ty: &Type) -> Vec<String> {
    let Type::Name(base, args) = ty else {
        return samples(ty);
    };
    let values: Vec<&str> = match base.as_str() {
        "int" => vec!["7", "-3", "0", "2 ** 70"],
        "float" => vec!["2.5", "-1.5", "0.0", "(1.0 / 0)", "(0.0 / 0.0)"],
        "string" => vec!["\"hello world\"", "\"\""],
        "symbol" => vec![":ok"],
        "bool" => vec!["true", "false"],
        "nil" => vec!["nil"],
        "range" => vec!["(1..4)", "(3..1)", "(1...3)"],
        "time" => vec!["Time.at(0)", "Time.utc(2024, 2, 29, 13, 4, 5)"],
        "duration" => vec!["90.minutes", "0.seconds"],
        "money" => vec!["money(\"12.50 USD\")"],
        "regex" => vec!["/l+/", "/(?<x>o)/i"],
        "match_data" => vec![
            "\"hello\".match(\"l+\").as(match_data)",
            "\"hello\".match(\"(?<x>e)(z)?\").as(match_data)",
        ],
        "error" => vec!["failure"],
        "enum_type" => vec!["Status"],
        "enum_value" => vec!["Status::Draft"],
        "array" => {
            let element = samples(&args[0]);
            let first = &element[0];
            // Rows of one length, so a matrix can be transposed.
            if matches!(&args[0], Type::Name(inner, _) if inner == "array") {
                return vec![format!("[{first}, {first}]"), "[]".into()];
            }
            let second = element.get(1).unwrap_or(first);
            return vec![format!("[{first}, {second}, {first}]"), "[]".into()];
        }
        "hash" => {
            let value = samples(&args[1]);
            let second = value.get(1).unwrap_or(&value[0]);
            return vec![format!("{{ b: {}, a: {second} }}", value[0]), "{}".into()];
        }
        _ => panic!("no receivers for {ty}"),
    };
    values.into_iter().map(str::to_owned).collect()
}

/// Arguments chosen for a parameter's meaning where any value of its type
/// would fail for reasons of content, such as a malformed layout. Each is
/// still a value of the declared type.
fn chosen(path: &str, param: &str) -> Option<Vec<&'static str>> {
    Some(match (path, param) {
        (_, "pad") => vec!["\"*\""],
        (_, "in") => vec!["\"UTC\"", "nil"],
        ("global.format", "pattern") => vec!["\"%s\""],
        ("global.format", "values") => vec!["\"a\""],
        ("global.money", "amount") => vec!["\"1.50 USD\""],
        ("global.money_cents", "currency") => vec!["\"USD\""],
        ("global.to_int", "value") => vec!["2", "2.0", "\"42\""],
        ("global.to_float", "value") => vec!["2", "1.5", "\"1.5\""],
        ("Duration.parse", "text") => vec!["\"PT1H30M\"", "\"1h30m\""],
        ("JSON.parse" | "JSON.parse_as", "text") => vec!["\"[1, 2]\""],
        ("JSON.parse_as", "schema") => vec!["array<int>"],
        ("Time.parse", "text") => vec!["\"2024-01-02\""],
        ("Time.parse", "layout") => vec!["\"2006-01-02\"", "nil"],
        ("Regex.match", "pattern") => vec!["\"l+\""],
        ("Regex.new", "pattern") => vec!["\"l+\""],
        ("Regex.union", "patterns") => vec!["\"a\"", "\"b\""],
        ("Regex.replace" | "Regex.replace_all" | "Regex.match", "text") => vec!["\"hello\""],
        ("Regex.replace" | "Regex.replace_all", "pattern") => vec!["\"l\""],
        ("time.format", "layout") => vec!["\"2006-01-02\""],
        ("time.strftime", "format") => vec!["\"%Y-%m-%d\""],
        ("time.localtime", "zone") => vec!["\"+05:30\"", "nil"],
        ("T.is_type?", "type") => vec![":int", ":string"],
        ("T.as", "type") => vec!["any"],
        ("string.template", "context") => vec!["{ a: 1 }"],
        ("string.tr" | "string.tr!", "to") => vec!["\"01\""],
        ("hash.remap_keys", "mapping") => vec!["{ a: \"z\" }"],
        ("hash.fetch" | "hash.delete" | "hash.dig", "key") => vec!["\"a\"", "\"b\""],
        _ => return None,
    })
}

/// Receivers chosen where content decides whether a call is meaningful.
fn chosen_receivers(path: &str) -> Option<Vec<&'static str>> {
    Some(match path {
        "string.to_i" => vec!["\"42\"", "\" -7 \""],
        "string.to_f" => vec!["\"1.5\"", "\"3\""],
        "string.ord" => vec!["\"hello\""],
        _ => return None,
    })
}

/// Type arguments for a class's variables: each bound's admissible choices
/// among a few element types, including one that is neither ordered nor
/// numeric.
fn instantiations(vars: &[TypeParam]) -> Vec<Bindings> {
    let choices = [
        name("int"),
        name("string"),
        generic("hash", vec![name("string"), name("int")]),
        name("float"),
        name("symbol"),
    ];
    let mut all = vec![Bindings::new()];
    for var in vars {
        let admissible: Vec<Type> = match &var.bound {
            Some(bound) => choices
                .iter()
                .filter(|choice| assignable(choice, bound))
                .cloned()
                .collect(),
            None => choices[..3].to_vec(),
        };
        all = all
            .into_iter()
            .flat_map(|bindings| {
                admissible.iter().map(move |choice| {
                    let mut bindings = bindings.clone();
                    bindings.insert(var.name.clone(), choice.clone());
                    bindings
                })
            })
            .collect();
    }
    all
}

/// Binds a function's own type parameters: an accumulator starts as an
/// element, and a type literal's argument is chosen with the literal.
fn bind_function(function: &Function, bindings: &mut Bindings) {
    for param in &function.type_params {
        let ty = if param.name == "A" {
            bindings.get("T").cloned().unwrap_or_else(|| name("int"))
        } else if function.name == "parse_as" {
            generic("array", vec![name("int")])
        } else if function.name == "as" {
            // Every receiver is cast to `any`, which it always is.
            name("any")
        } else {
            let choices = [name("int"), name("string")];
            match &param.bound {
                Some(bound) => choices
                    .into_iter()
                    .find(|choice| assignable(choice, bound))
                    .expect("an admissible type argument"),
                None => name("int"),
            }
        };
        bindings.insert(param.name.clone(), ty);
    }
}

/// Whether a runtime value belongs to a concrete declared type.
fn satisfies(value: &Value, ty: &Type) -> bool {
    let aliases = aliases();
    match ty {
        Type::Name(alias, args) if args.is_empty() && aliases.contains_key(alias) => {
            satisfies(value, &aliases[alias])
        }
        Type::Name(base, args) => match (base.as_str(), args.as_slice()) {
            ("any", _) => true,
            ("number", _) => matches!(value.type_name(), "int" | "float"),
            ("array", [element]) => value
                .as_array()
                .is_some_and(|items| items.iter().all(|item| satisfies(item, element))),
            ("hash", [_, element]) => value.as_hash().is_some_and(|entries| {
                entries
                    .iter()
                    .all(|(key, value)| key.as_bytes().is_some() && satisfies(value, element))
            }),
            ("match_data", _) => record_has(value, "captures"),
            ("error", _) => record_has(value, "message"),
            ("enum_type", _) => value.as_enum_type().is_some(),
            ("enum_value", _) => value.as_enum_member().is_some(),
            ("type", _) => value.as_type_literal().is_some(),
            (base, []) => value.type_name() == base,
            _ => false,
        },
        Type::Optional(inner) => value.type_name() == "nil" || satisfies(value, inner),
        Type::Union(arms) => arms.iter().any(|arm| satisfies(value, arm)),
        Type::Shape(fields, open) => value.as_hash().is_some_and(|entries| {
            let key = |key: &Value| String::from_utf8_lossy(key.as_bytes().unwrap()).into_owned();
            let present: BTreeMap<String, &Value> =
                entries.iter().map(|(k, v)| (key(k), v)).collect();
            fields.iter().all(|field| match present.get(&field.name) {
                Some(value) => satisfies(value, &field.ty),
                None => field.optional,
            }) && (*open
                || present
                    .keys()
                    .all(|key| fields.iter().any(|f| f.name == *key)))
        }),
        Type::Symbol(symbol) => {
            value.type_name() == "symbol" && value.as_bytes() == Some(symbol.as_bytes())
        }
        Type::Tuple(elements) => value.as_array().is_some_and(|items| {
            items.len() == elements.len()
                && items
                    .iter()
                    .zip(elements)
                    .all(|(item, ty)| satisfies(item, ty))
        }),
        _ => panic!("unexpected type {ty}"),
    }
}

fn record_has(value: &Value, field: &str) -> bool {
    value.as_hash().is_some_and(|entries| {
        entries
            .iter()
            .any(|(key, _)| key.as_bytes() == Some(field.as_bytes()))
    })
}

/// What a call is made on.
#[derive(Clone)]
enum Target {
    /// A member of a receiver expression.
    Member(String),
    /// A namespace member, such as `Math.sqrt`.
    Namespace(String),
    Global,
}

/// One call of a signature, with the types its result and block arguments
/// must have.
struct Call {
    /// The signature, such as `array.map` or `Math.sqrt`.
    path: String,
    /// The call as the language writes it: without parentheses when it
    /// passes no arguments.
    source: String,
    /// Whether the call passes no arguments, so the language writes it
    /// without parentheses.
    bare: bool,
    result: Type,
    block: Option<(Vec<Type>, Option<Type>)>,
}

fn variants(
    path: &str,
    target: &Target,
    spelling: &str,
    function: &Function,
    bindings: &Bindings,
) -> Vec<Call> {
    let result = function
        .result
        .as_ref()
        .map_or_else(|| name("nil"), |result| substitute(result, bindings));
    let values = |param: &Param| -> Vec<String> {
        if let Some(chosen) = chosen(path, &param.name) {
            return chosen.into_iter().map(str::to_owned).collect();
        }
        let ty = substitute(&param.ty, bindings);
        match param.kind {
            ParamKind::Rest => match ty {
                Type::Name(_, args) => samples(&args[0]),
                _ => unreachable!(),
            },
            _ => samples(&ty),
        }
    };
    // Every argument list: each parameter takes each of its samples, and an
    // optional one may be omitted, unless a later positional one is passed.
    let mut lists: Vec<Vec<Option<String>>> = vec![Vec::new()];
    for param in &function.params {
        let candidates = values(param);
        let mut choices: Vec<Option<String>> = match param.kind {
            ParamKind::Rest => vec![
                None,
                Some(candidates[0].clone()),
                Some(candidates.join(", ")),
            ],
            _ => candidates.into_iter().map(Some).collect(),
        };
        if param.optional {
            choices.push(None);
        }
        lists = lists
            .into_iter()
            .flat_map(|list| {
                choices.iter().map(move |choice| {
                    let mut list = list.clone();
                    list.push(choice.clone());
                    list
                })
            })
            .collect();
    }
    lists.retain(|list| {
        let positional = function
            .params
            .iter()
            .zip(list)
            .filter(|(param, _)| param.kind == ParamKind::Positional);
        let supplied: Vec<bool> = positional.map(|(_, value)| value.is_some()).collect();
        supplied.windows(2).all(|pair| pair[0] || !pair[1])
    });
    let blocks: Vec<Option<Body>> = match &function.block {
        None => vec![None],
        Some(block) => {
            let mut blocks: Vec<_> = block_bodies(path, block, bindings)
                .into_iter()
                .map(Some)
                .collect();
            if block.optional {
                blocks.push(None);
            }
            blocks
        }
    };
    let mut calls = Vec::new();
    let mut seen = BTreeSet::new();
    for list in &lists {
        let mut arguments = Vec::new();
        for (param, value) in function.params.iter().zip(list) {
            let Some(value) = value else { continue };
            match param.kind {
                ParamKind::Keyword => arguments.push(format!("{}: {value}", param.name)),
                _ => arguments.push(value.clone()),
            }
        }
        for block in &blocks {
            let prefix = match target {
                Target::Member(receiver) => format!("({receiver}).{spelling}"),
                Target::Namespace(namespace) => format!("{namespace}.{spelling}"),
                Target::Global => spelling.to_owned(),
            };
            let bare = arguments.is_empty();
            let mut source = if bare {
                prefix
            } else {
                format!("{prefix}({})", arguments.join(", "))
            };
            if let Some((_, _, body)) = block {
                source.push_str(&format!(" {body}"));
            }
            if seen.insert(source.clone()) {
                calls.push(Call {
                    path: path.to_owned(),
                    source,
                    bare,
                    result: result.clone(),
                    block: block
                        .as_ref()
                        .map(|(params, result, _)| (params.clone(), result.clone())),
                });
            }
        }
    }
    calls
}

/// Block literals for a block parameter: each records its arguments and
/// returns a value of the declared result type.
fn block_bodies(path: &str, block: &Block, bindings: &Bindings) -> Vec<Body> {
    let params: Vec<Type> = block
        .params
        .iter()
        .map(|ty| substitute(ty, bindings))
        .collect();
    let result = block.result.as_ref().map(|ty| substitute(ty, bindings));
    let names: Vec<String> = (0..params.len()).map(|index| format!("a{index}")).collect();
    let bars = if names.is_empty() {
        String::new()
    } else {
        format!("|{}| ", names.join(", "))
    };
    let record = format!("record({})", names.join(", "));
    let values = match (&result, path) {
        // Without a break, loop never ends.
        (_, "global.loop") => vec!["break 1".to_owned()],
        (None, _) => vec!["nil".to_owned()],
        (Some(result), _) => samples(result),
    };
    values
        .into_iter()
        .map(|value| {
            (
                params.clone(),
                result.clone(),
                format!("{{ {bars}{record}; {value} }}"),
            )
        })
        .collect()
}

struct Harness {
    engine: Engine,
    records: Arc<Mutex<Vec<Vec<Value>>>>,
}

impl Harness {
    fn new() -> Self {
        let records: Arc<Mutex<Vec<Vec<Value>>>> = Arc::default();
        let mut engine = Engine::new();
        let sink = records.clone();
        engine.register("record", move |_, args| {
            sink.lock().unwrap().push(args.to_vec());
            Ok(Value::nil())
        });
        engine.set_output_writer(|_, _| Ok(()));
        engine.set_error_writer(|_, _| Ok(()));
        Self { engine, records }
    }

    fn run(&self, call: &str) -> Result<Outcome, String> {
        let source = format!("{PRELUDE}def run -> any\n  {call}\nend\n");
        let script = self
            .engine
            .compile(&source)
            .map_err(|error| format!("compile error: {}", error.message))?;
        self.records.lock().unwrap().clear();
        let options = CallOptions {
            limits: Limits {
                steps: Some(200_000),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let result = script
            .call("run", &[], options)
            .map(|outcome| outcome.value);
        let records = std::mem::take(&mut *self.records.lock().unwrap());
        Ok(Outcome { result, records })
    }
}

struct Outcome {
    result: Result<Value, Error>,
    records: Vec<Vec<Value>>,
}

/// Failures a declared-valid call may still have, by signature and message.
/// Each is either decided by a value's content rather than its type, or a
/// constraint one signature per name cannot state; the latter are the table's
/// known awkward members.
const EXPECTED: &[(&str, &str)] = &[
    // A missing element or key raises (ADR-007's fetch).
    ("array.fetch", "outside of array bounds"),
    ("hash.fetch", "key not found"),
    ("hash.fetch_values", "key not found"),
    // Content: the pattern's operands, the bounds' order, an index past the end.
    ("global.format", "references missing operand"),
    ("int.clamp", "min must be <= max"),
    ("float.clamp", "min must be <= max"),
    ("float.clamp", "must not be NaN"),
    ("string.insert", "out of string"),
    // `fill` and `insert` past the end raise instead of padding with nil.
    ("array.fill", "past the end of the array"),
    ("array.fill", "out of range"),
    ("array.insert", "out of range"),
    ("match_data.begin", "capture index out of bounds"),
    ("match_data.end", "capture index out of bounds"),
    // At least one part is required.
    ("Duration.build", "expects seconds or named parts"),
    // A length only follows an integer start; a range or substring selects alone.
    ("array.fill", "does not accept a length with a range"),
    ("string.slice", "index must be integer"),
    ("string.byteslice", "start must be an integer"),
    // An exclusive range has no last element to clamp to.
    ("int.clamp", "exclusive range"),
    ("float.clamp", "exclusive range"),
    // The path must follow the value's structure.
    ("array.dig", "hash keys must be strings or symbols"),
    ("hash.dig", "hash keys must be strings or symbols"),
];

fn expected(path: &str, error: &Error) -> bool {
    EXPECTED
        .iter()
        .any(|(signature, fragment)| *signature == path && error.message.contains(fragment))
}

fn check(harness: &Harness, call: &Call, problems: &mut Vec<String>) {
    let outcome = match harness.run(&call.source) {
        Ok(outcome) => outcome,
        Err(error) => {
            problems.push(format!("{}\n    {error}", call.source));
            return;
        }
    };
    match &outcome.result {
        Ok(value) => {
            if !satisfies(value, &call.result) {
                problems.push(format!(
                    "{}\n    returned {} ({}), declared {}",
                    call.source,
                    describe(value),
                    value.type_name(),
                    call.result
                ));
            }
        }
        Err(error) => {
            let shape = matches!(error.kind, ErrorKind::Type | ErrorKind::Argument);
            if shape && !expected(&call.path, error) {
                problems.push(format!(
                    "{}\n    {:?}: {}",
                    call.source, error.kind, error.message
                ));
            }
        }
    }
    if let Some((params, _)) = &call.block {
        for args in &outcome.records {
            let fits = args.len() == params.len()
                && args
                    .iter()
                    .zip(params)
                    .all(|(value, ty)| satisfies(value, ty));
            if !fits {
                let args: Vec<String> = args.iter().map(describe).collect();
                let params: Vec<String> = params.iter().map(Type::to_string).collect();
                problems.push(format!(
                    "{}\n    yielded ({}), declared ({})",
                    call.source,
                    args.join(", "),
                    params.join(", ")
                ));
                break;
            }
        }
    }
}

fn describe(value: &Value) -> String {
    if let Some(items) = value.as_array() {
        let items: Vec<String> = items.iter().map(describe).collect();
        return format!("[{}]", items.join(", "));
    }
    if let Some(bytes) = value.as_bytes() {
        return format!("{:?}", String::from_utf8_lossy(bytes));
    }
    if let Some(number) = value.as_int() {
        return number.to_string();
    }
    value.type_name().to_owned()
}

/// Every call of every signature in the table.
fn calls() -> Vec<Call> {
    let table = signatures::table();
    let mut calls = Vec::new();
    for item in &table.items {
        match item {
            Item::Function(function) => {
                // Loading a module needs a module configuration.
                if function.name == "require" {
                    continue;
                }
                let mut bindings = Bindings::new();
                bind_function(function, &mut bindings);
                let path = format!("global.{}", function.name);
                calls.extend(variants(
                    &path,
                    &Target::Global,
                    &function.name,
                    function,
                    &bindings,
                ));
            }
            Item::Module(module) => {
                for member in &module.members {
                    let (namespace, name) = (&module.name, member.name());
                    let path = format!("{namespace}.{name}");
                    match member {
                        Member::Function(function) => {
                            let mut bindings = Bindings::new();
                            bind_function(function, &mut bindings);
                            calls.extend(variants(
                                &path,
                                &Target::Namespace(namespace.clone()),
                                name,
                                function,
                                &bindings,
                            ));
                        }
                        Member::Constant(constant) => calls.push(Call {
                            path: path.clone(),
                            source: format!("{namespace}::{name}"),
                            bare: true,
                            result: constant.ty.clone(),
                            block: None,
                        }),
                        _ => unreachable!(),
                    }
                }
            }
            Item::Class(class) => {
                let base = class.base();
                for bindings in instantiations(&class.vars) {
                    let receiver_types = if base == "T" {
                        universal_receivers()
                    } else {
                        vec![substitute(&class.receiver, &bindings)]
                    };
                    for receiver_type in receiver_types {
                        let mut bindings = bindings.clone();
                        if base == "T" {
                            bindings.insert("T".into(), receiver_type.clone());
                        }
                        for member in &class.members {
                            let path = format!("{base}.{}", member.name());
                            let receivers: Vec<String> = match chosen_receivers(&path) {
                                Some(chosen) => chosen.into_iter().map(str::to_owned).collect(),
                                None => receivers(&receiver_type),
                            };
                            for receiver in receivers {
                                match member {
                                    Member::Function(function) => {
                                        let mut bindings = bindings.clone();
                                        bind_function(function, &mut bindings);
                                        calls.extend(variants(
                                            &path,
                                            &Target::Member(receiver),
                                            member.name(),
                                            function,
                                            &bindings,
                                        ));
                                    }
                                    _ => unreachable!(),
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    calls
}

/// One type of each kind, for the members every type has.
fn universal_receivers() -> Vec<Type> {
    let mut types: Vec<Type> = [
        "int",
        "float",
        "string",
        "symbol",
        "bool",
        "nil",
        "range",
        "time",
        "duration",
        "money",
        "regex",
        "match_data",
        "error",
        "enum_type",
        "enum_value",
    ]
    .into_iter()
    .map(name)
    .collect();
    types.push(generic("array", vec![name("int")]));
    types.push(generic("hash", vec![name("string"), name("int")]));
    types
}

#[test]
fn builtin_signatures_agree_with_the_runtime() {
    let calls = calls();
    let workers = std::thread::available_parallelism().map_or(4, usize::from);
    let chunk = calls.len().div_ceil(workers);
    let problems: Vec<String> = common::scope(|scope| {
        let handles: Vec<_> = calls
            .chunks(chunk)
            .map(|calls| {
                scope.spawn(move || {
                    let harness = Harness::new();
                    let mut problems = Vec::new();
                    for call in calls {
                        check(&harness, call, &mut problems);
                    }
                    problems
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    assert!(
        problems.is_empty(),
        "{} of {} calls disagree\n{}",
        problems.len(),
        calls.len(),
        problems.join("\n")
    );
}

/// Calls without arguments are written without parentheses; the agreement
/// test runs them so, and this one names every member the runtime still
/// refuses that way.
#[test]
fn zero_argument_calls_work_without_parentheses() {
    let harness = Harness::new();
    let mut refused = BTreeSet::new();
    for call in calls().iter().filter(|call| call.bare) {
        let outcome = harness.run(&call.source).unwrap();
        if let Err(error) = outcome.result {
            let shape = matches!(error.kind, ErrorKind::Type | ErrorKind::Argument);
            if shape && !expected(&call.path, &error) {
                refused.insert(format!("{}: {}", call.source, error.message));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "refused without parentheses: {refused:#?}"
    );
}
