use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, bytecode::Method, json, ops,
    scan, sequence, value::Kind,
};

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}

fn string(value: &Value) -> Result<&[u8]> {
    if let Kind::Bytes(bytes) = &value.0 {
        Ok(&bytes.data)
    } else {
        Err(Error::new(ErrorKind::Type, "expected string"))
    }
}

fn window(
    ctx: &mut CallContext,
    receiver: &Value,
    start: usize,
    end: usize,
    bang: bool,
) -> Result<Value> {
    let bytes = string(receiver)?;
    if start == 0 && end == bytes.len() {
        return Ok(if bang { Value::nil() } else { receiver.clone() });
    }
    ctx.bytes(&bytes[start..end])
}

fn strip_space(byte: u8) -> bool {
    matches!(byte, 0 | 9..=13 | 32)
}

fn strip(ctx: &mut CallContext, bytes: &[u8], left: bool, right: bool) -> Result<(usize, usize)> {
    let mut start = 0;
    let mut end = bytes.len();
    if left {
        while start < end {
            let length = (end - start).min(4096);
            ctx.work_bytes(length)?;
            let skipped = bytes[start..start + length]
                .iter()
                .take_while(|&&b| strip_space(b))
                .count();
            start += skipped;
            if skipped != length {
                break;
            }
        }
    }
    if right {
        while end > start {
            let length = (end - start).min(4096);
            ctx.work_bytes(length)?;
            let skipped = bytes[end - length..end]
                .iter()
                .rev()
                .take_while(|&&b| strip_space(b))
                .count();
            end -= skipped;
            if skipped != length {
                break;
            }
        }
    }
    Ok((start, end))
}

fn last_rune(bytes: &[u8]) -> usize {
    let end = bytes.len();
    let mut start = end - 1;
    for _ in 0..3 {
        if start == 0 || bytes[start] & 0xc0 != 0x80 {
            break;
        }
        start -= 1;
    }
    let (_, width, valid) = scan::rune(&bytes[start..]);
    if valid && start + width == end {
        start
    } else {
        end - 1
    }
}

fn white_space(rune: char) -> bool {
    matches!(rune, '\t'..='\r' | ' ' | '\u{85}' | '\u{a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}')
}

fn fields(
    ctx: &mut CallContext,
    bytes: &[u8],
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
) -> Result<()> {
    let mut position = 0;
    let mut start = None;
    let mut emitted = false;
    while position < bytes.len() {
        let end = position + (bytes.len() - position).min(4096);
        ctx.work_bytes(end - position)?;
        while position < end {
            let (rune, width, _) = scan::rune(&bytes[position..]);
            if white_space(rune) {
                if let Some(begin) = start.take() {
                    if emitted {
                        emit(ctx, b" ")?;
                    }
                    emit(ctx, &bytes[begin..position])?;
                    emitted = true;
                }
            } else if start.is_none() {
                start = Some(position);
            }
            position += width;
        }
    }
    if let Some(begin) = start {
        if emitted {
            emit(ctx, b" ")?;
        }
        emit(ctx, &bytes[begin..])?;
    }
    Ok(())
}

fn squish(ctx: &mut CallContext, receiver: &Value, bang: bool) -> Result<Value> {
    let bytes = string(receiver)?;
    let mut length = 0;
    let mut changed = false;
    fields(ctx, bytes, |ctx, field| {
        let end = length + field.len();
        changed |= !json::bytes_equal(ctx, &bytes[length..end], field)?;
        length = end;
        Ok(())
    })?;
    changed |= length != bytes.len();
    if !changed {
        return Ok(if bang { Value::nil() } else { receiver.clone() });
    }
    let mut output = Buffer::with_capacity(ctx, length)?;
    fields(ctx, bytes, |ctx, field| output.extend(ctx, field))?;
    Value::from_bytes(ctx, output)
}

fn affix(ctx: &mut CallContext, bytes: &[u8], part: &[u8], suffix: bool) -> Result<bool> {
    if part.len() > bytes.len() {
        return Ok(false);
    }
    let window = if suffix {
        &bytes[bytes.len() - part.len()..]
    } else {
        &bytes[..part.len()]
    };
    json::bytes_equal(ctx, window, part)
}

#[derive(Clone, Copy)]
struct Pad {
    repeats: usize,
    prefix: usize,
    bytes: usize,
}

fn plan_pad(ctx: &mut CallContext, pad: &[u8], runes: usize, count: usize) -> Result<Pad> {
    let repeats = count / runes;
    let prefix = sequence::rune_offset(ctx, pad, count % runes)?;
    let Some(bytes) = repeats
        .checked_mul(pad.len())
        .and_then(|n| n.checked_add(prefix))
    else {
        return ctx.fail(ErrorKind::Memory, "padding output size overflow");
    };
    Ok(Pad {
        repeats,
        prefix,
        bytes,
    })
}

fn write_pad(ctx: &mut CallContext, output: &mut Buffer<u8>, pad: &[u8], plan: Pad) -> Result<()> {
    for _ in 0..plan.repeats {
        output.extend(ctx, pad)?;
    }
    output.extend(ctx, &pad[..plan.prefix])
}

fn padding(ctx: &mut CallContext, name: &str, receiver: &Value, args: &[Value]) -> Result<Value> {
    if args.is_empty() || args.len() > 2 {
        return Err(argument("padding expects width and an optional pad string"));
    }
    let width = sequence::integer(&args[0]).map_err(|_| {
        argument("padding width must be a finite number within the 64-bit integer range")
    })?;
    let pad = if args.len() == 2 {
        string(&args[1])?
    } else {
        b" "
    };
    if pad.is_empty() {
        return Err(argument("padding string must not be empty"));
    }
    let bytes = string(receiver)?;
    let length = ops::runes(ctx, bytes)?.0;
    if i128::from(width) <= length as i128 {
        return Ok(receiver.clone());
    }
    let Ok(count) = usize::try_from(width as u64 - length as u64) else {
        return ctx.fail(ErrorKind::Memory, "padding output size overflow");
    };
    let left = match name {
        "rjust" => count,
        "center" => count / 2,
        _ => 0,
    };
    let runes = ops::runes(ctx, pad)?.0;
    let right = plan_pad(ctx, pad, runes, count - left)?;
    let left = plan_pad(ctx, pad, runes, left)?;
    let Some(size) = left
        .bytes
        .checked_add(right.bytes)
        .and_then(|n| n.checked_add(bytes.len()))
    else {
        return ctx.fail(ErrorKind::Memory, "padding output size overflow");
    };
    ctx.work_bytes(size)?;
    let mut output = Buffer::with_capacity(ctx, size)?;
    write_pad(ctx, &mut output, pad, left)?;
    output.extend(ctx, bytes)?;
    write_pad(ctx, &mut output, pad, right)?;
    Value::from_bytes(ctx, output)
}

fn partition(ctx: &mut CallContext, receiver: &Value, part: &[u8], last: bool) -> Result<Value> {
    let bytes = string(receiver)?;
    let found = ops::find(ctx, bytes, part, last)?;
    let ranges = if let Some(start) = found {
        [
            (0, start),
            (start, start + part.len()),
            (start + part.len(), bytes.len()),
        ]
    } else if last {
        [(0, 0), (0, 0), (0, bytes.len())]
    } else {
        [(0, bytes.len()), (0, 0), (0, 0)]
    };
    let mut output = Buffer::with_capacity(ctx, 3)?;
    for (start, end) in ranges {
        output.data.push(window(ctx, receiver, start, end, false)?);
    }
    Value::from_array(ctx, output)
}

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<Option<Value>> {
    let Kind::Bytes(input) = &receiver.0 else {
        return Ok(None);
    };
    if matches!(
        name,
        "center" | "ljust" | "rjust" | "partition" | "rpartition"
    ) {
        ctx.charge(1)?;
        if keywords {
            return Err(argument(
                "padding and partition do not accept keyword arguments",
            ));
        }
        return if matches!(name, "partition" | "rpartition") {
            ops::arity(args, 1)?;
            partition(ctx, receiver, string(&args[0])?, name == "rpartition").map(Some)
        } else {
            padding(ctx, name, receiver, args).map(Some)
        };
    }
    let bang = name.ends_with('!');
    let name = name.strip_suffix('!').unwrap_or(name);
    if !matches!(
        name,
        "strip"
            | "lstrip"
            | "rstrip"
            | "squish"
            | "chomp"
            | "chop"
            | "delete_prefix"
            | "delete_suffix"
    ) && !(name == "reverse" && bang)
    {
        return Ok(None);
    }
    ctx.charge(1)?;
    let bytes = &input.data;
    let (start, end) = match name {
        "strip" | "lstrip" | "rstrip" => {
            ops::arity(args, 0)?;
            strip(ctx, bytes, name != "rstrip", name != "lstrip")?
        }
        "squish" => {
            ops::arity(args, 0)?;
            return squish(ctx, receiver, bang).map(Some);
        }
        "chomp" => {
            let end = match args {
                [] if bytes.ends_with(b"\r\n") => bytes.len() - 2,
                [] if bytes.last().is_some_and(|b| matches!(b, b'\r' | b'\n')) => bytes.len() - 1,
                [] | [Value(Kind::Nil)] => bytes.len(),
                [part] => {
                    let part = string(part)?;
                    if part.is_empty() {
                        let mut end = bytes.len();
                        while end > 0 {
                            let length = end.min(4096);
                            ctx.work_bytes(length)?;
                            let skipped = bytes[end - length..end]
                                .iter()
                                .rev()
                                .take_while(|&&b| matches!(b, b'\r' | b'\n'))
                                .count();
                            end -= skipped;
                            if skipped != length {
                                break;
                            }
                        }
                        end
                    } else if affix(ctx, bytes, part, true)? {
                        bytes.len() - part.len()
                    } else {
                        bytes.len()
                    }
                }
                _ => return Err(argument("chomp accepts at most one separator")),
            };
            (0, end)
        }
        "chop" => {
            ops::arity(args, 0)?;
            let end = if bytes.ends_with(b"\r\n") {
                bytes.len() - 2
            } else if bytes.is_empty() {
                0
            } else {
                last_rune(bytes)
            };
            (0, end)
        }
        "delete_prefix" | "delete_suffix" => {
            ops::arity(args, 1)?;
            let part = string(&args[0])?;
            let suffix = name == "delete_suffix";
            if affix(ctx, bytes, part, suffix)? {
                if suffix {
                    (0, bytes.len() - part.len())
                } else {
                    (part.len(), bytes.len())
                }
            } else {
                (0, bytes.len())
            }
        }
        "reverse" => {
            let result = super::method(ctx, Method::Reverse, receiver.clone(), args)?;
            let same = json::bytes_equal(ctx, bytes, result.require_bytes()?)?;
            return Ok(Some(if same { Value::nil() } else { result }));
        }
        _ => unreachable!(),
    };
    window(ctx, receiver, start, end, bang).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn unchanged_transforms_reuse_storage_and_bang_results_release_it() {
        for (method, args) in [
            ("strip", vec![]),
            ("lstrip", vec![]),
            ("rstrip", vec![]),
            ("squish", vec![]),
            ("chomp", vec![]),
            ("chomp", vec![Value::nil()]),
            ("delete_prefix", vec![Value::bytes("z")]),
            ("delete_suffix", vec![Value::bytes("")]),
        ] {
            for bang in [false, true] {
                let mut ctx = CallContext::new(CallOptions::default());
                let original = ctx.bytes(&vec![b'a'; 131072]).unwrap();
                let before = ctx.stats().retained_memory_bytes;
                let name = format!("{method}{}", if bang { "!" } else { "" });
                let result = call(&mut ctx, &name, &original, &args, false)
                    .unwrap()
                    .unwrap();
                assert_eq!(ctx.stats().peak_memory_bytes, before, "{name}");
                assert_eq!(ctx.stats().retained_memory_bytes, before);
                assert_eq!(matches!(result.0, Kind::Nil), bang);
                drop(original);
                assert_eq!(
                    ctx.stats().retained_memory_bytes,
                    if bang { 0 } else { before }
                );
                drop(result);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }

    #[test]
    fn padding_preflights_work_memory_and_size_overflow_before_allocation() {
        for method in ["center", "ljust", "rjust"] {
            for (limits, width, pad, expected) in [
                (
                    Limits {
                        memory_bytes: Some(8192),
                        ..Limits::default()
                    },
                    131072,
                    "é界",
                    ErrorKind::Memory,
                ),
                (
                    Limits {
                        steps: Some(64),
                        ..Limits::default()
                    },
                    i64::MAX,
                    "x",
                    ErrorKind::Steps,
                ),
                (
                    Limits {
                        steps: None,
                        ..Limits::default()
                    },
                    i64::MAX,
                    "🙂",
                    ErrorKind::Memory,
                ),
            ] {
                let mut ctx = CallContext::new(CallOptions {
                    limits,
                    ..CallOptions::default()
                });
                let input = ctx.import(&Value::bytes("x")).unwrap();
                let before = ctx.stats().retained_memory_bytes;
                let args = [Value::int(width), Value::bytes(pad)];
                let error = call(&mut ctx, method, &input, &args, false).unwrap_err();
                assert_eq!(error.kind, expected, "{method}");
                assert_eq!(ctx.stats().peak_memory_bytes, before);
                assert_eq!(ctx.stats().retained_memory_bytes, before);
                assert_eq!(ctx.charge(0).unwrap_err().kind, expected);
                drop(input);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }

    #[test]
    fn interrupted_scans_and_copies_release_temporary_storage() {
        for (method, raw, args) in [
            ("strip", vec![b' '; 65536], vec![]),
            ("lstrip", vec![0; 65536], vec![]),
            ("rstrip", vec![b'\t'; 65536], vec![]),
            ("squish", "a\u{3000}".repeat(16384).into_bytes(), vec![]),
            ("chomp", vec![b'\n'; 65536], vec![Value::bytes("")]),
            ("chop", vec![b'a'; 65536], vec![]),
            (
                "delete_prefix",
                vec![b'a'; 65536],
                vec![Value::bytes(vec![b'a'; 65536])],
            ),
            (
                "delete_suffix",
                vec![b'a'; 65536],
                vec![Value::bytes(vec![b'a'; 65536])],
            ),
            (
                "partition",
                vec![b'a'; 65536],
                vec![Value::bytes(vec![b'a'; 8192])],
            ),
            (
                "rpartition",
                vec![b'a'; 65536],
                vec![Value::bytes(vec![b'a'; 8192])],
            ),
            ("reverse!", vec![b'a'; 65536], vec![]),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let input = ctx.import(&Value::bytes(raw)).unwrap();
            let before = ctx.stats().retained_memory_bytes;
            let error = call(&mut ctx, method, &input, &args, false).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Steps, "{method}");
            assert_eq!(ctx.stats().retained_memory_bytes, before, "{method}");
            assert!(ctx.stats().steps < 256, "{method}");
            assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Steps);
            drop(input);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn cancellation_and_deadlines_remain_latched_even_for_noop_inputs() {
        for raw in ["", " x \r\n"] {
            for (method, args) in [
                ("strip", vec![]),
                ("lstrip!", vec![]),
                ("rstrip", vec![]),
                ("squish!", vec![]),
                ("chomp", vec![Value::nil()]),
                ("chop!", vec![]),
                ("delete_prefix!", vec![Value::bytes("")]),
                ("delete_suffix", vec![Value::bytes("absent")]),
                ("reverse!", vec![]),
                ("center", vec![Value::int(0)]),
                ("ljust", vec![Value::int(0)]),
                ("rjust", vec![Value::int(0)]),
                ("partition", vec![Value::bytes("")]),
                ("rpartition", vec![Value::bytes("")]),
            ] {
                for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
                    let mut options = CallOptions::default();
                    if kind == ErrorKind::Deadline {
                        options.deadline = Some(std::time::Instant::now());
                    } else {
                        options.cancellation.cancel();
                    }
                    let mut ctx = CallContext::new(options);
                    let error =
                        call(&mut ctx, method, &Value::bytes(raw), &args, false).unwrap_err();
                    assert_eq!(error.kind, kind, "{method}");
                    assert_eq!(ctx.stats().peak_memory_bytes, 0);
                    assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
                }
            }
        }
    }
}
