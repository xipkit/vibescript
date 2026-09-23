use self::traversal::{Entries, Frame, Stack};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    bytecode::Method,
    json, ops, scan,
    value::Kind,
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
mod traversal;

pub(crate) fn display(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut out = Buffer::empty();
    render(ctx, value, &mut out)?;
    Value::from_bytes(ctx, out)
}

pub(crate) fn append(ctx: &mut CallContext, value: &Value, out: &mut Buffer<u8>) -> Result<()> {
    render(ctx, value, out)
}

fn render(ctx: &mut CallContext, value: &Value, out: &mut Buffer<u8>) -> Result<()> {
    walk(ctx, value, out, MAX_VALUE_DEPTH)
}

/// Renders `value` with one charged frame per open container instead of native recursion.
///
/// Every value costs one step before its depth is checked; a value nested below
/// more than `limit` containers is refused, matching the former recursive guard.
fn walk(ctx: &mut CallContext, value: &Value, out: &mut Buffer<u8>, limit: usize) -> Result<()> {
    let mut stack: Stack<'_, ()> = Stack::new();
    let mut pending = Some(value);
    loop {
        if let Some(value) = pending.take() {
            ctx.charge(1)?;
            if stack.depth() > limit {
                return ctx.guard(ErrorKind::Recursion, "string conversion nesting too deep");
            }
            match &value.0 {
                Kind::Array(h) => {
                    out.push(ctx, b'[')?;
                    stack.push(ctx, Frame::new(Entries::Array(&h.buffer.data), ()))?;
                }
                Kind::Hash(h) if h.tag.protected() => {
                    let index = h.find(ctx, b"to_s")?.unwrap();
                    out.extend(ctx, h.buffer.data[index].1.require_bytes()?)?;
                }
                Kind::Hash(h) if h.object => out.extend(ctx, b"<object>")?,
                Kind::Hash(h) => {
                    out.push(ctx, b'{')?;
                    stack.push(ctx, Frame::new(Entries::Hash(&h.buffer.data), ()))?;
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
                    out.extend(ctx, b", ")?;
                }
                let (key, value) = frame.entries.get(position);
                if let Some(key) = key {
                    out.extend(ctx, key.require_bytes()?)?;
                    out.extend(ctx, b": ")?;
                }
                pending = Some(value);
            }
            None => {
                let closing = frame.entries.closing();
                stack.pop();
                out.extend(ctx, closing)?;
            }
        }
    }
}

fn leaf(ctx: &mut CallContext, value: &Value, out: &mut Buffer<u8>) -> Result<()> {
    match &value.0 {
        Kind::Instance(instance) => {
            out.extend(ctx, b"<")?;
            out.extend(ctx, instance.class().definition.name.as_bytes())?;
            out.extend(ctx, b" instance>")?;
        }
        Kind::Namespace(namespace) => {
            out.extend(ctx, b"<Class ")?;
            out.extend(ctx, namespace.definition.name.as_bytes())?;
            out.extend(ctx, b">")?;
        }
        Kind::Shape(shape) => crate::shapes::append(ctx, shape, out)?,
        Kind::Enum(_) | Kind::EnumMember(_) => crate::enums::append(ctx, value, out)?,
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

/// Rejects arguments to the string member `name`, which takes none.
fn nullary(name: &str, args: &[Value]) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::Argument,
        format!("string.{name} does not take arguments"),
    ))
}

/// Runs the string member `name`, which `method` identifies; `name` also
/// spells the member in its errors.
pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    name: &str,
    value: Value,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    let bytes = string(&value)?;
    match method {
        Lines => {
            nullary(name, args)?;
            iteration::lines(ctx, &value)
        }
        StartWith | EndWith => {
            let (member, part) = if matches!(method, StartWith) {
                ("string.start_with?", "prefix")
            } else {
                ("string.end_with?", "suffix")
            };
            if args.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("{member} expects at least one {part}"),
                ));
            }
            for arg in args {
                ctx.charge(1)?;
                let candidate = string(arg).map_err(|mut error| {
                    error.message = format!("{member} {part} must be string");
                    error
                })?;
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
            nullary(name, args)?;
            if bytes.is_empty() {
                return if matches!(method, Chr) {
                    ctx.bytes(b"")
                } else {
                    Err(Error::new(
                        ErrorKind::Argument,
                        "string.ord requires non-empty string",
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
            nullary(name, args)?;
            let mut out = Buffer::with_capacity(ctx, bytes.len())?;
            for &b in bytes {
                ctx.charge(1)?;
                out.data.push(Value::int(i64::from(b)));
            }
            Value::from_array(ctx, out)
        }
        Chars | Codepoints => {
            nullary(name, args)?;
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
            nullary(name, args)?;
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

#[cfg(test)]
mod tests {
    use super::traversal::support::{nested_arrays, nested_hashes, on_small_stack};
    use super::*;
    use crate::CallOptions;

    fn rendered(ctx: &mut CallContext, value: &Value, limit: usize) -> Result<Vec<u8>> {
        let mut out = Buffer::empty();
        walk(ctx, value, &mut out, limit)?;
        Ok(out.data)
    }

    #[test]
    fn nested_containers_render_with_the_recursive_spelling() {
        let value = Value::array(vec![
            Value::nil(),
            Value::hash(vec![
                (b"a".to_vec(), Value::array(vec![])),
                (b"b".to_vec(), Value::hash(vec![])),
                (
                    b"c".to_vec(),
                    Value::array(vec![Value::int(1), Value::symbol(b"s")]),
                ),
            ]),
            Value::object(vec![(b"z".to_vec(), Value::int(0))]),
            Value::range(Some(1), Some(3), true),
            Value::float(2.5),
        ]);
        let mut ctx = CallContext::new(CallOptions::default());
        let text = display(&mut ctx, &value).unwrap();
        assert_eq!(
            text.as_bytes().unwrap(),
            b"[, {a: [], b: {}, c: [1, s]}, <object>, 1...3, 2.5]"
        );
        drop(text);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn depth_guard_keeps_the_former_recursive_boundary() {
        let mut ctx = CallContext::new(CallOptions::default());
        // A scalar may sit below exactly MAX_VALUE_DEPTH containers.
        let value = nested_arrays(MAX_VALUE_DEPTH, Value::int(7));
        let expected = format!(
            "{}7{}",
            "[".repeat(MAX_VALUE_DEPTH),
            "]".repeat(MAX_VALUE_DEPTH)
        );
        let text = display(&mut ctx, &value).unwrap();
        assert_eq!(text.as_bytes().unwrap(), expected.as_bytes());
        drop(text);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        let value = nested_arrays(MAX_VALUE_DEPTH + 1, Value::int(7));
        let error = display(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(error.message, "string conversion nesting too deep");
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        // The guard counts enclosing containers of the value being rendered, so an
        // empty container below MAX_VALUE_DEPTH others (one more container than
        // the scalar case) still renders, exactly as the recursive walk allowed.
        let value = nested_hashes(MAX_VALUE_DEPTH, Value::hash(vec![]));
        assert!(display(&mut ctx, &value).is_ok());
        let value = nested_hashes(MAX_VALUE_DEPTH + 1, Value::hash(vec![]));
        assert_eq!(
            display(&mut ctx, &value).unwrap_err().kind,
            ErrorKind::Recursion
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.charge(1).unwrap();
    }

    #[test]
    fn ten_thousand_levels_render_on_a_small_native_stack() {
        on_small_stack(|| {
            const DEPTH: usize = 10_000;
            let mut ctx = CallContext::new(CallOptions::default());
            let shallow = nested_arrays(1, Value::int(7));
            rendered(&mut ctx, &shallow, DEPTH).unwrap();
            let baseline = ctx.stats();
            let deep = nested_arrays(DEPTH, Value::int(7));
            let text = rendered(&mut ctx, &deep, DEPTH).unwrap();
            assert_eq!(text.len(), 2 * DEPTH + 1);
            assert!(text[..DEPTH].iter().all(|b| *b == b'['));
            assert_eq!(text[DEPTH], b'7');
            assert!(text[DEPTH + 1..].iter().all(|b| *b == b']'));
            // Each extra level costs one step for the value and one for its closing delimiter.
            assert_eq!(
                ctx.stats().steps - 2 * baseline.steps,
                2 * (DEPTH - 1) as u64
            );
            drop(text);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert!(ctx.stats().peak_memory_bytes < 1 << 20);

            let deeper = nested_hashes(DEPTH + 1, Value::nil());
            let error = rendered(&mut ctx, &deeper, DEPTH).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        });
    }

    #[test]
    fn exhaustion_inside_a_walk_releases_frames_and_stays_latched() {
        let value = nested_arrays(64, Value::int(1));
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = Some(40);
        let error = display(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().steps, 41);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions::default());
        ctx.charge(1).unwrap();
        ctx.options.deadline = Some(std::time::Instant::now());
        let error = display(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Deadline);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.memory_bytes = Some(200);
        let error = display(&mut ctx, &value).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err(), error);
    }
}
