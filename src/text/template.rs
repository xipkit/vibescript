use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK},
    hash::Hash,
    json, ops,
    value::Kind,
};

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: &[(Value, Value)],
) -> Result<Option<Value>> {
    if name != "template" || !matches!(receiver.0, Kind::Bytes(_)) {
        return Ok(None);
    }
    ops::arity(args, 1)?;
    if !matches!(args[0].0, Kind::Hash(_)) {
        return Err(Error::new(
            ErrorKind::Type,
            "template context must be a hash or object",
        ));
    }
    let strict = match keywords {
        [] => false,
        [(key, value)] if json::bytes_equal(ctx, key.require_bytes()?, b"strict")? => {
            let Kind::Bool(strict) = value.0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    "template strict option must be boolean",
                ));
            };
            strict
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                "template supports only the strict option",
            ));
        }
    };
    render(ctx, receiver, &args[0], strict).map(Some)
}

fn render(ctx: &mut CallContext, receiver: &Value, context: &Value, strict: bool) -> Result<Value> {
    ctx.checkpoint()?;
    let bytes = receiver.require_bytes()?;
    let mut cache = Hash::empty();
    let mut length = 0;
    let mut last = 0;
    let mut scan = 0;
    while let Some(placeholder) = next(ctx, bytes, &mut scan)? {
        let key = &bytes[placeholder.key.clone()];
        let replacement = if let Some(index) = cache.find(ctx, key)? {
            cache.buffer.data[index].1.clone()
        } else {
            let replacement = match lookup(ctx, context, key)? {
                Some(value) => scalar(ctx, value)?,
                None if !strict => Value::nil(),
                None => {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "template placeholder was not found",
                    ));
                }
            };
            let key = ctx.bytes(key)?;
            cache.insert(ctx, key, replacement.clone())?;
            replacement
        };
        add_length(ctx, &mut length, placeholder.open - last)?;
        let count = if matches!(replacement.0, Kind::Nil) {
            placeholder.end - placeholder.open
        } else {
            replacement.require_bytes()?.len()
        };
        add_length(ctx, &mut length, count)?;
        last = placeholder.end;
    }
    if last == 0 {
        return Ok(receiver.clone());
    }
    add_length(ctx, &mut length, bytes.len() - last)?;
    let mut output = Buffer::with_capacity(ctx, length)?;
    last = 0;
    scan = 0;
    while let Some(placeholder) = next(ctx, bytes, &mut scan)? {
        output.extend(ctx, &bytes[last..placeholder.open])?;
        let index = cache.find(ctx, &bytes[placeholder.key])?.unwrap();
        let replacement = &cache.buffer.data[index].1;
        let part = if matches!(replacement.0, Kind::Nil) {
            &bytes[placeholder.open..placeholder.end]
        } else {
            replacement.require_bytes()?
        };
        output.extend(ctx, part)?;
        last = placeholder.end;
    }
    output.extend(ctx, &bytes[last..])?;
    Value::from_bytes(ctx, output)
}

fn add_length(ctx: &mut CallContext, length: &mut usize, bytes: usize) -> Result<()> {
    let Some(next) = length.checked_add(bytes) else {
        return ctx.fail(ErrorKind::Memory, "template output size overflow");
    };
    ctx.check_memory(next)?;
    *length = next;
    Ok(())
}

fn scalar(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    ctx.charge(1)?;
    match &value.0 {
        Kind::Bytes(_) | Kind::Symbol(_) => Ok(value.clone()),
        Kind::EnumMember(member) => ctx.bytes(member.definition().symbol.as_bytes()),
        Kind::Nil
        | Kind::Bool(_)
        | Kind::Int(_)
        | Kind::Big(_)
        | Kind::Float(_)
        | Kind::Money(_)
        | Kind::Duration(_)
        | Kind::Time(_)
        | Kind::Zoned(_) => ops::to_string(ctx, value),
        _ => Err(Error::new(
            ErrorKind::Type,
            "template placeholder value must be scalar",
        )),
    }
}

fn lookup<'a>(ctx: &mut CallContext, context: &'a Value, path: &[u8]) -> Result<Option<&'a Value>> {
    let mut current = context;
    let mut start = 0;
    for (index, byte) in path.iter().enumerate() {
        if index % CHUNK == 0 {
            ctx.work_bytes((path.len() - index).min(CHUNK))?;
        }
        if *byte == b'.' {
            let Some(value) = field(ctx, current, &path[start..index])? else {
                return Ok(None);
            };
            current = value;
            start = index + 1;
        }
    }
    field(ctx, current, &path[start..])
}

fn field<'a>(ctx: &mut CallContext, value: &'a Value, key: &[u8]) -> Result<Option<&'a Value>> {
    ctx.charge(1)?;
    let Kind::Hash(hash) = &value.0 else {
        return Ok(None);
    };
    if key.is_empty() {
        return Ok(None);
    }
    Ok(hash.find(ctx, key)?.map(|index| &hash.buffer.data[index].1))
}

struct Placeholder {
    open: usize,
    key: std::ops::Range<usize>,
    end: usize,
}

fn next(ctx: &mut CallContext, bytes: &[u8], scan: &mut usize) -> Result<Option<Placeholder>> {
    let mut visited = 0;
    while *scan < bytes.len() {
        scan_step(ctx, &mut visited)?;
        let open = *scan;
        *scan += 1;
        if bytes[open] != b'{' || bytes.get(open + 1) != Some(&b'{') {
            continue;
        }
        let mut index = open + 2;
        while bytes.get(index).is_some_and(|&byte| space(byte)) {
            scan_step(ctx, &mut visited)?;
            index += 1;
        }
        if !bytes.get(index).is_some_and(|&byte| key_start(byte)) {
            continue;
        }
        let start = index;
        while bytes.get(index).is_some_and(|&byte| key_byte(byte)) {
            scan_step(ctx, &mut visited)?;
            index += 1;
        }
        let end = index;
        while bytes.get(index).is_some_and(|&byte| space(byte)) {
            scan_step(ctx, &mut visited)?;
            index += 1;
        }
        if bytes.get(index) == Some(&b'}') && bytes.get(index + 1) == Some(&b'}') {
            *scan = index + 2;
            return Ok(Some(Placeholder {
                open,
                key: start..end,
                end: *scan,
            }));
        }
    }
    Ok(None)
}

fn scan_step(ctx: &mut CallContext, visited: &mut u8) -> Result<()> {
    if *visited % 64 == 0 {
        ctx.charge(1)?;
    }
    *visited = visited.wrapping_add(1);
    Ok(())
}

fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r')
}

fn key_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn key_byte(byte: u8) -> bool {
    key_start(byte) || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn repeated_large_scalars_are_converted_once_after_the_eighth_key() {
        let huge = Value::parse_integer(&"f".repeat(8192), 16).unwrap();
        let mut ctx = CallContext::new(CallOptions::default());
        let huge = ctx.import(&huge).unwrap();
        let before = ctx.stats().steps;
        let text = scalar(&mut ctx, &huge).unwrap();
        let conversion_steps = ctx.stats().steps - before;
        let digits = text.require_bytes().unwrap().len();
        drop(text);
        let mut context = Hash::empty();
        for index in 0..9 {
            let key = ctx.bytes(format!("k{index}").as_bytes()).unwrap();
            context
                .insert(
                    &mut ctx,
                    key,
                    if index == 8 {
                        huge.clone()
                    } else {
                        Value::int(0)
                    },
                )
                .unwrap();
        }
        let context = Value::from_hash(&mut ctx, context).unwrap();
        let prefix = (0..8).map(|n| format!("{{{{k{n}}}}}")).collect::<String>();
        let template = ctx
            .bytes((prefix + &"{{k8}}".repeat(8)).as_bytes())
            .unwrap();
        let baseline = ctx.stats();
        let output = render(&mut ctx, &template, &context, true).unwrap();
        assert_eq!(output.require_bytes().unwrap().len(), 8 + 8 * digits);
        assert!(ctx.stats().steps - baseline.steps < 2 * conversion_steps);
        drop(output);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
    }

    #[test]
    fn projected_output_fails_before_allocation_and_releases_the_cache() {
        let mut ctx = CallContext::new(CallOptions::default());
        let text = ctx.bytes(b"{{x}}{{x}}{{x}}").unwrap();
        let context = ctx
            .import(&Value::hash(vec![(
                b"x".to_vec(),
                Value::bytes(vec![b'a'; 8192]),
            )]))
            .unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(baseline + 4096);
        assert_eq!(
            render(&mut ctx, &text, &context, false).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        assert!(ctx.stats().peak_memory_bytes < baseline + 4096);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn literal_templates_reuse_storage_and_still_check_cancellation() {
        let mut ctx = CallContext::new(CallOptions::default());
        let text = ctx.bytes(b"plain {{?invalid}}").unwrap();
        let context = ctx.import(&Value::hash(vec![])).unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(baseline);
        let output = render(&mut ctx, &text, &context, false).unwrap();
        assert_eq!(
            output.require_bytes().unwrap().as_ptr(),
            text.require_bytes().unwrap().as_ptr()
        );
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        ctx.cancellation().cancel();
        assert_eq!(
            render(&mut ctx, &text, &context, false).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Cancelled);
    }
}
