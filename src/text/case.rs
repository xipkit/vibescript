use crate::checked::Checked;
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK},
    ops, scan,
    value::Kind,
};
use std::cmp::Ordering;

mod data;

#[derive(Clone, Copy)]
enum Transform {
    Upper,
    Lower,
    Capitalize,
    Swap,
    Fold,
}

impl Transform {
    fn index(self, first: bool) -> usize {
        match self {
            Self::Upper => 0,
            Self::Lower => 1,
            Self::Capitalize if first => 2,
            Self::Capitalize => 1,
            Self::Fold => 3,
            Self::Swap => 4,
        }
    }

    fn ascii(self, bytes: &mut [u8], first: bool) {
        match self {
            Self::Upper => scan::ascii_case(bytes, true),
            Self::Lower | Self::Fold => scan::ascii_case(bytes, false),
            Self::Capitalize => {
                scan::ascii_case(bytes, false);
                if first {
                    if let Some(byte) = bytes.first_mut() {
                        *byte = byte.to_ascii_uppercase();
                    }
                }
            }
            Self::Swap => {
                for byte in bytes {
                    if byte.is_ascii_alphabetic() {
                        *byte ^= 32;
                    }
                }
            }
        }
    }
}

/// Returns the case-fold class of a rune: equal folds compare equal ignoring case.
pub(crate) fn fold(rune: char) -> u32 {
    entry(rune).map_or(rune as u32, |(_, fold)| fold)
}

fn entry(rune: char) -> Option<(&'static [u32; 5], u32)> {
    let point = rune as u32;
    let index = data::MAPPINGS.partition_point(|&(r, _, _)| r < point);
    data::MAPPINGS
        .get(index)
        .filter(|&&(r, _, _)| r == point)
        .map(|(_, maps, fold)| (maps, *fold))
}

/// Maps `bytes` a run at a time, hands each mapped piece to `emit` and
/// reports whether any piece changed.
///
/// `emit` returns the steps its own work costs. They are summed with the
/// walk's, a step per 64 bytes of an ASCII run and one for every other
/// character, and charged with a checkpoint once per chunk of input: in text
/// that mixes scripts a piece is a few bytes, too short to charge and
/// checkpoint one at a time. They are charged as single steps, so a call the
/// limit stops reports a count one past it, as it did when its short pieces
/// were charged one at a time.
fn walk(
    ctx: &mut CallContext,
    bytes: &[u8],
    transform: Transform,
    ascii: bool,
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<u64>,
) -> Result<bool> {
    let mut changed = false;
    let mut position = 0;
    let mut scratch = [0; 4096];
    // Settled after every chunk or two of input, so the sum cannot wrap.
    let mut pending = 0u64;
    while position < bytes.len() {
        let settle = bytes.len().min(position + CHUNK);
        while position < settle {
            let end = position + (bytes.len() - position).min(scratch.len());
            let chunk = bytes.span(position, end)?;
            let length = if ascii {
                chunk.len()
            } else {
                scan::ascii_prefix(chunk)
            };
            if length > 0 {
                pending = pending.wrapping_add((length as u64).div_ceil(64));
                let original = chunk.span(0, length)?;
                let mapped = scratch.span_mut(0, length)?;
                mapped.copy_from_slice(original);
                transform.ascii(mapped, position == 0);
                changed |= mapped != original;
                pending = pending.wrapping_add(emit(ctx, mapped)?);
                position += length;
                continue;
            }
            pending = pending.wrapping_add(1);
            let (rune, length, _) = scan::rune(bytes.tail(position)?);
            let original = bytes.span(position, position + length)?;
            let encoded = entry(rune)
                .and_then(|(maps, _)| maps.get(transform.index(position == 0)))
                .copied()
                .unwrap_or(0);
            let mapped = if encoded == 0 {
                original
            } else {
                let start = (encoded >> 4) as usize;
                data::BYTES.span(start, start + (encoded & 15) as usize)?
            };
            changed |= mapped != original;
            pending = pending.wrapping_add(emit(ctx, mapped)?);
            position += length;
        }
        ctx.charge_each(std::mem::take(&mut pending))?;
        ctx.checkpoint()?;
    }
    Ok(changed)
}

fn transform(
    ctx: &mut CallContext,
    receiver: &Value,
    bytes: &[u8],
    transform: Transform,
    ascii: bool,
    bang: bool,
) -> Result<Value> {
    let ascii = ascii || !ops::runes(ctx, bytes)?.1;
    let mut size = 0usize;
    let changed = walk(ctx, bytes, transform, ascii, |ctx, mapped| {
        let Some(next) = size.checked_add(mapped.len()) else {
            return ctx.fail(ErrorKind::Memory, "case conversion output size overflow");
        };
        size = next;
        Ok(0)
    })?;
    if !changed {
        return Ok(if bang { Value::nil() } else { receiver.clone() });
    }
    let mut output = Buffer::with_capacity(ctx, size)?;
    walk(ctx, bytes, transform, ascii, |_, mapped| {
        output.data.extend_from_slice(mapped);
        Ok((mapped.len() as u64).div_ceil(64))
    })?;
    Value::from_bytes(ctx, output)
}

fn ascii_compare(ctx: &mut CallContext, a: &[u8], b: &[u8]) -> Result<Ordering> {
    for (a, b) in a.chunks(4096).zip(b.chunks(4096)) {
        ctx.work_bytes(a.len().min(b.len()))?;
        for (&a, &b) in a.iter().zip(b) {
            let result = a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase());
            if result != Ordering::Equal {
                return Ok(result);
            }
        }
    }
    Ok(a.len().cmp(&b.len()))
}

pub(crate) fn equal(ctx: &mut CallContext, mut a: &[u8], mut b: &[u8]) -> Result<bool> {
    if !ops::runes(ctx, a)?.1 || !ops::runes(ctx, b)?.1 {
        return Ok(ascii_compare(ctx, a, b)? == Ordering::Equal);
    }
    while !a.is_empty() && !b.is_empty() {
        ctx.charge(1)?;
        let (left, n, _) = scan::rune(a);
        let (right, m, _) = scan::rune(b);
        if fold(left) != fold(right) {
            return Ok(false);
        }
        a = a.tail(n)?;
        b = b.tail(m)?;
    }
    Ok(a.is_empty() && b.is_empty())
}

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
) -> Result<Option<Value>> {
    let Kind::Bytes(input) = &receiver.0 else {
        return Ok(None);
    };
    let bytes = input.data();
    if matches!(name, "casecmp" | "casecmp?") {
        ctx.charge(1)?;
        if args.len() != 1 {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("string.{name} expects exactly one string"),
            ));
        }
        let Kind::Bytes(other) = &args.at(0)?.0 else {
            return Ok(Some(Value::nil()));
        };
        return Ok(Some(if name == "casecmp?" {
            Value::boolean(equal(ctx, bytes, other.data())?)
        } else {
            Value::int(match ascii_compare(ctx, bytes, other.data())? {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            })
        }));
    }
    let full = name;
    let bang = name.ends_with('!');
    let name = name.strip_suffix('!').unwrap_or(name);
    let mut operation = match name {
        "upcase" => Transform::Upper,
        "downcase" => Transform::Lower,
        "capitalize" => Transform::Capitalize,
        "swapcase" => Transform::Swap,
        _ => return Ok(None),
    };
    ctx.charge(1)?;
    let ascii = match args {
        [] => false,
        [mode] if matches!(mode.0, Kind::Symbol(_)) => match mode.require_bytes()? {
            b"ascii" => true,
            b"fold" if name == "downcase" => {
                operation = Transform::Fold;
                false
            }
            option => {
                let bang = if bang { "!" } else { "" };
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!(
                        "string.{name}{bang} does not support the :{} option",
                        String::from_utf8_lossy(option)
                    ),
                ));
            }
        },
        [_, _, ..] => {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("string.{full} accepts at most one case-mapping option"),
            ));
        }
        _ => {
            let bang = if bang { "!" } else { "" };
            let problem = if args.len() > 1 {
                "accepts at most one case-mapping option"
            } else {
                "option must be a symbol"
            };
            return Err(Error::new(
                ErrorKind::Argument,
                format!("string.{name}{bang} {problem}"),
            ));
        }
    };
    transform(ctx, receiver, bytes, operation, ascii, bang).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn unchanged_results_reuse_storage_and_bang_results_keep_no_output() {
        for method in ["upcase", "upcase!"] {
            let mut ctx = CallContext::new(CallOptions::default());
            let original = ctx.bytes(&vec![b'A'; 131072]).unwrap();
            let before = ctx.stats().retained_memory_bytes;
            let result = call(&mut ctx, method, &original, &[]).unwrap().unwrap();
            assert_eq!(ctx.stats().peak_memory_bytes, before);
            assert_eq!(ctx.stats().retained_memory_bytes, before);
            assert_eq!(matches!(result.0, Kind::Nil), method.ends_with('!'));
            drop(original);
            if method.ends_with('!') {
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            } else {
                assert_eq!(ctx.stats().retained_memory_bytes, before);
            }
            drop(result);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn expansion_is_reserved_before_copying_and_failed_calls_release_scratch() {
        let source = "ΐ".repeat(8192);
        for (limits, expected) in [
            (
                Limits {
                    memory_bytes: Some(source.len() + 4096),
                    ..Limits::default()
                },
                ErrorKind::Memory,
            ),
            (
                Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ErrorKind::Steps,
            ),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let input = ctx.import(&Value::bytes(source.clone())).unwrap();
            let before = ctx.stats().retained_memory_bytes;
            assert!(matches!(call(&mut ctx, "upcase", &input, &[]), Err(e) if e.kind == expected));
            assert_eq!(ctx.stats().peak_memory_bytes, before);
            assert_eq!(ctx.stats().retained_memory_bytes, before);
            assert_eq!(ctx.charge(0).unwrap_err().kind, expected);
            drop(input);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn cancellation_and_deadlines_remain_latched_for_empty_and_nonempty_inputs() {
        for source in ["", "Straße"] {
            for method in [
                "upcase",
                "downcase",
                "capitalize",
                "swapcase",
                "upcase!",
                "casecmp",
                "casecmp?",
            ] {
                for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
                    let mut options = CallOptions::default();
                    if kind == ErrorKind::Deadline {
                        options.deadline = Some(std::time::Instant::now());
                    } else {
                        options.cancellation.cancel();
                    }
                    let mut ctx = CallContext::new(options);
                    let input = Value::bytes(source);
                    let args = if method.starts_with("casecmp") {
                        vec![input.clone()]
                    } else {
                        vec![]
                    };
                    assert!(
                        matches!(call(&mut ctx, method, &input, &args), Err(e) if e.kind == kind),
                        "{method}"
                    );
                    assert_eq!(ctx.stats().peak_memory_bytes, 0);
                    assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
                }
            }
        }
    }

    #[test]
    fn a_walk_charges_each_piece_as_if_charged_where_it_was_mapped() {
        for text in ops::testing::mixed_texts() {
            let ascii = std::str::from_utf8(&text).is_err();
            // A step per 64 bytes of each ASCII run, which is mapped at most
            // 4096 bytes at a time, one for every other character, and three
            // that `emit` asks for each piece.
            let mut steps = 0;
            let mut rest = &text[..];
            while !rest.is_empty() {
                let run = rest
                    .iter()
                    .take(4096)
                    .take_while(|byte| ascii || byte.is_ascii())
                    .count();
                let length = if run > 0 {
                    steps += run.div_ceil(64) as u64;
                    run
                } else {
                    steps += 1;
                    scan::rune(rest).1
                };
                steps += 3;
                rest = &rest[length..];
            }
            for short in [0, 1, 2, 17, steps / 2, steps] {
                if short > steps {
                    continue;
                }
                let mut ctx = ops::testing::context(Some(1 + steps - short), None);
                ctx.charge(1).unwrap();
                let mut mapped = Vec::new();
                let walked = walk(&mut ctx, &text, Transform::Swap, ascii, |_, piece| {
                    mapped.extend_from_slice(piece);
                    Ok(3)
                });
                if short == 0 {
                    walked.unwrap();
                    assert_eq!(ctx.stats().steps, 1 + steps);
                    assert_eq!(mapped.len(), text.len());
                } else {
                    let error = walked.unwrap_err();
                    assert_eq!(error.kind, ErrorKind::Steps);
                    assert_eq!(ctx.stats().steps, 2 + steps - short);
                    assert_eq!(ctx.charge(0).unwrap_err(), error);
                }
            }
        }
    }

    #[test]
    fn a_walk_notices_a_limit_cancellation_and_a_deadline_within_two_chunks() {
        let long = "aé".repeat(100 * CHUNK);
        // Each stops the walk once `after` bytes are mapped: at the start of
        // a text of any length, or part of the way through a long one.
        for (text, after) in [
            ("a", 0),
            ("é", 0),
            (long.as_str(), 0),
            (long.as_str(), 40 * CHUNK),
        ] {
            for kind in [ErrorKind::Steps, ErrorKind::Cancelled, ErrorKind::Deadline] {
                if kind == ErrorKind::Steps && text.len() < CHUNK {
                    continue;
                }
                // The long text costs two steps for every three bytes.
                let limit = 10 + after as u64 * 2 / 3;
                let mut ctx =
                    ops::testing::context((kind == ErrorKind::Steps).then_some(limit), None);
                ctx.charge(1).unwrap();
                let mut mapped = 0;
                let error = walk(
                    &mut ctx,
                    text.as_bytes(),
                    Transform::Upper,
                    false,
                    |ctx, piece| {
                        if mapped >= after {
                            match kind {
                                ErrorKind::Cancelled => ctx.cancellation().cancel(),
                                ErrorKind::Deadline => {
                                    ctx.options.deadline = Some(std::time::Instant::now());
                                }
                                _ => {}
                            }
                        }
                        mapped += piece.len();
                        Ok(0)
                    },
                )
                .unwrap_err();
                assert_eq!(error.kind, kind);
                assert!(mapped < after + 2 * CHUNK, "{mapped}");
            }
        }
    }

    #[test]
    fn a_changed_result_costs_a_second_walk_and_a_step_per_64_bytes_of_each_piece_copied() {
        // What changing the text adds to leaving it as it is.
        let extra = |text: &str| {
            let cost = |text: &str| {
                let mut ctx = CallContext::new(CallOptions::default());
                let input = ctx.import(&Value::bytes(text)).unwrap();
                let before = ctx.stats().steps;
                call(&mut ctx, "upcase", &input, &[]).unwrap();
                ctx.stats().steps - before
            };
            cost(text) - cost(&text.to_uppercase())
        };
        // An ASCII run is scanned again and copied, 64 bytes a step for each.
        assert_eq!(extra(&"a".repeat(6400)) - extra(&"a".repeat(64)), 2 * 99);
        // So is each short run and each other character, a step for each.
        assert_eq!(extra(&"aé".repeat(1000)) - extra("aé"), 4 * 999);
    }
}
