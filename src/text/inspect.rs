use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    hash::Hash,
    json, ops, scan,
    syntax::unicode,
    value::Kind,
};
use std::fmt::Write;

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if name != "inspect"
        || !matches!(
            receiver.0,
            Kind::Nil
                | Kind::Bool(_)
                | Kind::Bytes(_)
                | Kind::Symbol(_)
                | Kind::Array(_)
                | Kind::Hash(_)
                | Kind::Range(_)
        )
    {
        return Ok(None);
    }
    ops::arity(args, 0)?;
    if keywords || block {
        return Err(Error::new(
            ErrorKind::Argument,
            "inspect does not accept keyword arguments or blocks",
        ));
    }
    inspect(ctx, receiver).map(Some)
}

fn inspect(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut projection = Output::Size(0);
    render(ctx, value, &mut projection, 0)?;
    let Output::Size(length) = projection else {
        unreachable!()
    };
    let mut output = Output::Bytes(Buffer::with_capacity(ctx, length)?);
    render(ctx, value, &mut output, 0)?;
    let Output::Bytes(buffer) = output else {
        unreachable!()
    };
    Value::from_bytes(ctx, buffer)
}

enum Output {
    Size(usize),
    Bytes(Buffer<u8>),
}

impl Output {
    fn sizing(&self) -> bool {
        matches!(self, Self::Size(_))
    }

    fn add_size(&mut self, ctx: &mut CallContext, bytes: usize) -> Result<()> {
        let Self::Size(size) = self else {
            unreachable!()
        };
        let Some(next) = size.checked_add(bytes) else {
            return ctx.fail(ErrorKind::Memory, "inspect output size overflow");
        };
        ctx.check_memory(next)?;
        *size = next;
        Ok(())
    }

    fn append(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Size(_) => self.add_size(ctx, bytes.len()),
            Self::Bytes(buffer) => buffer.extend(ctx, bytes),
        }
    }
}

fn render(ctx: &mut CallContext, value: &Value, out: &mut Output, depth: usize) -> Result<()> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH {
        return ctx.fail(ErrorKind::Recursion, "inspect nesting too deep");
    }
    let mut scalar = json::Number::new();
    match &value.0 {
        Kind::Bytes(bytes) => return quoted(ctx, &bytes.data, out),
        Kind::Symbol(bytes) => {
            out.append(ctx, b":")?;
            return label(ctx, &bytes.data, out);
        }
        Kind::Regex(regex) => return regex.render(ctx, |ctx, piece| out.append(ctx, piece)),
        Kind::Nil => return out.append(ctx, b"nil"),
        Kind::Builtin(_) | Kind::Offset(_) => return out.append(ctx, b"<builtin>"),
        Kind::Array(array) => {
            out.append(ctx, b"[")?;
            for (index, element) in array.buffer.data.iter().enumerate() {
                if index > 0 {
                    out.append(ctx, b", ")?;
                }
                render(ctx, element, out, depth + 1)?;
            }
            return out.append(ctx, b"]");
        }
        Kind::Hash(hash) => {
            out.append(ctx, b"{")?;
            let order = if hash.object && !out.sizing() {
                object_order(ctx, hash)?
            } else {
                Buffer::empty()
            };
            for position in 0..hash.buffer.data.len() {
                if position > 0 {
                    out.append(ctx, b", ")?;
                }
                let index = order.data.get(position).copied().unwrap_or(position);
                let (key, value) = &hash.buffer.data[index];
                label(ctx, key.require_bytes()?, out)?;
                out.append(ctx, b": ")?;
                render(ctx, value, out, depth + 1)?;
            }
            return out.append(ctx, b"}");
        }
        Kind::Shape(shape) => {
            out.append(ctx, b"<Shape ")?;
            out.append(ctx, &shape.definition.text)?;
            return out.append(ctx, b">");
        }
        Kind::Namespace(namespace) => {
            out.append(ctx, b"<Class ")?;
            out.append(ctx, namespace.definition.name.as_bytes())?;
            return out.append(ctx, b">");
        }
        Kind::Enum(enumeration) => {
            out.append(ctx, b"<Enum ")?;
            out.append(ctx, enumeration.definition.name.as_bytes())?;
            return out.append(ctx, b">");
        }
        Kind::EnumMember(member) => {
            out.append(ctx, member.enumeration.definition.name.as_bytes())?;
            out.append(ctx, b"::")?;
            return out.append(ctx, member.definition().name.as_bytes());
        }
        Kind::Bool(value) => return out.append(ctx, if *value { b"true" } else { b"false" }),
        Kind::Int(value) => write!(scalar, "{value}").unwrap(),
        Kind::Float(value) => ops::format_float(&mut scalar, *value),
        Kind::Range(range) => write!(scalar, "{range}").unwrap(),
        Kind::Money(money) => write!(scalar, "{money}").unwrap(),
        Kind::Duration(seconds) => write!(scalar, "{seconds}s").unwrap(),
        Kind::Big(_) | Kind::Time(_) | Kind::Zoned(_) => {
            if out.sizing() {
                // Reserve an upper bound without allocating or converting a big integer twice.
                let bytes = if matches!(value.0, Kind::Big(_)) {
                    crate::integer::bits(value) / 3 + 2
                } else {
                    64
                };
                return out.add_size(ctx, bytes);
            }
            let text = ops::to_string(ctx, value)?;
            return out.append(ctx, text.require_bytes()?);
        }
    }
    out.append(ctx, scalar.bytes())
}

fn object_order(ctx: &mut CallContext, hash: &Hash) -> Result<Buffer<usize>> {
    let mut order = Buffer::with_capacity(ctx, hash.buffer.data.len())?;
    for index in 0..hash.buffer.data.len() {
        ctx.charge(1)?;
        order.data.push(index);
    }
    let mut sort = crate::sort::Sort::new(order.data.len());
    let mut comparison = None;
    loop {
        match sort.advance(ctx, comparison.take())? {
            crate::sort::Action::Compare(a, b) => {
                comparison = Some(crate::enums::compare_names(
                    ctx,
                    hash.buffer.data[order.data[a]].0.require_bytes()?,
                    hash.buffer.data[order.data[b]].0.require_bytes()?,
                )?);
            }
            crate::sort::Action::Swap(a, b) => order.data.swap(a, b),
            crate::sort::Action::Done => return Ok(order),
        }
    }
}

fn label(ctx: &mut CallContext, bytes: &[u8], out: &mut Output) -> Result<()> {
    if bare_identifier(ctx, bytes)? {
        out.append(ctx, bytes)
    } else {
        quoted(ctx, bytes, out)
    }
}

fn bare_identifier(ctx: &mut CallContext, bytes: &[u8]) -> Result<bool> {
    let mut index = 0;
    let mut checkpoint = 0;
    while index < bytes.len() {
        if index >= checkpoint {
            ctx.work_bytes((bytes.len() - index).min(CHUNK))?;
            checkpoint = index + CHUNK;
        }
        let (ch, width, valid) = scan::rune(&bytes[index..]);
        if !valid || !(ch == '_' || unicode::letter(ch) || (index > 0 && unicode::digit(ch))) {
            return Ok(false);
        }
        index += width;
    }
    Ok(index > 0)
}

fn quoted(ctx: &mut CallContext, bytes: &[u8], out: &mut Output) -> Result<()> {
    out.append(ctx, b"\"")?;
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if index % CHUNK == 0 {
            ctx.work_bytes((bytes.len() - index).min(CHUNK))?;
        }
        let escaped: &[u8] = match byte {
            b'\\' => b"\\\\",
            b'"' => b"\\\"",
            b'\n' => b"\\n",
            b'\t' => b"\\t",
            b'#' if bytes.get(index + 1) == Some(&b'{') => b"\\#",
            _ => continue,
        };
        out.append(ctx, &bytes[start..index])?;
        out.append(ctx, escaped)?;
        start = index + 1;
    }
    out.append(ctx, &bytes[start..])?;
    out.append(ctx, b"\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn escaped_output_is_projected_before_allocating() {
        let mut ctx = CallContext::new(CallOptions::default());
        let value = ctx.bytes(&vec![b'\n'; 32768]).unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 32768);
        assert_eq!(
            inspect(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn oversized_integer_output_is_rejected_before_decimal_conversion() {
        let mut ctx = CallContext::new(CallOptions::default());
        let integer = Value::parse_integer(&"f".repeat(8192), 16).unwrap();
        let value = ctx.array(&[integer]).unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 128);
        assert_eq!(
            inspect(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert!(ctx.stats().steps - baseline.steps < 10);
    }

    #[test]
    fn shared_graphs_cannot_expand_past_the_budget_during_projection() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut value = ctx.bytes(b"x").unwrap();
        for _ in 0..28 {
            value = ctx.array(&[value.clone(), value]).unwrap();
        }
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 4096);
        assert_eq!(
            inspect(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert!(ctx.stats().steps - baseline.steps < 10000);
    }

    #[test]
    fn object_fields_sort_and_temporary_ordering_storage_is_accounted() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut hash = Hash::empty();
        hash.object = true;
        for index in (0..64).rev() {
            let key = ctx.bytes(format!("k{index:02}").as_bytes()).unwrap();
            hash.insert(&mut ctx, key, Value::nil()).unwrap();
        }
        let object = Value::from_hash(&mut ctx, hash).unwrap();
        let expected = format!(
            "{{{}}}",
            (0..64)
                .map(|n| format!("k{n:02}: nil"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let result = inspect(&mut ctx, &object).unwrap();
        assert_eq!(result.require_bytes().unwrap(), expected.as_bytes());
        drop(result);
        let baseline = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(baseline + expected.len());
        assert_eq!(
            inspect(&mut ctx, &object).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
    }
}
