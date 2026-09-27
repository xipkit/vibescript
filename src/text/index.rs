#[cfg(any(all(feature = "simd", target_arch = "x86_64"), test))]
use crate::budget::CHUNK;
use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, scan, sequence, value::Kind,
};

fn string<'a>(value: &'a Value, member: &str) -> Result<&'a [u8]> {
    match &value.0 {
        Kind::Bytes(bytes) => Ok(&bytes.data),
        _ => Err(Error::new(
            ErrorKind::Type,
            format!("{member} substring must be string"),
        )),
    }
}

pub(super) fn call(
    ctx: &mut CallContext,
    text: &[u8],
    args: &[Value],
    reverse: bool,
) -> Result<Value> {
    let member = if reverse {
        "string.rindex"
    } else {
        "string.index"
    };
    if args.is_empty() || args.len() > 2 {
        return Err(Error::new(
            ErrorKind::Argument,
            format!("{member} expects substring and optional offset"),
        ));
    }
    let offset = match args.get(1) {
        Some(offset) => sequence::integer(offset).map_err(|mut error| {
            error.message = format!("{member} offset must be integer");
            error
        })?,
        None => {
            if reverse {
                i64::MAX
            } else {
                0
            }
        }
    };
    if !reverse {
        string(&args[0], member)?;
    }
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    let (length, valid) = ops::runes(ctx, text)?;
    #[cfg(not(all(feature = "simd", target_arch = "x86_64")))]
    let length = ops::runes(ctx, text)?.0;
    let offset = if offset < 0 {
        length as i128 + i128::from(offset)
    } else {
        i128::from(offset)
    };
    if offset < 0 {
        return Ok(Value::nil());
    }
    let needle = string(&args[0], member)?;
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
    #[cfg(all(feature = "simd", target_arch = "x86_64"))]
    if valid && length == text.len() && end - start >= 16 && pattern.data[0].is_ascii() {
        let found = search_ascii(
            ctx,
            text,
            &pattern.data,
            &table.data,
            start..end,
            &mut pending,
            reverse,
        )?;
        ctx.settle_bytes(&mut pending)?;
        return Ok(found);
    }
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

// Keep the verified ASCII bounds visible at the call site.
#[cfg(all(feature = "simd", target_arch = "x86_64"))]
#[inline(always)]
fn search_ascii(
    ctx: &mut CallContext,
    text: &[u8],
    pattern: &[char],
    table: &[usize],
    range: std::ops::Range<usize>,
    pending: &mut usize,
    reverse: bool,
) -> Result<Value> {
    let mut index = range.start;
    let mut matched = 0;
    let mut found = Value::nil();
    while index < range.end {
        let remaining = &text[index..range.end];
        if matched == 0 && remaining.len() >= 16 {
            let limit = remaining.len().min(CHUNK - *pending);
            let skipped = scan::ascii_mismatch(&remaining[..limit], pattern[0] as u8);
            if skipped != 0 {
                ctx.scan_bytes(pending, skipped)?;
                index += skipped;
                continue;
            }
        }
        let rune = char::from(remaining[0]);
        ctx.scan_bytes(pending, 1)?;
        while matched > 0 && rune != pattern[matched] {
            ctx.scan_bytes(pending, 1)?;
            matched = table[matched - 1];
        }
        if rune == pattern[matched] {
            matched += 1;
        }
        if matched == pattern.len() {
            found = Value::int((index + 1 - pattern.len()) as i64);
            if !reverse {
                break;
            }
            matched = table[matched - 1];
        }
        index += 1;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn malformed_single_byte_runes_keep_replacement_character_matching() {
        let mut text = vec![b'x'; 32];
        text.extend_from_slice(b"a\xffaba\xfe");
        for reverse in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let needle = ctx.bytes("a\u{fffd}".as_bytes()).unwrap();
            let output = call(&mut ctx, &text, &[needle], reverse).unwrap();
            assert_eq!(output.as_int(), Some(if reverse { 36 } else { 32 }));
        }
    }

    #[test]
    fn ascii_search_preserves_offsets_across_vector_and_accounting_boundaries() {
        for prefix in [15, 16, 17, CHUNK - 1, CHUNK, CHUNK + 1] {
            let text = format!("{}aba{}aba", "x".repeat(prefix), "x".repeat(32));
            for needle in ["aba", "ax", "aé", "\u{fffd}"] {
                let positions: Vec<_> = text
                    .as_bytes()
                    .windows(needle.len())
                    .enumerate()
                    .filter_map(|(i, bytes)| (bytes == needle.as_bytes()).then_some(i))
                    .collect();
                for offset in [0, 15, 16, prefix - 1, prefix, prefix + 1, text.len()] {
                    for reverse in [false, true] {
                        let expected = if reverse {
                            positions.iter().rev().find(|&&i| i <= offset)
                        } else {
                            positions.iter().find(|&&i| i >= offset)
                        };
                        let mut ctx = CallContext::new(CallOptions::default());
                        let needle = ctx.bytes(needle.as_bytes()).unwrap();
                        let output = call(
                            &mut ctx,
                            text.as_bytes(),
                            &[needle, Value::int(offset as i64)],
                            reverse,
                        )
                        .unwrap();
                        assert_eq!(output.as_int(), expected.map(|&i| i as i64));
                    }
                }
            }
        }
    }

    #[test]
    fn skipped_ascii_still_exhausts_steps_and_releases_search_storage() {
        let text = vec![b'x'; CHUNK * 3];
        for reverse in [false, true] {
            for allowance in [192, 193, 194, 255, 256, 257, 319, 320, 321] {
                let mut ctx = CallContext::new(CallOptions::default());
                let needle = ctx.bytes(b"missing").unwrap();
                let baseline = ctx.stats();
                ctx.options.limits.steps = Some(baseline.steps + allowance);
                assert_eq!(
                    call(&mut ctx, &text, std::slice::from_ref(&needle), reverse)
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
    }

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
