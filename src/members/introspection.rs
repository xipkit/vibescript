use crate::{CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, value::Kind};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Predicate {
    Respond,
    IsA,
    KindOf,
    InstanceOf,
    IsType,
}

impl Predicate {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "respond_to?" => Self::Respond,
            "is_a?" => Self::IsA,
            "kind_of?" => Self::KindOf,
            "instance_of?" => Self::InstanceOf,
            "is_type?" => Self::IsType,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Respond => "respond_to?",
            Self::IsA => "is_a?",
            Self::KindOf => "kind_of?",
            Self::InstanceOf => "instance_of?",
            Self::IsType => "is_type?",
        }
    }

    pub fn validate<'a>(
        self,
        ctx: &mut CallContext,
        args: &'a [Value],
        keywords: bool,
        block: bool,
    ) -> Result<Query<'a>> {
        ctx.checkpoint()?;
        let name = self.name();
        if keywords || block {
            let shape = if keywords {
                "keyword arguments"
            } else {
                "a block"
            };
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} does not take {shape}"),
            ));
        }
        if args.len() != 1 && !(self == Self::Respond && args.len() == 2) {
            let count = if self == Self::Respond {
                "1 or 2 arguments"
            } else {
                "exactly one argument"
            };
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} expects {count}"),
            ));
        }
        if self == Self::Respond || self == Self::IsType {
            let bytes = match &args[0].0 {
                Kind::Bytes(bytes) | Kind::Symbol(bytes) => &bytes.data,
                _ => {
                    let kind = if self == Self::Respond {
                        "method name"
                    } else {
                        "type atom"
                    };
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!("{name} expects a symbol or string {kind}"),
                    ));
                }
            };
            if self == Self::IsType {
                return Atom::parse(ctx, bytes).map(Query::Type);
            }
            let private = match args.get(1) {
                None => false,
                Some(Value(Kind::Bool(value))) => *value,
                _ => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "respond_to? expects a boolean second argument",
                    ));
                }
            };
            return Ok(Query::Respond(bytes, private));
        }
        let Kind::Namespace(class) = &args[0].0 else {
            return Err(Error::new(
                ErrorKind::Type,
                format!("{name} expects a class argument"),
            ));
        };
        Ok(Query::Class(class))
    }
}

pub(crate) enum Query<'a> {
    Respond(&'a [u8], bool),
    Class(&'a Arc<crate::namespace::Namespace>),
    Type(Atom<'a>),
}

pub(crate) fn supported(name: &str) -> bool {
    Predicate::parse(name).is_some()
}

pub(crate) fn applicable(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
) -> Result<bool> {
    if site.scope || !supported(name) {
        return Ok(false);
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.object {
            if let Some(index) = hash.find(ctx, name.as_bytes())? {
                if super::lifecycle::callable(&hash.buffer.data[index].1) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

pub(super) fn call(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if !applicable(ctx, site, name, receiver)? {
        return Ok(None);
    }
    let query = Predicate::parse(name)
        .unwrap()
        .validate(ctx, args, keywords, block)?;
    let result = match query {
        Query::Respond(name, _) => responds(ctx, receiver, name)?,
        Query::Class(class) => belongs(receiver, class),
        // Calls with an argument run through the VM, which supplies the lexical
        // type environment. Bare reads fail validation before reaching here.
        Query::Type(_) => {
            return Err(Error::new(
                ErrorKind::Type,
                "type predicate requires an execution context",
            ));
        }
    };
    Ok(Some(Value::boolean(result)))
}

pub(crate) fn belongs(receiver: &Value, class: &Arc<crate::namespace::Namespace>) -> bool {
    matches!(&receiver.0, Kind::Instance(instance) if Arc::ptr_eq(&instance.class().definition, &class.definition))
}

pub(crate) fn callable(value: &Value) -> bool {
    super::lifecycle::callable(value)
}

pub(crate) fn responds(ctx: &mut CallContext, receiver: &Value, name: &[u8]) -> Result<bool> {
    let text = method_name(ctx, name)?;
    let universal = text.is_some_and(super::names::universal);
    if universal && !matches!(text, Some("tap" | "yield_self")) {
        return Ok(true);
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if !universal
            && !hash.object
            && text.is_some_and(|name| super::names::typed(receiver, name).is_some())
        {
            return Ok(true);
        }
        if let Some(index) = hash.find(ctx, name)? {
            return Ok(callable(&hash.buffer.data[index].1));
        }
    }
    Ok(universal || text.is_some_and(|name| super::names::available(receiver, name)))
}

pub(crate) struct Atom<'a> {
    text: &'a [u8],
    pub name: &'a str,
    nullable: bool,
    pub nominal: bool,
}

impl<'a> Atom<'a> {
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
        if !self.nominal {
            if self.nullable && matches!(receiver.0, Kind::Nil) {
                return Ok(true);
            }
            return Ok(match self.name {
                "nil" => matches!(receiver.0, Kind::Nil),
                "bool" => matches!(receiver.0, Kind::Bool(_)),
                "int" => matches!(receiver.0, Kind::Int(_) | Kind::Big(_)),
                "float" => matches!(receiver.0, Kind::Float(_)),
                "number" => matches!(receiver.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)),
                "string" => matches!(receiver.0, Kind::Bytes(_)),
                "symbol" => matches!(receiver.0, Kind::Symbol(_)),
                "array" => matches!(receiver.0, Kind::Array(_)),
                "hash" | "object" => matches!(receiver.0, Kind::Hash(_)),
                "range" => matches!(receiver.0, Kind::Range(_)),
                "duration" => matches!(receiver.0, Kind::Duration(_)),
                "time" => matches!(receiver.0, Kind::Time(_) | Kind::Zoned(_)),
                "money" => matches!(receiver.0, Kind::Money(_)),
                _ => unreachable!(),
            });
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
                        Arc::ptr_eq(&instance.class().definition, &class.definition)
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

pub(crate) fn method_name<'a>(ctx: &mut CallContext, bytes: &'a [u8]) -> Result<Option<&'a str>> {
    let mut start = 0;
    while start < bytes.len() {
        let end = start + (bytes.len() - start).min(crate::budget::CHUNK);
        ctx.work_bytes(end - start)?;
        match std::str::from_utf8(&bytes[start..end]) {
            Ok(_) => start = end,
            Err(error) if error.error_len().is_none() && end < bytes.len() => {
                start += error.valid_up_to();
            }
            Err(_) => return Ok(None),
        }
    }
    // SAFETY: every byte was validated above. An incomplete boundary sequence
    // is revisited with subsequent bytes; an incomplete final sequence returns None.
    Ok(Some(unsafe { std::str::from_utf8_unchecked(bytes) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_names_match_utf8_validation_at_rune_boundaries() {
        for length in [0, 1, 4093, 4094, 4095, 4096, 8191] {
            for suffix in ["é", "€", "𐐀"] {
                for count in 0..=suffix.len() {
                    let mut bytes = vec![b'a'; length];
                    bytes.extend_from_slice(&suffix.as_bytes()[..count]);
                    let mut ctx = CallContext::new(crate::CallOptions::default());
                    assert_eq!(
                        method_name(&mut ctx, &bytes).unwrap(),
                        std::str::from_utf8(&bytes).ok()
                    );
                    bytes.extend_from_slice(&[b'b'; 4096]);
                    assert_eq!(
                        method_name(&mut ctx, &bytes).unwrap(),
                        std::str::from_utf8(&bytes).ok()
                    );
                    bytes.push(0xff);
                    assert_eq!(method_name(&mut ctx, &bytes).unwrap(), None);
                }
            }
        }
    }
}
