use super::traversal::{Entries, Frame, Stack};
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
    render(ctx, value, &mut projection)?;
    let Output::Size(length) = projection else {
        unreachable!()
    };
    let mut output = Output::Bytes(Buffer::with_capacity(ctx, length)?);
    render(ctx, value, &mut output)?;
    let Output::Bytes(buffer) = output else {
        unreachable!()
    };
    Value::from_bytes(ctx, buffer)
}

enum Output {
    Size(usize),
    Bytes(Buffer<u8>),
    Bounded {
        bytes: Option<Buffer<u8>>,
        length: usize,
        limit: usize,
    },
}

impl Output {
    fn sizing(&self) -> bool {
        matches!(self, Self::Size(_) | Self::Bounded { bytes: None, .. })
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
            Self::Bounded {
                bytes: buffer,
                length,
                limit,
            } => {
                if bytes.len() > *limit - *length {
                    return ctx.guard(ErrorKind::OutputLimit, "inspect output exceeds limit");
                }
                *length += bytes.len();
                if let Some(buffer) = buffer {
                    buffer.extend(ctx, bytes)
                } else {
                    ctx.work_bytes(bytes.len())?;
                    ctx.check_memory(*length)
                }
            }
        }
    }
}

pub(crate) fn output(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Buffer<u8>> {
    let mut output = Output::Bounded {
        bytes: None,
        length: 0,
        limit,
    };
    render(ctx, value, &mut output)?;
    let Output::Bounded { length, .. } = output else {
        unreachable!()
    };
    output = Output::Bounded {
        bytes: Some(Buffer::with_capacity(ctx, length + 1)?),
        length: 0,
        limit,
    };
    render(ctx, value, &mut output)?;
    let Output::Bounded {
        bytes: Some(mut bytes),
        ..
    } = output
    else {
        unreachable!()
    };
    bytes.push(ctx, b'\n')?;
    Ok(bytes)
}

fn render(ctx: &mut CallContext, value: &Value, out: &mut Output) -> Result<()> {
    walk(ctx, value, out, MAX_VALUE_DEPTH)
}

/// Sorted field positions for an object hash, owned by its frame while it is open.
type Order = Option<Buffer<usize>>;

/// Inspects `value` with one charged frame per open container.
///
/// Every value costs one step before its depth is checked; a value nested below
/// more than `limit` containers is refused, matching the former recursive guard.
/// Object field ordering is computed when a writing pass opens the hash and is
/// released with its frame, so it is charged for exactly its useful lifetime.
fn walk(ctx: &mut CallContext, value: &Value, out: &mut Output, limit: usize) -> Result<()> {
    let mut stack: Stack<'_, Order> = Stack::new();
    let mut pending = Some(value);
    loop {
        if let Some(value) = pending.take() {
            ctx.charge(1)?;
            if stack.depth() > limit {
                return ctx.guard(ErrorKind::Recursion, "inspect nesting too deep");
            }
            match &value.0 {
                Kind::Array(array) => {
                    out.append(ctx, b"[")?;
                    stack.push(ctx, Frame::new(Entries::Array(&array.buffer.data), None))?;
                }
                Kind::Hash(hash) => {
                    out.append(ctx, b"{")?;
                    let order = if hash.object && !out.sizing() {
                        Some(object_order(ctx, hash)?)
                    } else {
                        None
                    };
                    stack.push(ctx, Frame::new(Entries::Hash(&hash.buffer.data), order))?;
                }
                _ => leaf(ctx, value, out)?,
            }
            continue;
        }
        let Some(frame) = stack.top() else {
            return Ok(());
        };
        match frame.next() {
            Some(position) => {
                if position > 0 {
                    out.append(ctx, b", ")?;
                }
                let index = frame
                    .extra
                    .as_ref()
                    .map_or(position, |order| order.data[position]);
                let (key, value) = frame.entries.get(index);
                if let Some(key) = key {
                    label(ctx, key.require_bytes()?, out)?;
                    out.append(ctx, b": ")?;
                }
                pending = Some(value);
            }
            None => {
                let closing = frame.entries.closing();
                stack.pop();
                out.append(ctx, closing)?;
            }
        }
    }
}

fn leaf(ctx: &mut CallContext, value: &Value, out: &mut Output) -> Result<()> {
    let mut scalar = json::Number::new();
    match &value.0 {
        Kind::Bytes(bytes) => return quoted(ctx, &bytes.data, out),
        Kind::Symbol(bytes) => {
            out.append(ctx, b":")?;
            return label(ctx, &bytes.data, out);
        }
        Kind::Regex(regex) => return regex.render(ctx, |ctx, piece| out.append(ctx, piece)),
        Kind::Nil => return out.append(ctx, b"nil"),
        Kind::Function(function) => return Err(function.value_error()),
        Kind::Host(_) | Kind::Builtin(_) | Kind::Offset(_) => return out.append(ctx, b"<builtin>"),
        Kind::Array(_) | Kind::Hash(_) => unreachable!("containers are walked by frame"),
        Kind::Shape(shape) => {
            out.append(ctx, b"<Shape ")?;
            out.append(ctx, &shape.definition.text)?;
            return out.append(ctx, b">");
        }
        Kind::Instance(instance) => {
            out.append(ctx, b"<")?;
            out.append(ctx, instance.class().definition.name.as_bytes())?;
            return out.append(ctx, b" instance>");
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
            if matches!(out, Output::Size(_)) {
                // Reserve an upper bound without allocating or converting a big integer twice.
                let bytes = if matches!(value.0, Kind::Big(_)) {
                    crate::integer::bits(value) / 3 + 2
                } else {
                    64
                };
                return out.add_size(ctx, bytes);
            }
            if let Output::Bounded { length, limit, .. } = out {
                if matches!(value.0, Kind::Big(_)) {
                    let bits = crate::integer::bits(value);
                    let minimum = (bits.saturating_sub(1) as u128 * 301029 / 1_000_000) + 1;
                    if minimum > (*limit - *length) as u128 {
                        return ctx.guard(ErrorKind::OutputLimit, "inspect output exceeds limit");
                    }
                }
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
    use super::super::traversal::support::{self, nested_arrays, nested_hashes, on_small_stack};
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
        assert_eq!(baseline.peak_memory_bytes, baseline.retained_memory_bytes);
        let scratch = support::peak::<Order>(1);
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + scratch + 128);
        assert_eq!(
            inspect(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Memory
        );
        // The root frame fits; the reserved decimal size is refused before conversion.
        assert_eq!(
            ctx.stats().peak_memory_bytes,
            baseline.retained_memory_bytes + scratch
        );
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
        assert_eq!(baseline.peak_memory_bytes, baseline.retained_memory_bytes);
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 4096);
        assert_eq!(
            inspect(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Memory
        );
        // The projection descends all 28 levels before its size exceeds the quota,
        // so the only memory touched is the charged frame stack.
        assert_eq!(
            ctx.stats().peak_memory_bytes,
            baseline.retained_memory_bytes + support::peak::<Order>(28)
        );
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
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
        // Room for the projected string and the root frame, but not the field order.
        ctx.options.limits.memory_bytes =
            Some(baseline + expected.len() + support::peak::<Order>(1));
        assert_eq!(
            inspect(&mut ctx, &object).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
    }

    fn size(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<usize> {
        let mut projection = Output::Size(0);
        walk(ctx, value, &mut projection, limit)?;
        let Output::Size(length) = projection else {
            unreachable!()
        };
        Ok(length)
    }

    fn write(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Vec<u8>> {
        let length = size(ctx, value, limit)?;
        let mut output = Output::Bytes(Buffer::with_capacity(ctx, length)?);
        walk(ctx, value, &mut output, limit)?;
        let Output::Bytes(buffer) = output else {
            unreachable!()
        };
        Ok(buffer.data)
    }

    #[test]
    fn ten_thousand_levels_inspect_on_a_small_native_stack() {
        on_small_stack(|| {
            const DEPTH: usize = 10_000;
            let mut ctx = CallContext::new(CallOptions::default());
            let shallow = nested_arrays(1, Value::nil());
            size(&mut ctx, &shallow, DEPTH).unwrap();
            let baseline = ctx.stats();
            let deep = nested_arrays(DEPTH, Value::nil());
            let length = size(&mut ctx, &deep, DEPTH).unwrap();
            assert_eq!(length, 2 * DEPTH + 3);
            // Sizing charges one step per value and nothing for projected delimiters.
            assert_eq!(ctx.stats().steps - 2 * baseline.steps, (DEPTH - 1) as u64);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            let text = write(&mut ctx, &deep, DEPTH).unwrap();
            assert_eq!(text.len(), length);
            assert_eq!(
                text,
                format!("{}nil{}", "[".repeat(DEPTH), "]".repeat(DEPTH)).as_bytes()
            );
            drop(text);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert!(ctx.stats().peak_memory_bytes < 4 << 20);

            let deeper = nested_hashes(DEPTH + 1, Value::nil());
            let error = size(&mut ctx, &deeper, DEPTH).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.message, "inspect nesting too deep");
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        });
    }

    #[test]
    fn depth_guard_keeps_the_former_recursive_boundary() {
        let mut ctx = CallContext::new(CallOptions::default());
        let value = nested_arrays(MAX_VALUE_DEPTH, Value::nil());
        let text = inspect(&mut ctx, &value).unwrap();
        assert_eq!(text.require_bytes().unwrap().len(), 2 * MAX_VALUE_DEPTH + 3);
        drop(text);
        let value = nested_arrays(MAX_VALUE_DEPTH + 1, Value::nil());
        assert_eq!(
            inspect(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Recursion
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(
            output(&mut ctx, &value, 1 << 20).unwrap_err().kind,
            ErrorKind::Recursion
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.charge(1).unwrap();
    }

    #[test]
    fn nested_object_orders_are_held_per_frame_and_released_together() {
        let inner = Value::object(vec![
            (b"y".to_vec(), Value::int(1)),
            (b"x".to_vec(), Value::array(vec![Value::symbol(b"s")])),
        ]);
        let value = Value::object(vec![
            (b"z".to_vec(), inner),
            (
                b"a".to_vec(),
                Value::hash(vec![(b"q".to_vec(), Value::nil())]),
            ),
        ]);
        let mut ctx = CallContext::new(CallOptions::default());
        let text = inspect(&mut ctx, &value).unwrap();
        assert_eq!(
            text.require_bytes().unwrap(),
            b"{a: {q: nil}, z: {x: [:s], y: 1}}"
        );
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            crate::value::Bytes::header_bytes() + text.require_bytes().unwrap().len()
        );
        drop(text);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        // The writing pass holds the output, the frames and the live field orders
        // together, so its peak exceeds the frame-only sizing pass.
        let peak = ctx.stats().peak_memory_bytes;
        assert!(peak > support::peak::<Order>(3));

        // Refusing the peak allocation fails inside the writing pass while a field
        // order is still owned by an open frame; everything must be released.
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.memory_bytes = Some(peak - 1);
        let error = inspect(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);
    }

    #[test]
    fn exhaustion_inside_inspect_releases_frames_and_stays_latched() {
        let value = nested_arrays(64, Value::nil());
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = Some(40);
        let error = output(&mut ctx, &value, 1 << 20).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().steps, 41);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions::default());
        ctx.charge(1).unwrap();
        ctx.cancellation().cancel();
        let error = inspect(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.memory_bytes = Some(support::bytes::<Order>(8));
        let error = inspect(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);
    }
}
