//! Names a host declares for every call to supply: globals with their types
//! and capabilities with their members. The static checker types the names
//! by their declarations, the prelude prints them, and each call checks at
//! entry that its globals and capabilities match them.

use crate::{
    CallContext, Capability, Error, ErrorKind, Result, Value,
    signatures::{self, Item},
    types::{Type, TypeKind},
    value::Kind,
};
use std::collections::BTreeMap;

/// Every declared name, in name order.
pub(crate) type Declarations = BTreeMap<String, Declaration>;

/// One declared name.
#[derive(Clone, Debug)]
pub(crate) struct Declaration {
    /// The declaration as the prelude prints it and the checker reads it.
    pub item: Item,
    /// Whether a capability declares the name, rather than a global.
    pub capability: bool,
    /// What each call's value for the name must be.
    shape: Shape,
}

/// What a declared value must be at call entry.
#[derive(Clone, Debug)]
enum Shape {
    /// A value of this type; none accepts any value.
    Value(Option<Type>),
    /// A host method with this published signature; none accepts any method.
    Method(Option<crate::Signature>),
    /// An object with these members.
    Object(Vec<(String, Shape)>),
    /// A retained script declaration, pinned to its nominal identity and source.
    Retained(Value, Option<RetainedSource>),
}

/// The declarations a retained namespace's original code resolves.
#[derive(Clone, Debug)]
pub(crate) struct RetainedSource {
    pub declarations: BTreeMap<String, String>,
    pub aliases: BTreeMap<String, Vec<u8>>,
}

impl Declaration {
    /// A global of the annotation `ty`, or of any type when `ty` is empty.
    pub fn global(name: &str, ty: &str) -> Result<Self> {
        crate::syntax::binding_name(&(), name)?;
        if ty.trim().is_empty() {
            return Ok(Self {
                item: constant(name, signatures::Type::name("any")),
                capability: false,
                shape: Shape::Value(None),
            });
        }
        let parsed = crate::syntax::parse_type(ty).map_err(|error| {
            Error::new(
                ErrorKind::Argument,
                format!("declared type of global {name}: {}", error.message),
            )
        })?;
        if let Some(named) = named(&parsed) {
            return Err(Error::new(
                ErrorKind::Argument,
                format!(
                    "declared type of global {name} names {named}, which a host cannot supply; declare it with builtin types"
                ),
            ));
        }
        Ok(Self {
            item: constant(name, signatures::host::annotation(&parsed)),
            capability: false,
            shape: Shape::Value(Some(parsed)),
        })
    }

    /// A capability as its template describes it: a host method is a
    /// function, an object holding host methods a namespace of its methods
    /// and data, and other data a value of the type its contents show. A
    /// factory capability, whose value is known only when a call starts, is
    /// declared as `any`.
    pub fn capability(capability: &Capability) -> Result<Self> {
        let name = capability.name.as_str();
        let Some(template) = capability.template() else {
            crate::syntax::binding_name(&(), name)?;
            return Ok(Self {
                item: constant(name, signatures::Type::name("any")),
                capability: true,
                shape: Shape::Value(None),
            });
        };
        crate::capability::binding_name(&(), name, template)?;
        crate::capability::template_names(&(), template)?;
        Ok(Self {
            item: signatures::host::binding(name, template),
            capability: true,
            shape: shape(template, true)?,
        })
    }

    /// The retained script declaration, if the capability template names one.
    pub fn retained(&self) -> Option<(&Value, Option<&RetainedSource>)> {
        match &self.shape {
            Shape::Retained(value, source) => Some((value, source.as_ref())),
            _ => None,
        }
    }

    /// Checks the value a call supplies for the declared name `name`, as
    /// `subject` names it in errors, such as `global tenant`.
    fn check(&self, ctx: &mut CallContext, subject: &str, value: &Value) -> Result<()> {
        check(ctx, &self.shape, subject, value)
    }
}

fn constant(name: &str, ty: signatures::Type) -> Item {
    Item::Constant(signatures::Constant {
        doc: Vec::new(),
        name: name.to_owned(),
        ty,
    })
}

/// The first name in a type that is not a builtin type, which a host value
/// cannot have.
fn named(ty: &Type) -> Option<&str> {
    match &ty.kind {
        TypeKind::Named => Some(&ty.name),
        TypeKind::Array(Some(element)) | TypeKind::Literal(Some(element)) => named(element),
        TypeKind::Hash(Some(pair)) => named(&pair.0).or_else(|| named(&pair.1)),
        TypeKind::Shape(fields, _) => fields.iter().find_map(|field| named(&field.ty)),
        TypeKind::Union(options) | TypeKind::Tuple(options) => options.iter().find_map(named),
        _ => None,
    }
}

/// What a template requires of each call's value, matching the item
/// [`signatures::host::binding`] renders for it.
fn shape(value: &Value, top: bool) -> Result<Shape> {
    Ok(match &value.0 {
        Kind::Namespace(namespace) if top => {
            let owner = namespace
                .owner
                .clone()
                .or_else(|| {
                    namespace
                        .definition
                        .owner
                        .get()
                        .and_then(std::sync::Weak::upgrade)
                })
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Argument,
                        "retained namespace has no source owner",
                    )
                })?;
            owner
                .program
                .outline
                .iter()
                .find(|declaration| declaration.name == namespace.definition.name)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Argument,
                        "only top-level script declarations can be retained",
                    )
                })?;
            let parsed = crate::syntax::parse(owner.program.source.text(), &())?;
            let mut aliases = BTreeMap::new();
            for (scope, alias) in &parsed.additions.aliases {
                if scope.is_none() {
                    let mut text = Vec::new();
                    crate::shapes::format(&alias.ty, &mut text)?;
                    aliases.insert(alias.name.to_string(), text);
                }
            }
            let mut declarations: BTreeMap<_, _> = owner
                .program
                .outline
                .iter()
                .map(|declaration| {
                    (
                        declaration.name.clone(),
                        owner.program.source.text()[declaration.span.clone()].to_owned(),
                    )
                })
                .collect();
            let mut referenced = vec![declarations[&namespace.definition.name].as_bytes()];
            let mut used = std::collections::BTreeSet::from([namespace.definition.name.clone()]);
            // Textual references conservatively retain dependencies even in annotations.
            loop {
                let before = used.len();
                for (name, text) in declarations
                    .iter()
                    .map(|(name, text)| (name, text.as_bytes()))
                    .chain(aliases.iter().map(|(name, text)| (name, text.as_slice())))
                {
                    if !used.contains(name)
                        && referenced.iter().any(|source| {
                            source
                                .windows(name.len())
                                .any(|word| word == name.as_bytes())
                        })
                    {
                        used.insert(name.clone());
                        referenced.push(text);
                    }
                }
                if before == used.len() {
                    break;
                }
            }
            declarations.retain(|name, _| used.contains(name));
            aliases.retain(|name, _| used.contains(name));
            Shape::Retained(
                value.clone(),
                Some(RetainedSource {
                    declarations,
                    aliases,
                }),
            )
        }
        Kind::Enum(_) if top => Shape::Retained(value.clone(), None),
        Kind::Host(method) => {
            Shape::Method(method.signature().map(|signature| signature.source.clone()))
        }
        Kind::Hash(hash)
            if top
                && hash
                    .buffer
                    .data
                    .iter()
                    .any(|(_, field)| matches!(field.0, Kind::Host(_))) =>
        {
            let mut members = Vec::new();
            for (key, field) in &hash.buffer.data {
                let Some(key) = key.as_bytes() else {
                    continue;
                };
                members.push((
                    String::from_utf8_lossy(key).into_owned(),
                    shape(field, false)?,
                ));
            }
            Shape::Object(members)
        }
        _ => {
            let ty = signatures::host::value_type(value, 0).to_string();
            Shape::Value(Some(crate::syntax::parse_type(&ty)?))
        }
    })
}

fn check(ctx: &mut CallContext, shape: &Shape, subject: &str, value: &Value) -> Result<()> {
    ctx.charge(1)?;
    match shape {
        Shape::Retained(template, _) => {
            let same = match (&template.0, &value.0) {
                (Kind::Namespace(expected), Kind::Namespace(actual)) => expected.same_type(actual),
                (Kind::Enum(expected), Kind::Enum(actual)) => expected.identical(actual),
                _ => false,
            };
            if same {
                Ok(())
            } else {
                Err(Error::new(
                    ErrorKind::Type,
                    format!("{subject} must retain its declared script type identity"),
                )
                .with_class(crate::ErrorClass::Type))
            }
        }
        Shape::Value(None) => Ok(()),
        Shape::Value(Some(ty)) => {
            let prepared = crate::types::prepare(ctx, ty, |_, _| {
                Err(Error::new(ErrorKind::Type, "unknown named type"))
            })?;
            prepared.normalize_with(
                ctx,
                value.clone(),
                crate::types::Context::Subject(subject.as_bytes()),
            )?;
            Ok(())
        }
        Shape::Method(declared) => {
            let Kind::Host(method) = &value.0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    format!(
                        "{subject} must be a host method, as the host declares it, got {}",
                        value.type_name()
                    ),
                ));
            };
            let published = method.signature().map(|signature| &signature.source);
            if declared.is_some() && published != declared.as_ref() {
                return Err(Error::new(
                    ErrorKind::Type,
                    format!("{subject} does not have the signature the host declares for it"),
                ));
            }
            Ok(())
        }
        Shape::Object(members) => {
            let Kind::Hash(hash) = &value.0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    format!(
                        "{subject} must be an object with the members the host declares, got {}",
                        value.type_name()
                    ),
                ));
            };
            for (name, member) in members {
                ctx.work_bytes(name.len())?;
                let Some(index) = hash.find(ctx, name.as_bytes())? else {
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!("{subject} lacks the member {name} the host declares"),
                    ));
                };
                let field = hash.buffer.data[index].1.clone();
                check(ctx, member, &format!("{subject} member {name}"), &field)?;
            }
            Ok(())
        }
    }
}

/// Checks the declared globals a call supplies, and that every declared
/// name is supplied by a global or a capability. Capabilities are checked
/// as they are bound, by [`check_capability`].
pub(crate) fn check_globals(ctx: &mut CallContext, declared: &Declarations) -> Result<()> {
    let globals = std::mem::take(&mut ctx.options.globals);
    let capabilities = std::mem::take(&mut ctx.options.capabilities);
    let result = (|| {
        for (name, value) in &globals {
            crate::capability::binding_name(
                &crate::compilation::Meter(std::cell::RefCell::new(ctx)),
                name,
                value,
            )?;
            if !declared.contains_key(name) {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("undeclared global {name}; declare it on the engine before compiling"),
                ));
            }
        }
        for capability in &capabilities {
            ctx.work_bytes(capability.name.len())?;
            if !declared.contains_key(&capability.name) {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!(
                        "undeclared capability {}; declare it on the engine before compiling",
                        capability.name
                    ),
                ));
            }
        }
        for (name, declaration) in declared {
            ctx.work_bytes(name.len())?;
            if let Some(value) = globals.get(name) {
                declaration.check(ctx, &format!("global {name}"), value)?;
                continue;
            }
            ctx.charge(capabilities.len() as u64)?;
            let granted = capabilities
                .iter()
                .any(|capability| capability.name == *name);
            if !granted {
                let what = if declaration.capability {
                    "capability"
                } else {
                    "global"
                };
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("missing {what} {name}, which the host declares"),
                ));
            }
        }
        Ok(())
    })();
    ctx.options.globals = globals;
    ctx.options.capabilities = capabilities;
    result
}

/// Checks a bound capability against its declaration, unless no
/// declaration names it.
pub(crate) fn check_capability(
    ctx: &mut CallContext,
    declared: &Declarations,
    name: &str,
    value: &Value,
) -> Result<()> {
    ctx.work_bytes(name.len())?;
    match declared.get(name) {
        Some(declaration) => declaration.check(ctx, &format!("capability {name}"), value),
        None => Ok(()),
    }
}
