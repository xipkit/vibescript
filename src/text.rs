use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, bytecode::Method, json, ops,
    scan, value::Kind,
};
use std::fmt::Write;

pub(crate) mod basic;
pub(crate) mod bounded;
pub(crate) mod case;
pub(crate) mod charset;
mod index;
pub(crate) mod inspect;
pub(crate) mod iteration;
pub(crate) mod split;
pub(crate) mod template;
pub(crate) mod transform;

pub(crate) fn display(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut out = Buffer::empty();
    render(ctx, value, &mut out, 0)?;
    Value::from_bytes(ctx, out)
}

pub(crate) fn append(ctx: &mut CallContext, value: &Value, out: &mut Buffer<u8>) -> Result<()> {
    render(ctx, value, out, 0)
}

fn render(ctx: &mut CallContext, value: &Value, out: &mut Buffer<u8>, depth: usize) -> Result<()> {
    ctx.charge(1)?;
    if depth > crate::budget::MAX_VALUE_DEPTH {
        return ctx.fail(ErrorKind::Recursion, "string conversion nesting too deep");
    }
    match &value.0 {
        Kind::Shape(shape) => crate::shapes::append(ctx, shape, out)?,
        Kind::Enum(_) | Kind::EnumMember(_) => crate::enums::append(ctx, value, out)?,
        Kind::Array(h) => {
            out.push(ctx, b'[')?;
            for (i, v) in h.buffer.data.iter().enumerate() {
                if i > 0 {
                    out.extend(ctx, b", ")?;
                }
                render(ctx, v, out, depth + 1)?;
            }
            out.push(ctx, b']')?;
        }
        Kind::Hash(h) if h.match_data => {
            let index = h.find(ctx, b"to_s")?.unwrap();
            out.extend(ctx, h.buffer.data[index].1.require_bytes()?)?;
        }
        Kind::Hash(h) if h.object => out.extend(ctx, b"<object>")?,
        Kind::Hash(h) => {
            out.push(ctx, b'{')?;
            for (i, (k, v)) in h.buffer.data.iter().enumerate() {
                if i > 0 {
                    out.extend(ctx, b", ")?;
                }
                out.extend(ctx, k.require_bytes()?)?;
                out.extend(ctx, b": ")?;
                render(ctx, v, out, depth + 1)?;
            }
            out.push(ctx, b'}')?;
        }
        Kind::Range(r) => {
            let mut text = json::Number::new();
            write!(text, "{r}").unwrap();
            out.extend(ctx, text.bytes())?;
        }
        _ => {
            let atom = ops::to_string(ctx, value)?;
            out.extend(ctx, atom.require_bytes()?)?;
        }
    }
    Ok(())
}

fn string(value: &Value) -> Result<&[u8]> {
    let Kind::Bytes(bytes) = &value.0 else {
        return Err(Error::new(ErrorKind::Type, "expected string"));
    };
    Ok(&bytes.data)
}

pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    value: Value,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    let bytes = string(&value)?;
    match method {
        Lines => {
            ops::arity(args, 0)?;
            iteration::lines(ctx, &value)
        }
        StartWith | EndWith => {
            if args.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "affix predicate expects at least one argument",
                ));
            }
            for arg in args {
                ctx.charge(1)?;
                let candidate = string(arg)?;
                if candidate.len() > bytes.len() {
                    continue;
                }
                let part = if matches!(method, StartWith) {
                    &bytes[..candidate.len()]
                } else {
                    &bytes[bytes.len() - candidate.len()..]
                };
                if json::bytes_equal(ctx, part, candidate)? {
                    return Ok(Value::boolean(true));
                }
            }
            Ok(Value::boolean(false))
        }
        Ord | Chr => {
            ops::arity(args, 0)?;
            if bytes.is_empty() {
                return if matches!(method, Chr) {
                    ctx.bytes(b"")
                } else {
                    Err(Error::new(
                        ErrorKind::Argument,
                        "ord requires a non-empty string",
                    ))
                };
            }
            let (ch, _, _) = scan::rune(bytes);
            if matches!(method, Ord) {
                Ok(Value::int(i64::from(u32::from(ch))))
            } else {
                let mut encoded = [0; 4];
                ctx.bytes(ch.encode_utf8(&mut encoded).as_bytes())
            }
        }
        Bytes => {
            ops::arity(args, 0)?;
            let mut out = Buffer::with_capacity(ctx, bytes.len())?;
            for &b in bytes {
                ctx.charge(1)?;
                out.data.push(Value::int(i64::from(b)));
            }
            Value::from_array(ctx, out)
        }
        Chars | Codepoints => {
            ops::arity(args, 0)?;
            let mut out = Buffer::empty();
            let mut pos = 0;
            while pos < bytes.len() {
                ctx.charge(1)?;
                let (ch, n, _) = scan::rune(&bytes[pos..]);
                let v = if matches!(method, Codepoints) {
                    Value::int(i64::from(u32::from(ch)))
                } else {
                    let mut encoded = [0; 4];
                    ctx.bytes(ch.encode_utf8(&mut encoded).as_bytes())?
                };
                out.push(ctx, v)?;
                pos += n;
            }
            Value::from_array(ctx, out)
        }
        Reverse => {
            ops::arity(args, 0)?;
            let mut chars = Buffer::empty();
            let mut pos = 0;
            while pos < bytes.len() {
                ctx.charge(1)?;
                let (ch, n, _) = scan::rune(&bytes[pos..]);
                chars.push(ctx, ch)?;
                pos += n;
            }
            let mut out = Buffer::empty();
            for ch in chars.data.iter().rev() {
                ctx.charge(1)?;
                let mut encoded = [0; 4];
                out.extend(ctx, ch.encode_utf8(&mut encoded).as_bytes())?;
            }
            Value::from_bytes(ctx, out)
        }
        _ => Err(Error::new(ErrorKind::Type, "unsupported string method")),
    }
}
