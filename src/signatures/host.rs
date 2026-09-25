//! Renders a host's functions, capabilities and globals as signature
//! declarations for [`crate::Engine::prelude`].

use super::{
    Block, Constant, Field, Function, Item, Member, Module, Param, ParamKind, Table, Type,
};
use crate::{
    CallOptions, Value,
    capability::{BoundMethod, Registered},
    types::{Scalar, TypeKind},
    value::Kind,
};
use std::collections::{BTreeMap, BTreeSet};

/// The builtin table followed by the host's declarations: functions
/// registered on the engine, then the capabilities and globals `options`
/// grants a call.
pub(crate) fn table(
    hosts: &BTreeMap<String, Registered>,
    keywordless: &BTreeSet<String>,
    options: &CallOptions,
) -> Table {
    let mut table = super::table().clone();
    for (name, host) in hosts {
        let function = match host {
            Registered::Callback(_) => unsigned(name, !keywordless.contains(name), false),
            Registered::Method(method) => match &method.value().0 {
                Kind::Host(bound) => method_function(name, bound),
                _ => unreachable!(),
            },
        };
        table
            .items
            .push(documented(Item::Function(function), "A host function."));
    }
    // A later grant replaces an earlier one of the same name, and an
    // explicit global shadows a capability.
    let mut capabilities = BTreeMap::new();
    for capability in &options.capabilities {
        capabilities.insert(capability.name.as_str(), capability);
    }
    for (name, capability) in capabilities {
        if options.globals.contains_key(name) {
            continue;
        }
        let item = match capability.template() {
            Some(value) => documented(binding(name, value), "A capability."),
            None => documented(
                Item::Constant(Constant {
                    doc: Vec::new(),
                    name: name.to_owned(),
                    ty: Type::name("any"),
                }),
                "A capability bound when each call starts, so its members are not known here.",
            ),
        };
        table.items.push(item);
    }
    for (name, value) in &options.globals {
        let item = match binding(name, value) {
            Item::Constant(constant) => Item::Constant(Constant {
                ty: Type::name("any"),
                ..constant
            }),
            item => item,
        };
        table.items.push(documented(item, "A global."));
    }
    table
}

/// A registered host function's signature, as the static checker reads it:
/// its published signature, or `any` arguments and result without one.
pub(crate) fn function(name: &str, host: &Registered) -> Function {
    match host {
        Registered::Callback(_) => unsigned(name, true, false),
        Registered::Method(method) => match &method.value().0 {
            Kind::Host(bound) => method_function(name, bound),
            _ => unreachable!(),
        },
    }
}

fn documented(item: Item, doc: &str) -> Item {
    let doc = vec![doc.to_owned()];
    match item {
        Item::Function(function) => Item::Function(Function { doc, ..function }),
        Item::Constant(constant) => Item::Constant(Constant { doc, ..constant }),
        Item::Module(module) => Item::Module(Module { doc, ..module }),
        item => item,
    }
}

/// A bound value as a declaration: a host method becomes a function, an
/// object holding host methods a namespace, and any other value a constant.
fn binding(name: &str, value: &Value) -> Item {
    match &value.0 {
        Kind::Host(bound) => Item::Function(method_function(name, bound)),
        Kind::Hash(hash)
            if hash
                .buffer
                .data
                .iter()
                .any(|(_, field)| matches!(field.0, Kind::Host(_))) =>
        {
            let members = hash
                .buffer
                .data
                .iter()
                .filter_map(|(key, field)| {
                    let key = String::from_utf8_lossy(key.as_bytes()?).into_owned();
                    Some(match &field.0 {
                        Kind::Host(bound) => Member::Function(method_function(&key, bound)),
                        _ => Member::Constant(Constant {
                            doc: Vec::new(),
                            name: key,
                            ty: value_type(field, 0),
                        }),
                    })
                })
                .collect();
            Item::Module(Module {
                doc: Vec::new(),
                name: name.to_owned(),
                members,
            })
        }
        _ => Item::Constant(Constant {
            doc: Vec::new(),
            name: name.to_owned(),
            ty: value_type(value, 0),
        }),
    }
}

/// A host method's published signature, or an unsigned one of `any`.
fn method_function(name: &str, method: &BoundMethod) -> Function {
    let Some(signature) = method.signature() else {
        return unsigned(name, true, method.supports_block());
    };
    let params = signature
        .source
        .params
        .iter()
        .zip(&signature.params)
        .enumerate()
        .map(|(index, (param, ty))| Param {
            name: if param.name.is_empty() {
                format!("arg{}", index + 1)
            } else {
                param.name.clone()
            },
            kind: ParamKind::Positional,
            ty: ty.as_ref().map_or_else(|| Type::name("any"), annotation),
            optional: param.optional,
            default: None,
        })
        .collect();
    Function {
        doc: Vec::new(),
        name: name.to_owned(),
        type_params: Vec::new(),
        params,
        block: signature.source.accepts_block.then(any_block),
        result: Some(
            signature
                .result
                .as_ref()
                .map_or_else(|| Type::name("any"), annotation),
        ),
    }
}

/// A function without a signature: any arguments, an optional block when the
/// host can run one, and an `any` result.
fn unsigned(name: &str, keywords: bool, block: bool) -> Function {
    let mut params = vec![Param {
        name: "args".into(),
        kind: ParamKind::Rest,
        ty: Type::Name("array".into(), vec![Type::name("any")]),
        optional: false,
        default: None,
    }];
    if keywords {
        params.push(Param {
            name: "keywords".into(),
            kind: ParamKind::KeywordRest,
            ty: Type::Name("hash".into(), vec![Type::name("string"), Type::name("any")]),
            optional: false,
            default: None,
        });
    }
    Function {
        doc: Vec::new(),
        name: name.to_owned(),
        type_params: Vec::new(),
        params,
        block: block.then(any_block),
        result: Some(Type::name("any")),
    }
}

/// A block the host may call with any arguments.
fn any_block() -> Block {
    Block {
        name: "block".into(),
        optional: true,
        params: Vec::new(),
        rest: Some(Type::name("any")),
        result: Some(Type::name("any")),
    }
}

/// A runtime annotation in the signature table's canonical spelling.
fn annotation(ty: &crate::types::Type) -> Type {
    let base = match &ty.kind {
        TypeKind::Scalar(scalar) => Type::name(match scalar {
            Scalar::Any => "any",
            Scalar::Int => "int",
            Scalar::Float => "float",
            Scalar::Number => "number",
            Scalar::String => "string",
            Scalar::Bool => "bool",
            Scalar::Nil => "nil",
            Scalar::Duration => "duration",
            Scalar::Time => "time",
            Scalar::Money => "money",
            Scalar::Range => "range",
            Scalar::Symbol => "symbol",
            Scalar::Regex => "regex",
            Scalar::MatchData => "match_data",
            Scalar::Error => "error",
        }),
        TypeKind::Array(element) => Type::Name(
            "array".into(),
            vec![
                element
                    .as_deref()
                    .map_or_else(|| Type::name("any"), annotation),
            ],
        ),
        TypeKind::Hash(pair) => Type::Name(
            "hash".into(),
            match pair.as_deref() {
                Some((key, value)) => vec![annotation(key), annotation(value)],
                None => vec![Type::name("string"), Type::name("any")],
            },
        ),
        TypeKind::Shape(fields, open) => Type::Shape(
            fields
                .iter()
                .map(|field| Field {
                    name: String::from_utf8_lossy(&field.name).into_owned(),
                    ty: annotation(&field.ty),
                    optional: field.optional,
                })
                .collect(),
            *open,
        ),
        TypeKind::Union(arms) => Type::Union(arms.iter().map(annotation).collect()),
        TypeKind::Tuple(elements) => Type::Tuple(elements.iter().map(annotation).collect()),
        TypeKind::Literal(described) => Type::Name(
            "type".into(),
            described.as_deref().map(annotation).into_iter().collect(),
        ),
        TypeKind::Named => Type::name(&ty.name),
    };
    if ty.nullable {
        Type::Optional(Box::new(base))
    } else {
        base
    }
}

/// The type of a host data value, as far as its contents show it.
fn value_type(value: &Value, depth: usize) -> Type {
    if depth > 16 {
        return Type::name("any");
    }
    match &value.0 {
        Kind::Nil => Type::name("nil"),
        Kind::Bool(_) => Type::name("bool"),
        Kind::Int(_) | Kind::Big(_) => Type::name("int"),
        Kind::Float(_) => Type::name("float"),
        Kind::Bytes(_) => Type::name("string"),
        Kind::Symbol(_) => Type::name("symbol"),
        Kind::Money(_) => Type::name("money"),
        Kind::Duration(_) => Type::name("duration"),
        Kind::Time(_) | Kind::Zoned(_) => Type::name("time"),
        Kind::Range(_) => Type::name("range"),
        Kind::Regex(_) => Type::name("regex"),
        Kind::Array(array) => {
            let mut arms: Vec<Type> = Vec::new();
            for item in &array.buffer.data {
                let ty = value_type(item, depth + 1);
                if !arms.contains(&ty) {
                    arms.push(ty);
                }
            }
            let element = match arms.len() {
                0 => Type::name("any"),
                1 => arms.pop().unwrap(),
                _ => Type::Union(arms),
            };
            Type::Name("array".into(), vec![element])
        }
        Kind::Hash(hash) => {
            let mut fields = Vec::new();
            for (key, field) in &hash.buffer.data {
                let Some(key) = key.as_bytes() else {
                    return Type::name("any");
                };
                fields.push(Field {
                    name: String::from_utf8_lossy(key).into_owned(),
                    ty: value_type(field, depth + 1),
                    optional: false,
                });
            }
            Type::Shape(fields, false)
        }
        _ => Type::name("any"),
    }
}
