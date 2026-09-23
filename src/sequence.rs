use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, bytecode::Method, ops, scan,
    value::Kind,
};

pub(crate) fn integer(value: &Value) -> Result<i64> {
    match value.0 {
        Kind::Int(n) => Ok(n),
        Kind::Float(n) if n.is_finite() && n >= i64::MIN as f64 && n < 9223372036854775808.0 => {
            Ok(n as i64)
        }
        _ => Err(Error::new(
            ErrorKind::Type,
            "expected an index within the 64-bit integer range",
        )),
    }
}

fn bound(n: i64, length: usize) -> i128 {
    if n < 0 {
        length as i128 + i128::from(n)
    } else {
        i128::from(n)
    }
}

/// The messages a slice method reports for malformed selectors.
pub(crate) struct Selectors {
    /// Too few or too many selectors.
    arity: &'static str,
    /// A lone selector that is not an index or range.
    index: &'static str,
    /// A start, or a range given a length, that is not an index.
    start: &'static str,
    /// A length that is not an index.
    length: &'static str,
}

/// Selector messages for `array.slice`.
const ARRAY_SLICE: Selectors = Selectors {
    arity: "array.slice expects an index, a start and length, or a range",
    index: "array.slice index must be integer",
    start: "array.slice index must be integer",
    length: "array.slice length must be integer",
};

/// Selector messages for `string.slice`.
const STRING_SLICE: Selectors = Selectors {
    arity: "string.slice expects an index, range, or substring with optional length",
    index: "string.slice index must be an integer, range, or substring",
    start: "string.slice index must be integer",
    length: "string.slice length must be integer",
};

/// Selector messages for `string.byteslice`.
const STRING_BYTESLICE: Selectors = Selectors {
    arity: "string.byteslice expects an index, a range, or a start and length",
    index: "string.byteslice index must be an integer or range",
    start: "string.byteslice start must be an integer",
    length: "string.byteslice length must be an integer",
};

/// Resolves slice selectors to a window. `member` holds the slice method's
/// own messages for malformed selectors; the index operator, which validates
/// its selectors first, passes `None`.
fn window(
    args: &[Value],
    length: usize,
    member: Option<&Selectors>,
) -> Result<Option<(usize, usize, bool)>> {
    let relabel = |error: Error, message: fn(&Selectors) -> &'static str| match member {
        Some(member) => error.with_message(message(member).to_owned()),
        None => error,
    };
    if args.is_empty() || args.len() > 2 {
        let error = Error::new(
            ErrorKind::Argument,
            "slice expects an index, a start and length, or a range",
        );
        return Err(relabel(error, |member| member.arity));
    }
    let len = length as i128;
    let (start, end, single) = if let Kind::Range(range) = &args[0].0 {
        ops::arity(args, 1).map_err(|error| relabel(error, |member| member.start))?;
        let start = range.start.map_or(0, |n| bound(n, length));
        let end = range
            .end
            .map_or(len, |n| bound(n, length) + i128::from(!range.exclusive));
        (start, end, false)
    } else {
        let single = args.len() == 1;
        let first: fn(&Selectors) -> &'static str = if single {
            |member| member.index
        } else {
            |member| member.start
        };
        let start = integer(&args[0]).map_err(|error| relabel(error, first))?;
        let start = bound(start, length);
        let count = if single {
            1
        } else {
            integer(&args[1]).map_err(|error| relabel(error, |member| member.length))?
        };
        if count < 0 {
            return Ok(None);
        }
        (start, start + i128::from(count), single)
    };
    if start < 0 || start > len || (single && start == len) {
        return Ok(None);
    }
    Ok(Some((
        start as usize,
        end.clamp(start, len) as usize,
        single,
    )))
}

pub(crate) fn rune_offset(ctx: &mut CallContext, bytes: &[u8], count: usize) -> Result<usize> {
    let mut offset = 0;
    for _ in 0..count {
        ctx.charge(1)?;
        if offset == bytes.len() {
            break;
        }
        offset += scan::rune(&bytes[offset..]).1;
    }
    Ok(offset)
}

/// Reads a slice of an array or string. `member` holds the slice method's
/// messages for malformed selectors, or is `None` for the index operator.
pub(crate) fn slice(
    ctx: &mut CallContext,
    value: &Value,
    args: &[Value],
    byte_slice: bool,
    member: Option<&Selectors>,
) -> Result<Value> {
    if !byte_slice {
        if let Some(array) = value.as_array() {
            let Some((start, end, single)) = window(args, array.len(), member)? else {
                return Ok(Value::nil());
            };
            if single {
                return Ok(array[start].clone());
            }
            return ctx.array(&array[start..end]);
        }
    }
    let Kind::Bytes(bytes) = &value.0 else {
        return Err(Error::new(
            ErrorKind::Type,
            "slice requires an array or string",
        ));
    };
    let bytes = &bytes.data;
    if !byte_slice && args.len() == 1 && matches!(args[0].0, Kind::Bytes(_)) {
        return Ok(
            if ops::find(ctx, bytes, args[0].require_bytes()?, false)?.is_some() {
                args[0].clone()
            } else {
                Value::nil()
            },
        );
    }
    let length = if byte_slice {
        bytes.len()
    } else {
        ops::runes(ctx, bytes)?.0
    };
    let Some((start, end, _)) = window(args, length, member)? else {
        return Ok(Value::nil());
    };
    let (start, end) = if byte_slice {
        (start, end)
    } else {
        let from = rune_offset(ctx, bytes, start)?;
        let to = from + rune_offset(ctx, &bytes[from..], end - start)?;
        (from, to)
    };
    let selected = &bytes[start..end];
    if byte_slice || ops::runes(ctx, selected)?.1 {
        return ctx.bytes(selected);
    }
    let mut out = Buffer::empty();
    let mut offset = 0;
    while offset < selected.len() {
        ctx.charge(1)?;
        let (ch, n, _) = scan::rune(&selected[offset..]);
        let mut encoded = [0; 4];
        out.extend(ctx, ch.encode_utf8(&mut encoded).as_bytes())?;
        offset += n;
    }
    Value::from_bytes(ctx, out)
}

pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    value: Value,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    match method {
        Slice | ByteSlice => {
            let member = match (&value.0, method) {
                (Kind::Array(_), _) => Some(&ARRAY_SLICE),
                (Kind::Bytes(_), ByteSlice) => Some(&STRING_BYTESLICE),
                (Kind::Bytes(_), _) => Some(&STRING_SLICE),
                _ => None,
            };
            slice(ctx, &value, args, matches!(method, ByteSlice), member)
        }
        At => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "array.at expects exactly one index",
                ));
            }
            if value.as_array().is_none() {
                return Err(Error::new(ErrorKind::Type, "at requires an array"));
            }
            let index = integer(&args[0])
                .map_err(|error| error.with_message("array.at index must be integer".to_owned()))?;
            ops::index(ctx, &value, &Value::int(index))
        }
        GetByte => {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "string.getbyte expects exactly one index",
                ));
            }
            let bytes = value.require_bytes()?;
            let index = integer(&args[0]).map_err(|error| {
                error.with_message("string.getbyte index must be an integer".to_owned())
            })?;
            let n = bound(index, bytes.len());
            Ok(usize::try_from(n)
                .ok()
                .and_then(|n| bytes.get(n))
                .map_or_else(Value::nil, |&b| Value::int(i64::from(b))))
        }
        First | Last => {
            let array = value
                .as_array()
                .ok_or_else(|| Error::new(ErrorKind::Type, "expected array"))?;
            let last = matches!(method, Last);
            if args.is_empty() {
                return Ok((if last { array.last() } else { array.first() })
                    .cloned()
                    .unwrap_or_default());
            }
            let member = if last { "array.last" } else { "array.first" };
            if args.len() > 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("{member} accepts at most one count"),
                ));
            }
            let invalid = || format!("{member} expects non-negative integer");
            let n = integer(&args[0]).map_err(|error| error.with_message(invalid()))?;
            if n < 0 {
                return Err(Error::new(ErrorKind::Argument, invalid()));
            }
            let n = usize::try_from(n).unwrap_or(usize::MAX).min(array.len());
            ctx.array(if last {
                &array[array.len() - n..]
            } else {
                &array[..n]
            })
        }
        ToArray => {
            if !args.is_empty() && matches!(value.0, Kind::Hash(_)) {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "hash.to_a does not take arguments",
                ));
            }
            ops::arity(args, 0)?;
            match &value.0 {
                Kind::Hash(h) => {
                    let mut out = Buffer::with_capacity(ctx, h.buffer.data.len())?;
                    for (key, value) in &h.buffer.data {
                        ctx.charge(1)?;
                        let pair = ctx.array(&[key.clone(), value.clone()])?;
                        out.data.push(pair);
                    }
                    Value::from_array(ctx, out)
                }
                _ => Err(Error::new(ErrorKind::Type, "unsupported to_a receiver")),
            }
        }
        _ => Err(Error::new(ErrorKind::Type, "unsupported sequence method")),
    }
}
