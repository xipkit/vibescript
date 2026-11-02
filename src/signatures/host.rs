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
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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
                false => value_type(value),
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

/// How many levels into a host data value [`value_type`] looks; anything
/// deeper declares as `any`.
pub(super) const INFERENCE_DEPTH: usize = 16;

/// The steps [`value_type`] may take over one value: one per array element
/// or hash field and one per byte of a field name, counted as if no array
/// or hash in it were shared.
pub(super) const INFERENCE_WORK: usize = 1 << 24;

/// How large a type [`value_type`] may infer: one per type in it, plus the
/// bytes of each field name.
pub(super) const INFERENCE_SIZE: usize = 1 << 20;

/// The type of a host data value, as far as its contents show it.
///
/// An array or hash shared within the value is described once at each
/// depth it appears and reused after, so a compact graph of shared children
/// takes time and memory in the size of the graph, not of the tree it
/// unfolds to. The bounds count the unfolded tree, though: a value whose
/// description would take more than [`INFERENCE_WORK`] steps, or come to
/// more than [`INFERENCE_SIZE`], declares as `any`, as data too deep to see
/// does, and a shared value infers exactly the type an unshared copy does.
pub(crate) fn value_type(value: &Value) -> Type {
    let mut inference = Inference::default();
    match inference.infer(value, 0) {
        Some(id) => inference.build(id),
        None => Type::name("any"),
    }
}

/// One [`value_type`] walk. Each type it infers is kept once, by id, so
/// equal types have equal ids; the walk remembers the type and the steps
/// it took for each array or hash, by address and depth. A step that
/// returns `None` has run past a bound, which ends the walk.
#[derive(Default)]
struct Inference {
    described: std::collections::HashMap<(usize, usize), (usize, usize)>,
    types: Vec<(Inferred, usize)>,
    ids: std::collections::HashMap<Inferred, usize>,
    work: usize,
}

/// A [`Type`] [`value_type`] infers, with its parts by id.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Inferred {
    Name(&'static str),
    Array(usize),
    Union(Vec<usize>),
    Shape(Vec<(String, usize)>),
}

impl Inference {
    fn charge(&mut self, steps: usize) -> Option<()> {
        self.work = self.work.saturating_add(steps);
        (self.work <= INFERENCE_WORK).then_some(())
    }

    /// The id of `ty`, which comes to `size` once built.
    fn intern(&mut self, ty: Inferred) -> Option<usize> {
        if let Some(&id) = self.ids.get(&ty) {
            return Some(id);
        }
        let size = match &ty {
            Inferred::Name(_) => 1,
            Inferred::Array(element) => 1 + self.types[*element].1,
            Inferred::Union(arms) => 1 + arms.iter().map(|&arm| self.types[arm].1).sum::<usize>(),
            Inferred::Shape(fields) => {
                1 + fields
                    .iter()
                    .map(|(name, field)| name.len() + self.types[*field].1)
                    .sum::<usize>()
            }
        };
        if size > INFERENCE_SIZE {
            return None;
        }
        let id = self.types.len();
        self.types.push((ty.clone(), size));
        self.ids.insert(ty, id);
        Some(id)
    }

    /// The id of the type of `value`, `depth` levels down.
    fn infer(&mut self, value: &Value, depth: usize) -> Option<usize> {
        if depth > INFERENCE_DEPTH {
            return self.intern(Inferred::Name("any"));
        }
        // Only an array or hash held more than once can be met again.
        let (address, shared) = match &value.0 {
            Kind::Array(array) => (
                Arc::as_ptr(array).cast::<()>(),
                Arc::strong_count(array) > 1,
            ),
            Kind::Hash(hash) => (Arc::as_ptr(hash).cast::<()>(), Arc::strong_count(hash) > 1),
            _ => return self.intern(Inferred::Name(scalar_type(value))),
        };
        let key = (address as usize, depth);
        if let Some(&(id, work)) = shared.then(|| self.described.get(&key)).flatten() {
            self.charge(work)?;
            return Some(id);
        }
        let start = self.work;
        let ty = match &value.0 {
            Kind::Array(array) => self.array(&array.buffer.data, depth)?,
            Kind::Hash(hash) => self.hash(&hash.buffer.data, depth)?,
            _ => unreachable!(),
        };
        let id = self.intern(ty)?;
        if shared {
            self.described.insert(key, (id, self.work - start));
        }
        Some(id)
    }

    /// An array of the distinct types of `items`, in the order they first
    /// appear.
    fn array(&mut self, items: &[Value], depth: usize) -> Option<Inferred> {
        let mut arms = Vec::new();
        let mut distinct = std::collections::HashSet::new();
        for item in items {
            self.charge(1)?;
            let arm = self.infer(item, depth + 1)?;
            if distinct.insert(arm) {
                arms.push(arm);
            }
        }
        let element = match arms.len() {
            0 => self.intern(Inferred::Name("any"))?,
            1 => arms[0],
            _ => self.intern(Inferred::Union(arms))?,
        };
        Some(Inferred::Array(element))
    }

    /// A closed shape of `entries`, or `any` if a key is not a string.
    fn hash(&mut self, entries: &[(Value, Value)], depth: usize) -> Option<Inferred> {
        if entries.iter().any(|(key, _)| key.as_bytes().is_none()) {
            self.charge(entries.len())?;
            return Some(Inferred::Name("any"));
        }
        let mut fields = Vec::with_capacity(entries.len());
        for (key, field) in entries {
            let key = key.as_bytes().unwrap();
            self.charge(1 + key.len())?;
            let field = self.infer(field, depth + 1)?;
            fields.push((String::from_utf8_lossy(key).into_owned(), field));
        }
        Some(Inferred::Shape(fields))
    }

    /// The [`Type`] with id `id`.
    fn build(&self, id: usize) -> Type {
        match &self.types[id].0 {
            Inferred::Name(name) => Type::name(name),
            Inferred::Array(element) => Type::Name("array".into(), vec![self.build(*element)]),
            Inferred::Union(arms) => Type::Union(arms.iter().map(|&arm| self.build(arm)).collect()),
            Inferred::Shape(fields) => Type::Shape(
                fields
                    .iter()
                    .map(|(name, field)| Field {
                        name: name.clone(),
                        ty: self.build(*field),
                        optional: false,
                    })
                    .collect(),
                false,
            ),
        }
    }
}

/// The type of a value that holds no others.
fn scalar_type(value: &Value) -> &'static str {
    match &value.0 {
        Kind::Nil => "nil",
        Kind::Bool(_) => "bool",
        Kind::Int(_) | Kind::Big(_) => "int",
        Kind::Float(_) => "float",
        Kind::Bytes(_) => "string",
        Kind::Symbol(_) => "symbol",
        Kind::Money(_) => "money",
        Kind::Duration(_) => "duration",
        Kind::Time(_) | Kind::Zoned(_) => "time",
        Kind::Range(_) => "range",
        Kind::Regex(_) => "regex",
        _ => "any",
    }
}
