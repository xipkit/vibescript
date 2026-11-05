use crate::budget::CHUNK;
use crate::checked::Checked;
use crate::numeric::{to_i64, to_usize};
use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, scan, sequence, value::Kind,
};

fn string<'a>(value: &'a Value, member: &str) -> Result<&'a [u8]> {
    match &value.0 {
        Kind::Bytes(bytes) => Ok(bytes.data()),
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
        string(args.at(0)?, member)?;
    }
    let (length, valid) = ops::runes(ctx, text)?;
    let offset = if offset < 0 {
        length as i128 + i128::from(offset)
    } else {
        i128::from(offset)
    };
    if offset < 0 {
        return Ok(Value::nil());
    }
    let needle = string(args.at(0)?, member)?;
    if !reverse && offset > length as i128 || needle.len() > text.len().saturating_mul(4) {
        return Ok(Value::nil());
    }
    let offset = to_usize(offset.min(length as i128));
    if needle.is_empty() {
        return Ok(Value::int(to_i64(offset)));
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
        let (rune, width, _) = scan::rune(needle.tail(position)?);
        ctx.scan_bytes(&mut pending, width)?;
        pattern.data.push(rune);
        position += width;
    }
    table.data.push(0usize);
    let mut matched = 0;
    for rune in pattern.data.iter().skip(1) {
        ctx.scan_bytes(&mut pending, 1)?;
        while matched > 0 && rune != pattern.at(matched)? {
            ctx.scan_bytes(&mut pending, 1)?;
            matched = *table.at(matched - 1)?;
        }
        if rune == pattern.at(matched)? {
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
    let found = if valid && length == text.len() && end - start >= 16 && pattern.at(0)?.is_ascii() {
        search_ascii(
            ctx,
            text,
            &pattern.data,
            &table.data,
            start..end,
            &mut pending,
            reverse,
        )?
    } else {
        search_runes(
            ctx,
            text,
            &pattern.data,
            &table.data,
            start..end,
            &mut pending,
            reverse,
        )?
    };
    ctx.settle_bytes(&mut pending)?;
    Ok(found)
}

// The rune loop needs its own registers for the same reason as the ASCII loop.
#[inline(never)]
fn search_runes(
    ctx: &mut CallContext,
    text: &[u8],
    pattern: &[char],
    table: &[usize],
    range: std::ops::Range<usize>,
    pending: &mut usize,
    reverse: bool,
) -> Result<Value> {
    let mut position = sequence::rune_offset(ctx, text, range.start)?;
    let mut matched = 0;
    let mut found = Value::nil();
    for index in range {
        let (rune, width) = scan::code_point(text.tail(position)?);
        ctx.scan_bytes(pending, width)?;
        position += width;
        while matched > 0 && rune != u32::from(*pattern.at(matched)?) {
            ctx.scan_bytes(pending, 1)?;
            matched = *table.at(matched - 1)?;
        }
        if rune == u32::from(*pattern.at(matched)?) {
            matched += 1;
        }
        if matched == pattern.len() {
            found = Value::int(to_i64(index + 1 - pattern.len()));
            if !reverse {
                break;
            }
            matched = *table.at(matched - 1)?;
        }
    }
    Ok(found)
}

// A function of its own, so the per-byte loop's registers are not shared
// with the rune counting and table building around the call.
#[inline(never)]
fn search_ascii(
    ctx: &mut CallContext,
    text: &[u8],
    pattern: &[char],
    table: &[usize],
    range: std::ops::Range<usize>,
    pending: &mut usize,
    reverse: bool,
) -> Result<Value> {
    let mut index = sequence::rune_offset(ctx, text, range.start)?;
    let text = text.span(0, range.end)?;
    let mut matched = 0;
    let mut found = Value::nil();
    while index < text.len() {
        let remaining = text.tail(index)?;
        if matched == 0 && remaining.len() >= 16 {
            let limit = remaining.len().min(CHUNK - *pending);
            let skipped = scan::ascii_mismatch(remaining.span(0, limit)?, *pattern.at(0)? as u8);
            if skipped != 0 {
                ctx.scan_bytes(pending, skipped)?;
                index += skipped;
                continue;
            }
        }
        let rune = char::from(*remaining.at(0)?);
        ctx.scan_bytes(pending, 1)?;
        while matched > 0 && rune != *pattern.at(matched)? {
            ctx.scan_bytes(pending, 1)?;
            matched = *table.at(matched - 1)?;
        }
        if rune == *pattern.at(matched)? {
            matched += 1;
        }
        if matched == pattern.len() {
            found = Value::int(to_i64(index + 1 - pattern.len()));
            if !reverse {
                break;
            }
            matched = *table.at(matched - 1)?;
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
    fn invalid_ascii_search_ranges_and_tables_return_internal_errors() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut pending = 0;
        for (end, table) in [(5, &[][..]), (4, &[][..])] {
            let error = search_ascii(
                &mut ctx,
                b"aaac",
                &['a', 'a', 'b'],
                table,
                0..end,
                &mut pending,
                false,
            )
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Internal);
            assert!(error.message.contains("out of range"));
        }
    }

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
                            &[needle, Value::int(to_i64(offset))],
                            reverse,
                        )
                        .unwrap();
                        assert_eq!(output.as_int(), expected.map(|&i| to_i64(i)));
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
