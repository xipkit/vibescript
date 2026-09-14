use crate::{
    CallContext, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    json, ops,
    value::{Bytes, Kind},
};
use std::fmt::Write;

struct Output {
    bytes: Option<Buffer<u8>>,
    length: usize,
    limit: usize,
}

impl Output {
    fn append(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.limit - self.length {
            return ctx.fail(ErrorKind::OutputLimit, "replacement output exceeds 1 MiB");
        }
        self.length += bytes.len();
        if let Some(output) = &mut self.bytes {
            output.extend(ctx, bytes)
        } else {
            ctx.work_bytes(bytes.len())?;
            ctx.check_memory(Bytes::header_bytes() + self.length)
        }
    }
}

pub(crate) fn render(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Value> {
    if let Kind::Bytes(bytes) = &value.0 {
        if bytes.data.len() > limit {
            return ctx.fail(ErrorKind::OutputLimit, "replacement output exceeds 1 MiB");
        }
        ctx.work_bytes(bytes.data.len())?;
        return Ok(value.clone());
    }
    let mut output = Output {
        bytes: None,
        length: 0,
        limit,
    };
    visit(ctx, value, &mut output, 0)?;
    let length = output.length;
    output.bytes = Some(Buffer::with_capacity(ctx, length)?);
    output.length = 0;
    visit(ctx, value, &mut output, 0)?;
    Value::from_bytes(ctx, output.bytes.unwrap())
}

fn visit(ctx: &mut CallContext, value: &Value, output: &mut Output, depth: usize) -> Result<()> {
    ctx.charge(1)?;
    if depth >= MAX_VALUE_DEPTH && matches!(value.0, Kind::Array(_) | Kind::Hash(_)) {
        return ctx.fail(ErrorKind::Recursion, "replacement string nesting too deep");
    }
    let mut scalar = json::Number::new();
    match &value.0 {
        Kind::Bytes(bytes) | Kind::Symbol(bytes) => output.append(ctx, &bytes.data),
        Kind::Nil => output.append(ctx, b""),
        Kind::Builtin(_) | Kind::Offset(_) => output.append(ctx, b"<builtin>"),
        Kind::Array(array) => {
            output.append(ctx, b"[")?;
            for (index, value) in array.buffer.data.iter().enumerate() {
                if index != 0 {
                    output.append(ctx, b", ")?;
                }
                visit(ctx, value, output, depth + 1)?;
            }
            output.append(ctx, b"]")
        }
        Kind::Hash(hash) if hash.match_data => {
            let index = hash.find(ctx, b"to_s")?.unwrap();
            output.append(ctx, hash.buffer.data[index].1.require_bytes()?)
        }
        Kind::Hash(hash) if hash.object => output.append(ctx, b"<object>"),
        Kind::Hash(hash) => {
            output.append(ctx, b"{")?;
            for (index, (key, value)) in hash.buffer.data.iter().enumerate() {
                if index != 0 {
                    output.append(ctx, b", ")?;
                }
                output.append(ctx, key.require_bytes()?)?;
                output.append(ctx, b": ")?;
                visit(ctx, value, output, depth + 1)?;
            }
            output.append(ctx, b"}")
        }
        Kind::Regex(regex) => regex.render(ctx, |ctx, bytes| output.append(ctx, bytes)),
        Kind::Shape(shape) => {
            output.append(ctx, b"<Shape ")?;
            output.append(ctx, &shape.definition.text)?;
            output.append(ctx, b">")
        }
        Kind::Enum(enumeration) => {
            output.append(ctx, b"<Enum ")?;
            output.append(ctx, enumeration.definition.name.as_bytes())?;
            output.append(ctx, b">")
        }
        Kind::EnumMember(member) => {
            output.append(ctx, member.enumeration.definition.name.as_bytes())?;
            output.append(ctx, b"::")?;
            output.append(ctx, member.definition().name.as_bytes())
        }
        Kind::Range(range) => {
            write!(scalar, "{range}").unwrap();
            output.append(ctx, scalar.bytes())
        }
        _ => {
            if matches!(value.0, Kind::Big(_)) {
                let bits = crate::integer::bits(value);
                let minimum = (bits.saturating_sub(1) as u128 * 301029 / 1_000_000) + 1;
                if minimum > (output.limit - output.length) as u128 {
                    return ctx.fail(ErrorKind::OutputLimit, "replacement output exceeds 1 MiB");
                }
            }
            let text = ops::to_string(ctx, value)?;
            output.append(ctx, text.require_bytes()?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn expansion_preflight_does_not_copy_shared_payloads() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut value = ctx.bytes(&vec![b'x'; 16384]).unwrap();
        for _ in 0..7 {
            let mut children = Buffer::with_capacity(&mut ctx, 2).unwrap();
            children.data.extend([value.clone(), value]);
            value = Value::from_array(&mut ctx, children).unwrap();
        }
        let baseline = ctx.stats();
        let error = render(&mut ctx, &value, 1 << 20).unwrap_err();
        assert_eq!(error.kind, ErrorKind::OutputLimit);
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::OutputLimit);
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn output_projection_checks_memory_before_allocating_a_rendered_string() {
        let mut ctx = CallContext::new(CallOptions::default());
        let text = ctx.bytes(&vec![b'x'; 16384]).unwrap();
        let mut children = Buffer::with_capacity(&mut ctx, 2).unwrap();
        children.data.extend([text.clone(), text]);
        let value = Value::from_array(&mut ctx, children).unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 1024);
        assert_eq!(
            render(&mut ctx, &value, 1 << 20).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn large_integers_are_refused_before_base_conversion() {
        let mut ctx = CallContext::new(CallOptions::default());
        let value =
            crate::integer::binary(&mut ctx, "**", &Value::int(2), &Value::int(4096)).unwrap();
        let baseline = ctx.stats();
        assert_eq!(
            render(&mut ctx, &value, 100).unwrap_err().kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::OutputLimit);
    }

    #[test]
    fn bounded_conversion_preserves_exact_delimiter_boundaries_and_reclaims_scratch() {
        let values = [
            (
                Value::array(vec![Value::nil(), Value::symbol(b"x")]),
                b"[, x]".as_slice(),
            ),
            (
                Value::hash(vec![(b"a".to_vec(), Value::int(7))]),
                b"{a: 7}".as_slice(),
            ),
            (Value::array(vec![Value::array(vec![])]), b"[[]]".as_slice()),
        ];
        for (value, expected) in values {
            let mut ctx = CallContext::new(CallOptions::default());
            let rendered = render(&mut ctx, &value, expected.len()).unwrap();
            assert_eq!(rendered.as_bytes().unwrap(), expected);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                Bytes::header_bytes() + expected.len()
            );
            drop(rendered);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(
                render(&mut ctx, &value, expected.len() - 1)
                    .unwrap_err()
                    .kind,
                ErrorKind::OutputLimit
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
