use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, scan, sequence, value::Kind,
};

fn string(value: &Value) -> Result<&[u8]> {
    match &value.0 {
        Kind::Bytes(bytes) => Ok(&bytes.data),
        _ => Err(Error::new(ErrorKind::Type, "substring must be a string")),
    }
}

pub(super) fn call(
    ctx: &mut CallContext,
    text: &[u8],
    args: &[Value],
    reverse: bool,
) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(Error::new(
            ErrorKind::Argument,
            "index expects a substring and optional offset",
        ));
    }
    let offset = match args.get(1) {
        Some(offset) => sequence::integer(offset)?,
        None => {
            if reverse {
                i64::MAX
            } else {
                0
            }
        }
    };
    if !reverse {
        string(&args[0])?;
    }
    let length = ops::runes(ctx, text)?.0;
    let offset = if offset < 0 {
        length as i128 + i128::from(offset)
    } else {
        i128::from(offset)
    };
    if offset < 0 {
        return Ok(Value::nil());
    }
    let needle = string(&args[0])?;
    if !reverse && offset > length as i128 || needle.len() > text.len().saturating_mul(4) {
        return Ok(Value::nil());
    }
    let offset = offset.min(length as i128) as usize;
    if needle.is_empty() {
        return Ok(Value::int(offset as i64));
    }
    let count = ops::runes(ctx, needle)?.0;
    if count > length || !reverse && count > length - offset {
        return Ok(Value::nil());
    }
    let Some(storage) = count.checked_mul(size_of::<char>() + size_of::<usize>()) else {
        return ctx.fail(ErrorKind::Memory, "substring search size overflow");
    };
    ctx.check_memory(storage)?;
    let mut pattern = Buffer::with_capacity(ctx, count)?;
    let mut table = Buffer::with_capacity(ctx, count)?;
    // Decoded bytes and table transitions are charged as byte work.
    let mut pending = 0;
    let mut position = 0;
    while position < needle.len() {
        let (rune, width, _) = scan::rune(&needle[position..]);
        ctx.scan_bytes(&mut pending, width)?;
        pattern.data.push(rune);
        position += width;
    }
    table.data.push(0usize);
    let mut matched = 0;
    for i in 1..count {
        ctx.scan_bytes(&mut pending, 1)?;
        while matched > 0 && pattern.data[i] != pattern.data[matched] {
            ctx.scan_bytes(&mut pending, 1)?;
            matched = table.data[matched - 1];
        }
        if pattern.data[i] == pattern.data[matched] {
            matched += 1;
        }
        table.data.push(matched);
    }
    let start = if reverse { 0 } else { offset };
    let end = if reverse {
        offset.saturating_add(count).min(length)
    } else {
        length
    };
    position = sequence::rune_offset(ctx, text, start)?;
    matched = 0;
    let mut found = Value::nil();
    for index in start..end {
        let (rune, width, _) = scan::rune(&text[position..]);
        ctx.scan_bytes(&mut pending, width)?;
        position += width;
        while matched > 0 && rune != pattern.data[matched] {
            ctx.scan_bytes(&mut pending, 1)?;
            matched = table.data[matched - 1];
        }
        if rune == pattern.data[matched] {
            matched += 1;
        }
        if matched == count {
            found = Value::int((index + 1 - count) as i64);
            if !reverse {
                break;
            }
            matched = table.data[matched - 1];
        }
    }
    ctx.settle_bytes(&mut pending)?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn invalid_utf8_search_streams_the_subject_and_preflights_only_needle_storage() {
        for short in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let text = vec![255; 65536];
            let needle = ctx.bytes(b"\xef\xbf\xbd\xff").unwrap();
            let baseline = ctx.stats();
            let storage = 2 * (size_of::<char>() + size_of::<usize>());
            ctx.options.limits.memory_bytes =
                Some(baseline.retained_memory_bytes + storage - usize::from(short));
            let output = call(&mut ctx, &text, std::slice::from_ref(&needle), true);
            if short {
                assert_eq!(output.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
                assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            } else {
                assert_eq!(output.unwrap().as_int(), Some(65534));
                assert_eq!(
                    ctx.stats().peak_memory_bytes,
                    baseline.peak_memory_bytes + storage
                );
            }
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                baseline.retained_memory_bytes
            );
        }
    }

    #[test]
    fn overlapping_searches_are_bounded_and_release_interrupted_scratch() {
        let mut ctx = CallContext::new(CallOptions::default());
        let text = vec![b'a'; 65536];
        let mut needle = vec![b'a'; 1024];
        needle[1023] = b'b';
        let needle = ctx.bytes(&needle).unwrap();
        let baseline = ctx.stats();
        // Reads and table transitions cost about 2,000 steps.
        ctx.options.limits.steps = Some(baseline.steps + 1000);
        assert_eq!(
            call(&mut ctx, &text, std::slice::from_ref(&needle), false)
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    }
}
