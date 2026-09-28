use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK},
    scan,
    value::{Bytes, Heap, Kind},
};

pub(super) fn space(byte: u8) -> bool {
    matches!(byte, b' ' | 9..=13)
}

enum Mode<'a> {
    Whitespace,
    Characters,
    Literal(&'a [u8]),
}

struct Split<'a> {
    mode: Mode<'a>,
    limit: i64,
    table: Buffer<usize>,
}

impl<'a> Split<'a> {
    fn new(ctx: &mut CallContext, text: &[u8], args: &'a [Value]) -> Result<Self> {
        if args.len() > 2 {
            return Err(Error::new(
                ErrorKind::Argument,
                "string.split accepts at most a separator and a limit",
            ));
        }
        let limit = match args.get(1) {
            Some(Value(Kind::Int(limit))) => *limit,
            Some(limit) => {
                let message = if limit.is_integer() {
                    "string.split limit must fit in a 64-bit integer"
                } else {
                    "string.split limit must be integer"
                };
                return Err(Error::new(ErrorKind::Type, message));
            }
            None => 0,
        };
        let mode = match args.first() {
            None | Some(Value(Kind::Nil)) => Mode::Whitespace,
            Some(Value(Kind::Bytes(bytes))) => match bytes.data.as_slice() {
                b" " => Mode::Whitespace,
                b"" => Mode::Characters,
                other => Mode::Literal(other),
            },
            _ => {
                return Err(Error::new(
                    ErrorKind::Type,
                    "string.split separator must be string or nil",
                ));
            }
        };
        let mut table = Buffer::empty();
        if let Mode::Literal(needle) = mode {
            if needle.len() <= text.len() && limit != 1 {
                table.ensure(ctx, needle.len())?;
                table.data.push(0);
                let mut matched = 0;
                let mut pending = 0;
                for i in 1..needle.len() {
                    ctx.scan_bytes(&mut pending, 1)?;
                    while matched > 0 && needle[i] != needle[matched] {
                        ctx.scan_bytes(&mut pending, 1)?;
                        matched = table.data[matched - 1];
                    }
                    if needle[i] == needle[matched] {
                        matched += 1;
                    }
                    table.data.push(matched);
                }
                ctx.settle_bytes(&mut pending)?;
            }
        }
        Ok(Self { mode, limit, table })
    }

    fn walk(
        &self,
        ctx: &mut CallContext,
        text: &[u8],
        mut emit: impl FnMut(&mut CallContext, usize, usize) -> Result<bool>,
    ) -> Result<()> {
        ctx.checkpoint()?;
        if text.is_empty() {
            return Ok(());
        }
        if self.limit == 1 {
            emit(ctx, 0, text.len())?;
            return Ok(());
        }
        let mut count = 0usize;
        match self.mode {
            Mode::Whitespace => {
                let mut position = 0;
                while position < text.len() {
                    skip(ctx, text, &mut position, true)?;
                    if position == text.len() {
                        break;
                    }
                    if self.limit > 0 && count as u64 == self.limit as u64 - 1 {
                        emit(ctx, position, text.len())?;
                        return Ok(());
                    }
                    let start = position;
                    skip(ctx, text, &mut position, false)?;
                    if !emit(ctx, start, position)? {
                        return Ok(());
                    }
                    count += 1;
                }
                if self.limit != 0 && space(text[text.len() - 1]) {
                    emit(ctx, text.len(), text.len())?;
                }
            }
            Mode::Characters => {
                let mut position = 0;
                while position < text.len() {
                    ctx.charge(1)?;
                    if self.limit > 1 && count as u64 == self.limit as u64 - 1 {
                        emit(ctx, position, text.len())?;
                        return Ok(());
                    }
                    let end = position + scan::rune(&text[position..]).1;
                    if !emit(ctx, position, end)? {
                        return Ok(());
                    }
                    count += 1;
                    position = end;
                }
                if self.limit != 0 {
                    emit(ctx, text.len(), text.len())?;
                }
            }
            Mode::Literal(needle) => {
                if needle.len() > text.len() {
                    emit(ctx, 0, text.len())?;
                    return Ok(());
                }
                // Byte reads and table transitions are charged as byte work.
                let mut start = 0;
                let mut matched = 0;
                let mut pending = 0;
                for (index, &byte) in text.iter().enumerate() {
                    ctx.scan_bytes(&mut pending, 1)?;
                    while matched > 0 && byte != needle[matched] {
                        ctx.scan_bytes(&mut pending, 1)?;
                        matched = self.table.data[matched - 1];
                    }
                    if byte == needle[matched] {
                        matched += 1;
                    }
                    if matched == needle.len() {
                        if !emit(ctx, start, index + 1 - needle.len())? {
                            return ctx.settle_bytes(&mut pending);
                        }
                        start = index + 1;
                        matched = 0;
                        count += 1;
                        if self.limit > 0 && count as u64 == self.limit as u64 - 1 {
                            ctx.settle_bytes(&mut pending)?;
                            emit(ctx, start, text.len())?;
                            return Ok(());
                        }
                    }
                }
                ctx.settle_bytes(&mut pending)?;
                emit(ctx, start, text.len())?;
            }
        }
        Ok(())
    }
}

fn skip(ctx: &mut CallContext, text: &[u8], position: &mut usize, whitespace: bool) -> Result<()> {
    while *position < text.len() {
        let start = *position;
        let end = start.saturating_add(CHUNK).min(text.len());
        while *position < end && space(text[*position]) == whitespace {
            *position += 1;
        }
        ctx.work_bytes(*position - start)?;
        if *position != end {
            break;
        }
    }
    Ok(())
}

pub(crate) fn call(ctx: &mut CallContext, receiver: &Value, args: &[Value]) -> Result<Value> {
    ctx.checkpoint()?;
    let Kind::Bytes(bytes) = &receiver.0 else {
        return Err(Error::new(ErrorKind::Type, "split requires a string"));
    };
    let text = bytes.data.as_slice();
    let split = Split::new(ctx, text, args)?;
    let mut count = 0usize;
    let mut pending = 0usize;
    let mut projected = Heap::<Value>::header_bytes();
    ctx.check_memory(projected)?;
    split.walk(ctx, text, |ctx, start, end| {
        ctx.charge(1)?;
        if split.limit == 0 && matches!(split.mode, Mode::Literal(_)) && start == end {
            pending += 1;
            return Ok(true);
        }
        let copy = if start == 0 && end == text.len() {
            0
        } else {
            Bytes::header_bytes() + end - start
        };
        let next = pending
            .checked_mul(size_of::<Value>() + Bytes::header_bytes())
            .and_then(|n| n.checked_add(copy))
            .and_then(|n| n.checked_add(size_of::<Value>()))
            .and_then(|n| projected.checked_add(n));
        let Some(next) = next else {
            return ctx.fail(ErrorKind::Memory, "split output size overflow");
        };
        ctx.check_memory(next)?;
        projected = next;
        count += pending + 1;
        pending = 0;
        Ok(true)
    })?;
    let mut output = Buffer::with_capacity(ctx, count)?;
    if count != 0 {
        split.walk(ctx, text, |ctx, start, end| {
            ctx.charge(1)?;
            output.data.push(if start == 0 && end == text.len() {
                receiver.clone()
            } else {
                ctx.bytes(&text[start..end])?
            });
            Ok(output.data.len() < count)
        })?;
    }
    Value::from_array(ctx, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn split_preflights_all_slots_copies_and_search_storage() {
        for short in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let input = ctx.bytes(b"a,,b,").unwrap();
            let separator = ctx.bytes(b",").unwrap();
            let baseline = ctx.stats();
            let storage = Heap::<Value>::header_bytes()
                + 3 * (size_of::<Value>() + Bytes::header_bytes())
                + 2;
            let scratch = size_of::<usize>();
            ctx.options.limits.memory_bytes =
                Some(baseline.retained_memory_bytes + storage + scratch - usize::from(short));
            let output = call(&mut ctx, &input, std::slice::from_ref(&separator));
            if short {
                assert_eq!(output.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(
                    ctx.stats().peak_memory_bytes,
                    baseline.peak_memory_bytes + scratch
                );
                assert_eq!(
                    ctx.stats().retained_memory_bytes,
                    baseline.retained_memory_bytes
                );
                assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            } else {
                let output = output.unwrap();
                let rows = output.as_array().unwrap();
                assert_eq!(
                    rows.iter()
                        .map(|v| v.as_bytes().unwrap())
                        .collect::<Vec<_>>(),
                    [b"a".as_slice(), b"", b"b"]
                );
                assert_eq!(
                    ctx.stats().retained_memory_bytes,
                    baseline.retained_memory_bytes + storage
                );
                assert_eq!(
                    ctx.stats().peak_memory_bytes,
                    baseline.retained_memory_bytes + storage + scratch
                );
                let first = rows[0].clone();
                drop(output);
                drop(input);
                drop(separator);
                assert_eq!(ctx.stats().retained_memory_bytes, Bytes::header_bytes() + 1);
                assert_eq!(first.as_bytes().unwrap(), b"a");
                drop(first);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }

    #[test]
    fn discarded_empty_fields_do_not_allocate_output_slots_or_strings() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(&vec![b','; 65536]).unwrap();
        let separator = ctx.bytes(b",").unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        let storage = Heap::<Value>::header_bytes();
        ctx.options.limits.memory_bytes = Some(baseline + storage + size_of::<usize>());
        let output = call(&mut ctx, &input, std::slice::from_ref(&separator)).unwrap();
        assert!(output.as_array().unwrap().is_empty());
        assert_eq!(ctx.stats().retained_memory_bytes, baseline + storage);
        assert_eq!(
            ctx.stats().peak_memory_bytes,
            baseline + storage + size_of::<usize>()
        );
        drop(output);
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
    }

    #[test]
    fn character_expansion_fails_before_output_allocation() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(&vec![b'a'; 65536]).unwrap();
        let separator = ctx.bytes(b"").unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 512);
        assert_eq!(
            call(&mut ctx, &input, &[separator]).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert!(ctx.stats().steps - baseline.steps < 100);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn whole_input_results_reuse_bytes_without_unnecessary_search_storage() {
        for (text, separator, limit) in [
            (b"abc".as_slice(), b"abcdef".as_slice(), 0),
            (b" a b ", b"a", 1),
            (b"abc", b"", 1),
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            let input = ctx.bytes(text).unwrap();
            let separator = ctx.bytes(separator).unwrap();
            let baseline = ctx.stats().retained_memory_bytes;
            let storage = Heap::<Value>::header_bytes() + size_of::<Value>();
            ctx.options.limits.memory_bytes = Some(baseline + storage);
            let output = call(&mut ctx, &input, &[separator, Value::int(limit)]).unwrap();
            let rows = output.as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0].as_bytes().unwrap().as_ptr(),
                input.as_bytes().unwrap().as_ptr()
            );
            assert_eq!(ctx.stats().peak_memory_bytes, baseline + storage);
        }
    }

    #[test]
    fn work_and_cancellation_failures_release_search_scratch() {
        for cancel in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let input = ctx.bytes(&vec![b'a'; 65536]).unwrap();
            let separator = ctx.bytes(b"aaaaab").unwrap();
            let baseline = ctx.stats();
            if cancel {
                ctx.cancellation().cancel();
            } else {
                ctx.options.limits.steps = Some(baseline.steps + 100);
            }
            let error = call(&mut ctx, &input, std::slice::from_ref(&separator)).unwrap_err();
            assert_eq!(
                error.kind,
                if cancel {
                    ErrorKind::Cancelled
                } else {
                    ErrorKind::Steps
                }
            );
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                baseline.retained_memory_bytes
            );
            assert!(
                ctx.stats().peak_memory_bytes
                    <= baseline.peak_memory_bytes + 6 * size_of::<usize>()
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, error.kind);
        }
    }
}
