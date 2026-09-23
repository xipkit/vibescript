use super::traversal::{Entries, Frame, Stack};
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
    header: Option<usize>,
    prefix: bool,
    runes: Option<usize>,
}

impl Output {
    fn append(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        if self.done() {
            return Ok(());
        }
        let bytes = if self.prefix {
            &bytes[..bytes.len().min(self.limit - self.length)]
        } else {
            bytes
        };
        if bytes.len() > self.limit - self.length {
            return ctx.guard(ErrorKind::OutputLimit, "output exceeds limit 1048576 bytes");
        }
        self.length += bytes.len();
        if let Some(runes) = &mut self.runes {
            *runes += ops::runes(ctx, bytes)?.0;
        }
        if let Some(output) = &mut self.bytes {
            output.extend(ctx, bytes)
        } else {
            ctx.work_bytes(bytes.len())?;
            if let Some(header) = self.header {
                ctx.check_memory(header + self.length)?;
            }
            Ok(())
        }
    }

    fn done(&self) -> bool {
        self.prefix && self.length == self.limit
    }
}

pub(crate) fn measure(
    ctx: &mut CallContext,
    value: &Value,
    limit: usize,
    prefix: bool,
) -> Result<usize> {
    ctx.checkpoint()?;
    let mut output = Output {
        bytes: None,
        length: 0,
        limit,
        header: None,
        prefix,
        runes: None,
    };
    visit(ctx, value, &mut output)?;
    Ok(output.length)
}

pub(crate) fn measure_runes(ctx: &mut CallContext, value: &Value) -> Result<usize> {
    let mut output = Output {
        bytes: None,
        length: 0,
        limit: usize::MAX,
        header: None,
        prefix: false,
        runes: Some(0),
    };
    visit(ctx, value, &mut output)?;
    Ok(output.runes.unwrap())
}

pub(crate) fn prefix(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Value> {
    let length = measure(ctx, value, limit, true)?;
    let mut output = Output {
        bytes: Some(Buffer::with_capacity(ctx, length)?),
        length: 0,
        limit: length,
        header: None,
        prefix: true,
        runes: None,
    };
    visit(ctx, value, &mut output)?;
    Value::from_bytes(ctx, output.bytes.unwrap())
}

pub(crate) fn render(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Value> {
    if let Kind::Bytes(bytes) = &value.0 {
        if bytes.data.len() > limit {
            return ctx.guard(ErrorKind::OutputLimit, "output exceeds limit 1048576 bytes");
        }
        ctx.work_bytes(bytes.data.len())?;
        return Ok(value.clone());
    }
    let mut output = Output {
        bytes: None,
        length: 0,
        limit,
        header: Some(Bytes::header_bytes()),
        prefix: false,
        runes: None,
    };
    visit(ctx, value, &mut output)?;
    let length = output.length;
    output.bytes = Some(Buffer::with_capacity(ctx, length)?);
    output.length = 0;
    visit(ctx, value, &mut output)?;
    Value::from_bytes(ctx, output.bytes.unwrap())
}

pub(crate) fn output(
    ctx: &mut CallContext,
    value: &Value,
    limit: usize,
    newline: bool,
) -> Result<Buffer<u8>> {
    let mut output = Output {
        bytes: None,
        length: 0,
        limit,
        header: Some(0),
        prefix: false,
        runes: None,
    };
    visit(ctx, value, &mut output)?;
    output.bytes = Some(Buffer::with_capacity(
        ctx,
        output.length + usize::from(newline),
    )?);
    output.length = 0;
    visit(ctx, value, &mut output)?;
    let mut bytes = output.bytes.unwrap();
    if newline {
        bytes.push(ctx, b'\n')?;
    }
    Ok(bytes)
}

fn visit(ctx: &mut CallContext, value: &Value, output: &mut Output) -> Result<()> {
    walk(ctx, value, output, MAX_VALUE_DEPTH)
}

/// Projects or writes `value` with one charged frame per open container.
///
/// Containers may open only below `limit` enclosing containers, matching the
/// former recursive guard. Once a prefix projection is complete the walk stops
/// immediately: no further steps are charged and the frames are released.
fn walk(ctx: &mut CallContext, value: &Value, output: &mut Output, limit: usize) -> Result<()> {
    let mut stack: Stack<'_, ()> = Stack::new();
    let mut pending = Some(value);
    loop {
        if output.done() {
            return Ok(());
        }
        if let Some(value) = pending.take() {
            ctx.charge(1)?;
            if stack.depth() >= limit && matches!(value.0, Kind::Array(_) | Kind::Hash(_)) {
                return ctx.guard(ErrorKind::Recursion, "replacement string nesting too deep");
            }
            match &value.0 {
                Kind::Array(array) => {
                    output.append(ctx, b"[")?;
                    stack.push(ctx, Frame::new(Entries::Array(&array.buffer.data), ()))?;
                }
                Kind::Hash(hash) if hash.tag.protected() => {
                    let index = hash.find(ctx, b"to_s")?.unwrap();
                    output.append(ctx, hash.buffer.data[index].1.require_bytes()?)?;
                }
                Kind::Hash(hash) if hash.object => output.append(ctx, b"<object>")?,
                Kind::Hash(hash) => {
                    output.append(ctx, b"{")?;
                    stack.push(ctx, Frame::new(Entries::Hash(&hash.buffer.data), ()))?;
                }
                _ => leaf(ctx, value, output)?,
            }
            continue;
        }
        let Some(frame) = stack.top() else {
            return Ok(());
        };
        match frame.next() {
            Some(position) => {
                if position != 0 {
                    output.append(ctx, b", ")?;
                }
                let (key, value) = frame.entries.get(position);
                if let Some(key) = key {
                    output.append(ctx, key.require_bytes()?)?;
                    output.append(ctx, b": ")?;
                }
                pending = Some(value);
            }
            None => {
                let closing = frame.entries.closing();
                stack.pop();
                output.append(ctx, closing)?;
            }
        }
    }
}

fn leaf(ctx: &mut CallContext, value: &Value, output: &mut Output) -> Result<()> {
    let mut scalar = json::Number::new();
    match &value.0 {
        Kind::Bytes(bytes) | Kind::Symbol(bytes) => output.append(ctx, &bytes.data),
        Kind::Nil => output.append(ctx, b""),
        Kind::Builtin(_) | Kind::Offset(_) => output.append(ctx, b"<builtin>"),
        Kind::Regex(regex) => regex.render(ctx, |ctx, bytes| output.append(ctx, bytes)),
        Kind::Instance(instance) => {
            output.append(ctx, b"<")?;
            output.append(ctx, instance.class().definition.name.as_bytes())?;
            output.append(ctx, b" instance>")
        }
        Kind::Namespace(namespace) => {
            output.append(ctx, b"<Class ")?;
            output.append(ctx, namespace.definition.name.as_bytes())?;
            output.append(ctx, b">")
        }
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
            if !output.prefix && matches!(value.0, Kind::Big(_)) {
                let bits = crate::integer::bits(value);
                let minimum = (bits.saturating_sub(1) as u128 * 301029 / 1_000_000) + 1;
                if minimum > (output.limit - output.length) as u128 {
                    return ctx.guard(ErrorKind::OutputLimit, "output exceeds limit 1048576 bytes");
                }
            }
            let text = ops::to_string(ctx, value)?;
            output.append(ctx, text.require_bytes()?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::traversal::support::{self, nested_arrays, nested_hashes, on_small_stack};
    use super::*;
    use crate::CallOptions;

    #[test]
    fn precision_prefixes_stop_before_expanding_shared_graphs() {
        let mut value = Value::array(vec![Value::bytes(vec![b'x'; 1 << 20])]);
        for _ in 0..24 {
            value = Value::array(vec![value.clone(), value]);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = Some(100);
        let result = prefix(&mut ctx, &value, 1).unwrap();
        assert_eq!(result.as_bytes(), Some(b"[".as_slice()));
        assert!(ctx.stats().peak_memory_bytes < 128);
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn measured_and_rendered_prefixes_preserve_bytes_and_boundaries() {
        let value = Value::array(vec![
            Value::bytes([0xc3, 0xa9, 0xff]),
            Value::hash(vec![(b"x".to_vec(), Value::int(7))]),
        ]);
        let expected = b"[\xc3\xa9\xff, {x: 7}]";
        for limit in 0..=expected.len() + 1 {
            let mut ctx = CallContext::new(CallOptions::default());
            let length = measure(&mut ctx, &value, limit, true).unwrap();
            assert_eq!(length, expected.len().min(limit));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert!(ctx.stats().peak_memory_bytes < 256);
            let result = prefix(&mut ctx, &value, limit).unwrap();
            assert_eq!(result.as_bytes(), Some(&expected[..length]));
            drop(result);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

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
        assert_eq!(baseline.peak_memory_bytes, baseline.retained_memory_bytes);
        let error = render(&mut ctx, &value, 1 << 20).unwrap_err();
        assert_eq!(error.kind, ErrorKind::OutputLimit);
        // Only the seven charged traversal frames were added; no payload was copied.
        assert_eq!(
            ctx.stats().peak_memory_bytes,
            baseline.retained_memory_bytes + support::peak::<()>(7)
        );
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        ctx.charge(1).unwrap();
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
        assert_eq!(baseline.peak_memory_bytes, baseline.retained_memory_bytes);
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 1024);
        assert_eq!(
            render(&mut ctx, &value, 1 << 20).unwrap_err().kind,
            ErrorKind::Memory
        );
        // The projection refuses the string before allocating it; only the root frame was charged.
        assert_eq!(
            ctx.stats().peak_memory_bytes,
            baseline.retained_memory_bytes + support::peak::<()>(1)
        );
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
        ctx.charge(1).unwrap();
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

    fn project(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<usize> {
        let mut output = Output {
            bytes: None,
            length: 0,
            limit: usize::MAX,
            header: None,
            prefix: false,
            runes: None,
        };
        walk(ctx, value, &mut output, limit)?;
        Ok(output.length)
    }

    fn write(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Vec<u8>> {
        let length = project(ctx, value, limit)?;
        let mut output = Output {
            bytes: Some(Buffer::with_capacity(ctx, length)?),
            length: 0,
            limit: length,
            header: None,
            prefix: false,
            runes: None,
        };
        walk(ctx, value, &mut output, limit)?;
        Ok(output.bytes.unwrap().data)
    }

    #[test]
    fn ten_thousand_levels_project_and_write_on_a_small_native_stack() {
        on_small_stack(|| {
            const DEPTH: usize = 10_000;
            let mut ctx = CallContext::new(CallOptions::default());
            let shallow = nested_hashes(1, Value::int(7));
            project(&mut ctx, &shallow, DEPTH).unwrap();
            let baseline = ctx.stats();
            let deep = nested_hashes(DEPTH, Value::int(7));
            let length = project(&mut ctx, &deep, DEPTH).unwrap();
            assert_eq!(length, 5 * DEPTH + 1);
            // Each extra level charges the value, its opening brace, key, separator and closing brace.
            assert_eq!(
                ctx.stats().steps - 2 * baseline.steps,
                5 * (DEPTH - 1) as u64
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            let text = write(&mut ctx, &deep, DEPTH).unwrap();
            assert_eq!(text.len(), length);
            assert_eq!(
                text,
                format!("{}7{}", "{k: ".repeat(DEPTH), "}".repeat(DEPTH)).as_bytes()
            );
            drop(text);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert!(ctx.stats().peak_memory_bytes < 1 << 20);

            let deeper = nested_arrays(DEPTH + 1, Value::int(7));
            let error = project(&mut ctx, &deeper, DEPTH).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.message, "replacement string nesting too deep");
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        });
    }

    #[test]
    fn containers_may_open_only_below_the_depth_limit() {
        let mut ctx = CallContext::new(CallOptions::default());
        let value = nested_arrays(MAX_VALUE_DEPTH, Value::int(7));
        assert_eq!(
            measure(&mut ctx, &value, usize::MAX, false).unwrap(),
            2 * MAX_VALUE_DEPTH + 1
        );
        let value = nested_arrays(MAX_VALUE_DEPTH + 1, Value::int(7));
        for prefix in [false, true] {
            let error = measure(&mut ctx, &value, usize::MAX, prefix).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
        assert_eq!(
            output(&mut ctx, &value, 1 << 20, true).unwrap_err().kind,
            ErrorKind::Recursion
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.charge(1).unwrap();
    }

    #[test]
    fn completed_prefixes_stop_the_walk_and_release_every_frame() {
        let value = nested_arrays(40, Value::int(7));
        let mut ctx = CallContext::new(CallOptions::default());
        let steps = ctx.stats().steps;
        let result = prefix(&mut ctx, &value, 5).unwrap();
        assert_eq!(result.as_bytes(), Some(b"[[[[[".as_slice()));
        // Both passes stop once the fifth container opens: five values and five
        // opening brackets charged in each pass, nothing for the untouched levels.
        assert_eq!(ctx.stats().steps - steps, 20);
        assert_eq!(ctx.stats().retained_memory_bytes, Bytes::header_bytes() + 5);
        assert_eq!(ctx.stats().peak_memory_bytes, 5 + support::peak::<()>(5));
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn exhaustion_inside_output_projection_releases_frames_and_stays_latched() {
        let value = nested_arrays(64, Value::int(1));
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = Some(40);
        let error = output(&mut ctx, &value, 1 << 20, true).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions::default());
        ctx.charge(1).unwrap();
        ctx.cancellation().cancel();
        let error = render(&mut ctx, &value, 1 << 20).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.memory_bytes = Some(support::bytes::<()>(8));
        let error = measure(&mut ctx, &value, usize::MAX, false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);
    }
}
