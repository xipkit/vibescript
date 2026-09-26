use crate::{CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, value::Kind};
use std::sync::Arc;

/// Reads the type atom of `is_type?`, refusing keywords, then a block, then
/// any argument count but one, in the reference's wording.
pub(crate) fn type_atom<'a>(
    ctx: &mut CallContext,
    args: &'a [Value],
    keywords: bool,
    block: bool,
) -> Result<Atom<'a>> {
    ctx.checkpoint()?;
    if keywords || block {
        let shape = if keywords {
            "keyword arguments"
        } else {
            "a block"
        };
        return Err(Error::new(
            ErrorKind::Argument,
            format!("is_type? does not take {shape}"),
        ));
    }
    if args.len() != 1 {
        return Err(Error::new(
            ErrorKind::Argument,
            "is_type? expects exactly one argument",
        ));
    }
    let bytes = match &args[0].0 {
        Kind::Bytes(bytes) | Kind::Symbol(bytes) => &bytes.data,
        _ => {
            return Err(Error::new(
                ErrorKind::Type,
                "is_type? expects a symbol or string type atom",
            ));
        }
    };
    Atom::parse(ctx, bytes)
}

/// Whether `receiver.is_type?(...)` runs the type predicate, which an
/// object's callable field of that name overrides.
pub(crate) fn applicable(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
) -> Result<bool> {
    if site.scope || name != "is_type?" {
        return Ok(false);
    }
    Ok(!field_named(ctx, name, receiver)?)
}

/// Whether an object has a callable field named `name`, which a call of
/// that name reaches instead of a builtin member.
pub(crate) fn field_named(ctx: &mut CallContext, name: &str, receiver: &Value) -> Result<bool> {
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.object {
            if let Some(index) = hash.find(ctx, name.as_bytes())? {
                return Ok(super::callable(&hash.buffer.data[index].1));
            }
        }
    }
    Ok(false)
}

pub(crate) struct Atom<'a> {
    text: &'a [u8],
    pub name: &'a str,
    nullable: bool,
    pub nominal: bool,
}

impl<'a> Atom<'a> {
    /// Matches a primitive type atom without resolving a nominal declaration.
    pub(crate) fn native_match(&self, kind: super::names::Receiver) -> Option<bool> {
        use super::names::Receiver;
        if self.nominal {
            return None;
        }
        if self.nullable && kind == Receiver::Nil {
            return Some(true);
        }
        Some(match self.name {
            "nil" => kind == Receiver::Nil,
            "bool" => kind == Receiver::Bool,
            "int" => matches!(kind, Receiver::Int | Receiver::Big),
            "float" => kind == Receiver::Float,
            "number" => matches!(kind, Receiver::Int | Receiver::Big | Receiver::Float),
            "string" => kind == Receiver::Bytes,
            "symbol" => kind == Receiver::Symbol,
            "array" => kind == Receiver::Array,
            "hash" | "object" => kind == Receiver::Hash,
            "range" => kind == Receiver::Range,
            "duration" => kind == Receiver::Duration,
            "time" => matches!(kind, Receiver::Time | Receiver::Zoned),
            "money" => kind == Receiver::Money,
            _ => unreachable!(),
        })
    }

    fn parse(ctx: &mut CallContext, text: &'a [u8]) -> Result<Self> {
        if text.len() > 256 {
            return Err(Error::new(
                ErrorKind::Type,
                format!(
                    "is_type? supports type atoms only, got {} bytes",
                    text.len()
                ),
            ));
        }
        ctx.work_bytes(text.len())?;
        let invalid = || atom_error(text, false);
        let source = std::str::from_utf8(text).map_err(|_| invalid())?;
        let nullable = source.ends_with('?');
        let name = source.strip_suffix('?').unwrap_or(source);
        let (qualifier, segment) = name
            .split_once('.')
            .map_or((None, name), |(a, b)| (Some(a), b));
        if !identifier(segment) || qualifier.is_some_and(|alias| !identifier(alias)) {
            return Err(invalid());
        }
        let nominal = !matches!(
            name,
            "nil"
                | "bool"
                | "int"
                | "float"
                | "number"
                | "string"
                | "symbol"
                | "array"
                | "hash"
                | "object"
                | "range"
                | "duration"
                | "time"
                | "money"
        );
        if nominal
            && !segment
                .chars()
                .next()
                .is_some_and(crate::syntax::unicode::upper)
        {
            return Err(atom_error(text, true));
        }
        Ok(Self {
            text,
            name,
            nullable,
            nominal,
        })
    }

    pub fn matches(
        &self,
        ctx: &mut CallContext,
        receiver: &Value,
        resolved: Option<&Value>,
    ) -> Result<bool> {
        ctx.charge(1)?;
        if let Some(result) = self.native_match(super::names::Receiver::of(receiver)) {
            return Ok(result);
        }
        if let Some(resolved) = resolved {
            let name = match &resolved.0 {
                Kind::Namespace(class) => &class.definition.name,
                Kind::Enum(enumeration) => &enumeration.definition.name,
                _ => unreachable!(),
            };
            ctx.work_bytes(name.len())?;
            let last = name
                .rsplit("::")
                .next()
                .unwrap()
                .rsplit('.')
                .next()
                .unwrap();
            if self.name.rsplit('.').next().unwrap() == last {
                if self.nullable && matches!(receiver.0, Kind::Nil) {
                    return Ok(true);
                }
                return Ok(match (&receiver.0, &resolved.0) {
                    (Kind::Instance(instance), Kind::Namespace(class)) => {
                        instance.class().same_type(class)
                    }
                    (Kind::EnumMember(member), Kind::Enum(enumeration)) => {
                        Arc::ptr_eq(&member.enumeration.definition, &enumeration.definition)
                    }
                    _ => false,
                });
            }
        }
        if self.name.contains('.') {
            return Err(atom_error(self.text, true));
        }
        let name = match &receiver.0 {
            Kind::Instance(instance) => &instance.class().definition.name,
            Kind::EnumMember(member) => &member.enumeration.definition.name,
            _ => return Ok(false),
        };
        crate::json::bytes_equal(ctx, self.name.as_bytes(), name.as_bytes())
    }
}

fn identifier(text: &str) -> bool {
    use crate::syntax::unicode;
    !text.is_empty()
        && text
            .chars()
            .enumerate()
            .all(|(i, ch)| ch == '_' || unicode::letter(ch) || (i > 0 && unicode::digit(ch)))
}

fn atom_error(text: &[u8], unknown: bool) -> Error {
    let mut quoted = Vec::new();
    crate::shapes::quote(text, &mut quoted);
    let quoted = std::str::from_utf8(&quoted).unwrap();
    Error::new(
        ErrorKind::Type,
        if unknown {
            format!("unknown type atom {quoted} in is_type?")
        } else {
            format!("is_type? supports type atoms only, got {quoted}")
        },
    )
}
