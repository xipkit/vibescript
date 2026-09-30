//! Renders a host's functions, capabilities and globals as signature
//! declarations for [`crate::Engine::prelude`].

use super::{
    Block, Constant, Field, Function, Item, Member, Module, Param, ParamKind, Table, Type,
};
use crate::{
    CallOptions, Value,
    capability::{BoundMethod, Registered},
    syntax::HostName,
    types::{Scalar, TypeKind},
    value::Kind,
};
use std::collections::{BTreeMap, BTreeSet};

/// The builtin table followed by the host's declarations: functions
/// registered on the engine, the globals and capabilities it declares, then
/// the capabilities and globals `options` grants a call that no declaration
/// names.
pub(crate) fn table(
    hosts: &BTreeMap<String, Registered>,
    keywordless: &BTreeSet<String>,
    declared: &crate::declared::Declarations,
    options: &CallOptions,
) -> Table {
    let mut table = super::table().clone();
    for (name, host) in hosts {
        if crate::syntax::host_function_name(&(), HostName::FUNCTION, name).is_err() {
            continue;
        }
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
    for declaration in declared.values() {
        let doc = if declaration.capability {
            "A capability the host declares."
        } else {
            "A global the host declares."
        };
        table.items.push(documented(declaration.item.clone(), doc));
    }
    // A later grant replaces an earlier one of the same name, and an
    // explicit global shadows a capability.
    let mut capabilities = BTreeMap::new();
    for capability in &options.capabilities {
        capabilities.insert(capability.name.as_str(), capability);
    }
    for (name, capability) in capabilities {
        if options.globals.contains_key(name) || declared.contains_key(name) {
            continue;
        }
        let item = match capability.template() {
            Some(value) if valid_binding(HostName::CAPABILITY, name, value) => {
                documented(binding(name, value), "A capability.")
            }
            None => {
                let Ok(item) = factory(name) else { continue };
                documented(
                    item,
                    "A capability bound when each call starts, so its members are not known here.",
                )
            }
            _ => continue,
        };
        table.items.push(item);
    }
    for (name, value) in &options.globals {
        if declared.contains_key(name) || !valid_binding(HostName::GLOBAL, name, value) {
            continue;
        }
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

fn valid_binding(host: HostName<'_>, name: &str, value: &Value) -> bool {
    crate::capability::binding_name(&(), host, name, value).is_ok()
        && crate::capability::template_names(&(), host.member_of(name), value).is_ok()
}

/// An opaque factory's declaration, deferring value-sensitive checks to binding.
pub(crate) fn factory(name: &str) -> crate::Result<Item> {
    if name.ends_with(['?', '!']) {
        crate::syntax::host_function_name(&(), HostName::CAPABILITY, name)?;
        Ok(Item::Function(unsigned(name, true, true)))
    } else {
        crate::syntax::binding_name(&(), HostName::CAPABILITY, name)?;
        Ok(Item::Constant(Constant {
            doc: Vec::new(),
            name: name.to_owned(),
            ty: Type::name("any"),
        }))
    }
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
pub(crate) fn binding(name: &str, value: &Value) -> Item {
    binding_in(name, value, &Methods::of(value), 0)
}

/// How deep objects of host methods nest as namespaces; one nested deeper
/// declares as `any`, as data deeper than [`value_type`] looks does, so the
/// declaration, the shape a grant must match and every later walk of them
/// stay shallow however deep a template nests.
pub(crate) const NAMESPACE_DEPTH: usize = 64;

/// [`binding`] of a value `depth` objects deep in its template.
fn binding_in(name: &str, value: &Value, methods: &Methods, depth: usize) -> Item {
    match &value.0 {
        Kind::Host(bound) => Item::Function(method_function(name, bound)),
        Kind::Hash(hash) if methods.holds(value) && depth < NAMESPACE_DEPTH => {
            let members = hash
                .buffer
                .data
                .iter()
                .filter_map(|(key, field)| {
                    let key = member_key(key.as_bytes()?)?;
                    Some(match binding_in(key, field, methods, depth + 1) {
                        Item::Function(function) => Member::Function(function),
                        Item::Module(module) => Member::Module(module),
                        Item::Constant(constant) => Member::Constant(constant),
                        _ => unreachable!(),
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
            ty: match methods.holds(value) {
                true => Type::name("any"),
                false => value_type(value, 0),
            },
        }),
    }
}

/// The member an object's key declares: none for a key no script can read
/// as a member, such as one not UTF-8 or the data key `"with space"`.
pub(crate) fn member_key(key: &[u8]) -> Option<&str> {
    let key = std::str::from_utf8(key).ok()?;
    let stem = key.strip_suffix(['?', '!']).unwrap_or(key);
    crate::syntax::identifier(stem).then_some(key)
}

/// The objects in a template that hold a host method at any depth, found
/// in one walk that visits each object once, without recursion.
pub(crate) struct Methods(std::collections::HashSet<usize>);

impl Methods {
    pub(crate) fn of(value: &Value) -> Self {
        let address = |hash: &std::sync::Arc<_>| std::sync::Arc::as_ptr(hash) as usize;
        let mut holding = std::collections::HashSet::new();
        let mut seen = std::collections::HashSet::new();
        // Each object is decided after its fields, which it pushes above it.
        let mut pending = vec![(value, false)];
        while let Some((value, fields_done)) = pending.pop() {
            let Kind::Hash(hash) = &value.0 else {
                continue;
            };
            if fields_done {
                let holds = hash.buffer.data.iter().any(|(_, field)| match &field.0 {
                    Kind::Host(_) => true,
                    Kind::Hash(inner) => holding.contains(&address(inner)),
                    _ => false,
                });
                if holds {
                    holding.insert(address(hash));
                }
            } else if seen.insert(address(hash)) {
                pending.push((value, true));
                pending.extend(hash.buffer.data.iter().map(|(_, field)| (field, false)));
            }
        }
        Self(holding)
    }

    /// Whether `value` is a host method or an object holding one.
    pub(crate) fn holds(&self, value: &Value) -> bool {
        match &value.0 {
            Kind::Host(_) => true,
            Kind::Hash(hash) => self.0.contains(&(std::sync::Arc::as_ptr(hash) as usize)),
            _ => false,
        }
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
pub(crate) fn annotation(ty: &crate::types::Type) -> Type {
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
            Scalar::EnumValue => "enum_value",
            Scalar::EnumType => "enum_type",
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
pub(crate) fn value_type(value: &Value, depth: usize) -> Type {
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
