use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    types::{Scalar, Type, TypeKind},
    value::Kind,
};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct Definition {
    pub ty: Type,
    pub text: Vec<u8>,
    bytes: usize,
}

#[derive(Debug)]
pub(crate) struct Shape {
    pub definition: Arc<Definition>,
    header: Option<Charge>,
    _metadata: Option<Charge>,
}

pub(crate) fn compile(ty: Type) -> Value {
    let mut text = Vec::new();
    format(&ty, &mut text).unwrap();
    let bytes = size_of::<Definition>() + 2 * size_of::<usize>() + text.capacity() + retained(&ty);
    Value(Kind::Shape(Arc::new(Shape {
        definition: Arc::new(Definition { ty, text, bytes }),
        header: None,
        _metadata: None,
    })))
}

impl Shape {
    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) || ctx.options.limits.memory_bytes.is_none() {
            return Ok(value.clone());
        }
        let metadata = ctx.reserve(value.definition.bytes)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            definition: value.definition.clone(),
            header,
            _metadata: metadata,
        }))
    }
}

pub(crate) fn append(ctx: &mut CallContext, shape: &Shape, output: &mut Buffer<u8>) -> Result<()> {
    let Some(capacity) = output
        .data
        .len()
        .checked_add(shape.definition.text.len())
        .and_then(|length| length.checked_add(8))
    else {
        return ctx.fail(ErrorKind::Memory, "type literal output size overflow");
    };
    output.ensure(ctx, capacity)?;
    output.extend(ctx, b"<Shape ")?;
    output.extend(ctx, &shape.definition.text)?;
    output.push(ctx, b'>')
}

pub(crate) fn member(
    ctx: &mut CallContext,
    name: &str,
    value: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if !matches!(value.0, Kind::Shape(_)) {
        return Ok(None);
    }
    if !matches!(name, "nil?" | "itself" | "dup") {
        return Err(Error::new(
            ErrorKind::Name,
            "unsupported member access on shape",
        ));
    }
    crate::members::universal_shape(name, value, args.len(), keywords, block)?;
    ctx.charge(1)?;
    Ok(Some(if name == "nil?" {
        Value::boolean(false)
    } else {
        value.clone()
    }))
}

pub(crate) fn retained(ty: &Type) -> usize {
    ty.name.capacity()
        + match &ty.kind {
            TypeKind::Array(Some(element)) => size_of::<Type>() + retained(element),
            TypeKind::Hash(Some(pair)) => {
                size_of::<(Type, Type)>() + retained(&pair.0) + retained(&pair.1)
            }
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.capacity() * size_of::<Type>() + options.iter().map(retained).sum::<usize>()
            }
            TypeKind::Literal(Some(described)) => size_of::<Type>() + retained(described),
            TypeKind::Shape(fields, _) => {
                fields.capacity() * size_of::<crate::types::Field>()
                    + fields
                        .iter()
                        .map(|field| field.name.capacity() + retained(&field.ty))
                        .sum::<usize>()
            }
            _ => 0,
        }
}

pub(crate) trait TypeWriter {
    fn write(&mut self, bytes: &[u8]) -> Result<()>;

    fn node(&mut self) -> Result<()> {
        Ok(())
    }

    fn byte(&mut self, byte: u8) -> Result<()> {
        self.write(&[byte])
    }
}

impl TypeWriter for Vec<u8> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

pub(crate) fn format(
    ty: &impl crate::types::description::Description,
    out: &mut impl TypeWriter,
) -> Result<()> {
    use crate::types::description::{FieldDescription, View};
    out.node()?;
    match ty.view() {
        View::Scalar(scalar) => out.write(match scalar {
            Scalar::Any => b"any",
            Scalar::Int => b"int",
            Scalar::Float => b"float",
            Scalar::Number => b"number",
            Scalar::String => b"string",
            Scalar::Symbol => b"symbol",
            Scalar::Bool => b"bool",
            Scalar::Nil => b"nil",
            Scalar::Duration => b"duration",
            Scalar::Money => b"money",
            Scalar::Time => b"time",
            Scalar::Range => b"range",
            Scalar::Regex => b"regex",
            Scalar::MatchData => b"match_data",
            Scalar::Error => b"error",
        })?,
        View::Named => out.write(ty.name().as_bytes())?,
        View::Array(element) => {
            out.write(b"array")?;
            if let Some(element) = element {
                out.byte(b'<')?;
                format(element, out)?;
                out.byte(b'>')?;
            }
        }
        View::Hash(pair) => {
            let object = ty
                .name()
                .chars()
                .map(|c| crate::casing::map(c, false))
                .eq("object".chars());
            out.write(if object { b"object" } else { b"hash" })?;
            if let Some(pair) = pair {
                out.byte(b'<')?;
                format(&pair.0, out)?;
                out.write(b", ")?;
                format(&pair.1, out)?;
                out.byte(b'>')?;
            }
        }
        View::Tuple(elements) => {
            out.byte(b'[')?;
            for (index, element) in elements.iter().enumerate() {
                if index > 0 {
                    out.write(b", ")?;
                }
                format(element, out)?;
            }
            out.byte(b']')?;
        }
        View::Literal(described) => {
            out.write(b"type")?;
            if let Some(described) = described {
                out.byte(b'<')?;
                format(described, out)?;
                out.byte(b'>')?;
            }
        }
        View::Union(options) => {
            for (index, option) in options.iter().enumerate() {
                if index > 0 {
                    out.write(b" | ")?;
                }
                format(option, out)?;
            }
            return Ok(());
        }
        View::Shape(fields, open) => {
            if fields.is_empty() && !open {
                out.write(b"{}")?;
            } else {
                out.write(b"{ ")?;
                for (index, field) in fields.iter().enumerate() {
                    out.node()?;
                    if index > 0 {
                        out.write(b", ")?;
                    }
                    if field.name().ends_with(b"?") {
                        quoted(field.name(), out)?;
                    } else {
                        out.write(field.name())?;
                    }
                    if field.optional() {
                        out.byte(b'?')?;
                    }
                    out.write(b": ")?;
                    format(field.ty(), out)?;
                }
                if open {
                    if !fields.is_empty() {
                        out.write(b", ")?;
                    }
                    out.write(b"...")?;
                }
                out.write(b" }")?;
            }
        }
    }
    if ty.nullable() && !matches!(ty.view(), View::Named if ty.name().ends_with('?')) {
        out.byte(b'?')?;
    }
    Ok(())
}

pub(crate) fn quote(bytes: &[u8], out: &mut Vec<u8>) {
    quoted(bytes, out).unwrap();
}

/// Writes `bytes` quoted as Go's `%q` renders a string.
pub(crate) fn quoted(bytes: &[u8], out: &mut impl TypeWriter) -> Result<()> {
    out.byte(b'"')?;
    let mut position = 0;
    while position < bytes.len() {
        let (rune, width, valid) = crate::scan::rune(&bytes[position..]);
        let escape = match rune {
            '\u{7}' => Some(b'a'),
            '\u{8}' => Some(b'b'),
            '\u{c}' => Some(b'f'),
            '\n' => Some(b'n'),
            '\r' => Some(b'r'),
            '\t' => Some(b't'),
            '\u{b}' => Some(b'v'),
            '\\' => Some(b'\\'),
            '"' => Some(b'"'),
            _ => None,
        };
        if !valid {
            hex(bytes[position] as u32, 2, b'x', out)?;
        } else if let Some(escape) = escape {
            out.write(&[b'\\', escape])?;
        } else if crate::printable::is_print(rune) {
            out.write(&bytes[position..position + width])?;
        } else if rune < ' ' || rune == '\u{7f}' {
            hex(rune as u32, 2, b'x', out)?;
        } else if rune <= '\u{ffff}' {
            hex(rune as u32, 4, b'u', out)?;
        } else {
            hex(rune as u32, 8, b'U', out)?;
        }
        position += width;
    }
    out.byte(b'"')
}

fn hex(value: u32, digits: usize, prefix: u8, out: &mut impl TypeWriter) -> Result<()> {
    out.write(&[b'\\', prefix])?;
    for index in (0..digits).rev() {
        out.byte(b"0123456789abcdef"[((value >> (index * 4)) & 15) as usize])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    fn shape(value: &Value) -> &Arc<Shape> {
        let Kind::Shape(shape) = &value.0 else {
            panic!("expected a type literal");
        };
        shape
    }

    #[test]
    fn imports_share_metadata_with_independent_reclaimable_charges() {
        let compiled = compile(Type::named("int".into()));
        let mut first = CallContext::new(CallOptions::default());
        let owned = first.import(&compiled).unwrap();
        let retained = first.stats().retained_memory_bytes;
        assert!(retained > size_of::<Shape>());
        let clone = first.import(&owned).unwrap();
        assert!(Arc::ptr_eq(shape(&owned), shape(&clone)));
        assert_eq!(first.stats().peak_memory_bytes, retained);

        let mut second = CallContext::new(CallOptions::default());
        let imported = second.import(&owned).unwrap();
        assert!(!Arc::ptr_eq(shape(&owned), shape(&imported)));
        assert!(Arc::ptr_eq(
            &shape(&owned).definition,
            &shape(&imported).definition
        ));
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(compiled);
        drop(owned);
        assert_eq!(first.stats().retained_memory_bytes, retained);
        drop(clone);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        assert_eq!(imported.as_type_literal(), Some(b"int".as_slice()));
        drop(imported);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn metadata_and_rendering_reserve_before_allocating_and_latch_failure() {
        let value = compile(Type {
            name: String::new(),
            kind: TypeKind::Shape(
                vec![crate::types::Field {
                    name: vec![b'x'; 32768],
                    ty: Type::named("int".into()),
                    optional: false,
                }],
                false,
            ),
            nullable: false,
        });
        let mut small = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(1024),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(small.import(&value).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(small.stats().peak_memory_bytes, 0);
        assert_eq!(small.bytes(b"x").unwrap_err().kind, ErrorKind::Memory);

        let mut full = CallContext::new(CallOptions::default());
        let owned = full.import(&value).unwrap();
        let retained = full.stats().retained_memory_bytes;
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(retained + value.as_type_literal().unwrap().len() + 7),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let imported = ctx.import(&owned).unwrap();
        let mut output = Buffer::empty();
        assert_eq!(
            append(&mut ctx, shape(&imported), &mut output)
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
        assert!(output.data.is_empty());
        assert_eq!(ctx.stats().peak_memory_bytes, retained);
        drop(imported);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }
}
