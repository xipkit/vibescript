use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, scan, value::Kind};
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
                    bytes[0] = bytes[0].to_ascii_uppercase();
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

fn entry(rune: char) -> Option<(&'static [u32; 5], u32)> {
    let point = rune as u32;
    let index = data::MAPPINGS.partition_point(|&(r, _, _)| r < point);
    data::MAPPINGS
        .get(index)
        .filter(|&&(r, _, _)| r == point)
        .map(|(_, maps, fold)| (maps, *fold))
}

fn walk(
    ctx: &mut CallContext,
    bytes: &[u8],
    transform: Transform,
    ascii: bool,
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
) -> Result<bool> {
    let mut changed = false;
    let mut position = 0;
    let mut scratch = [0; 4096];
    while position < bytes.len() {
        let end = position + (bytes.len() - position).min(scratch.len());
        let chunk = &bytes[position..end];
        let length = if ascii {
            chunk.len()
        } else {
            scan::prefix(chunk, scan::Class::Ascii)
        };
        if length > 0 {
            ctx.work_bytes(length)?;
            let original = &chunk[..length];
            let mapped = &mut scratch[..length];
            mapped.copy_from_slice(original);
            transform.ascii(mapped, position == 0);
            changed |= mapped != original;
            emit(ctx, mapped)?;
            position += length;
            continue;
        }
        ctx.charge(1)?;
        let (rune, length, _) = scan::rune(&bytes[position..]);
        let original = &bytes[position..position + length];
        let encoded = entry(rune).map_or(0, |(maps, _)| maps[transform.index(position == 0)]);
        let mapped = if encoded == 0 {
            original
        } else {
            let start = (encoded >> 4) as usize;
            &data::BYTES[start..start + (encoded & 15) as usize]
        };
        changed |= mapped != original;
        emit(ctx, mapped)?;
        position += length;
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
        Ok(())
    })?;
    if !changed {
        return Ok(if bang { Value::nil() } else { receiver.clone() });
    }
    let mut output = Buffer::with_capacity(ctx, size)?;
    walk(ctx, bytes, transform, ascii, |ctx, mapped| {
        ctx.work_bytes(mapped.len())?;
        output.data.extend_from_slice(mapped);
        Ok(())
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

fn equal(ctx: &mut CallContext, mut a: &[u8], mut b: &[u8]) -> Result<bool> {
    if !ops::runes(ctx, a)?.1 || !ops::runes(ctx, b)?.1 {
        return Ok(ascii_compare(ctx, a, b)? == Ordering::Equal);
    }
    while !a.is_empty() && !b.is_empty() {
        ctx.charge(1)?;
        let (left, n, _) = scan::rune(a);
        let (right, m, _) = scan::rune(b);
        let canonical = |r| entry(r).map_or(r as u32, |(_, fold)| fold);
        if canonical(left) != canonical(right) {
            return Ok(false);
        }
        a = &a[n..];
        b = &b[m..];
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
    let bytes = &input.data;
    if matches!(name, "casecmp" | "casecmp?") {
        ctx.charge(1)?;
        ops::arity(args, 1)?;
        let Kind::Bytes(other) = &args[0].0 else {
            return Ok(Some(Value::nil()));
        };
        return Ok(Some(if name == "casecmp?" {
            Value::boolean(equal(ctx, bytes, &other.data)?)
        } else {
            Value::int(match ascii_compare(ctx, bytes, &other.data)? {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            })
        }));
    }
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
            _ => {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "unsupported case-mapping option",
                ));
            }
        },
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                "case conversion accepts one optional symbol",
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
}
