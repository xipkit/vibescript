use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, bytecode::Method, hash::Hash,
    ops, sequence, value::Kind,
};

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}
fn size(ctx: &mut CallContext, value: i128) -> Result<usize> {
    match usize::try_from(value) {
        Ok(value) => Ok(value),
        Err(_) => ctx.fail(ErrorKind::Memory, "collection size overflow"),
    }
}

pub(crate) fn call(
    ctx: &mut CallContext,
    method: Method,
    name: &str,
    receiver: Value,
    args: &[Value],
) -> Result<(Value, Value)> {
    use Method::*;
    if matches!(&receiver.0, Kind::Hash(hash) if hash.tag.protected()) {
        let Kind::Hash(hash) = &receiver.0 else {
            unreachable!()
        };
        return Err(hash.tag.mutation_error(name));
    }
    if matches!(receiver.0, Kind::Bytes(_)) {
        let result = string(ctx, method, &receiver, args)?;
        return Ok((receiver, result));
    }
    match (&receiver.0, method) {
        (Kind::Array(_), Push) => {
            let receiver = receiver.push(ctx, args)?;
            Ok((receiver.clone(), receiver))
        }
        (Kind::Array(_), Pop | Shift) => remove_end(ctx, receiver, args, matches!(method, Shift)),
        (Kind::Array(array), Prepend | Insert) => {
            let (at, values) = if matches!(method, Prepend) {
                (0, args)
            } else {
                let Some(index) = args.first() else {
                    return Err(argument("insert expects an index"));
                };
                let index = i128::from(sequence::integer(index)?);
                if args.len() == 1 {
                    return Ok((receiver.clone(), receiver));
                }
                let at = if index < 0 {
                    index + array.buffer.data.len() as i128 + 1
                } else {
                    index
                };
                if at < 0 {
                    return Err(argument("insert index out of bounds"));
                }
                (size(ctx, at)?, &args[1..])
            };
            if values.is_empty() {
                return Ok((receiver.clone(), receiver));
            }
            let source = &array.buffer.data;
            let length = size(ctx, at.max(source.len()) as i128 + values.len() as i128)?;
            let mut out = Buffer::with_capacity(ctx, length)?;
            out.extend(ctx, &source[..at.min(source.len())])?;
            while out.data.len() < at {
                ctx.charge(1)?;
                out.data.push(Value::nil());
            }
            out.extend(ctx, values)?;
            out.extend(ctx, &source[at.min(source.len())..])?;
            let result = Value::from_array(ctx, out)?;
            Ok((result.clone(), result))
        }
        (Kind::Array(_), Clear) => {
            ops::arity(args, 0)?;
            let value = Value::from_array(ctx, Buffer::empty())?;
            Ok((value.clone(), value))
        }
        (Kind::Hash(original), Clear) => {
            ops::arity(args, 0)?;
            let mut hash = Hash::empty();
            hash.object = original.object;
            let value = Value::from_hash(ctx, hash)?;
            Ok((value.clone(), value))
        }
        (Kind::Array(array), Delete) => {
            ops::arity(args, 1)?;
            let mut out = Buffer::empty();
            let mut removed = None;
            for (i, item) in array.buffer.data.iter().enumerate() {
                if ops::equal(ctx, item, &args[0], 0)? {
                    if removed.is_none() {
                        out.extend(ctx, &array.buffer.data[..i])?;
                    }
                    removed = Some(item.clone());
                } else if removed.is_some() {
                    out.push(ctx, item.clone())?;
                }
            }
            if let Some(removed) = removed {
                Ok((Value::from_array(ctx, out)?, removed))
            } else {
                Ok((receiver, Value::nil()))
            }
        }
        (Kind::Hash(_), Delete) => {
            ops::arity(args, 1)?;
            args[0].hash_key_for("hash.delete key is an")?;
            receiver.delete_hash(ctx, &args[0])
        }
        (Kind::Hash(_), Store) => {
            ops::arity(args, 2)?;
            args[0].hash_key_for("hash.store key is an")?;
            let value = ops::set_index(ctx, receiver, args[0].clone(), args[1].clone())?;
            Ok((value, args[1].clone()))
        }
        (Kind::Hash(original), Replace) => {
            ops::arity(args, 1)?;
            if !matches!(args[0].0, Kind::Hash(_)) {
                return Err(argument("hash replacement must be a hash"));
            }
            let mut value = args[0].clone();
            if let Kind::Hash(hash) = &mut value.0 {
                if hash.object != original.object {
                    Hash::make_mut(ctx, hash)?.object = original.object;
                }
            }
            Ok((value.clone(), value))
        }
        (Kind::Array(array), Fill) => {
            let Some(value) = args.first() else {
                return Err(argument("fill expects a value"));
            };
            let source = &array.buffer.data;
            let (start, end, length) = fill_span(ctx, &args[1..], source.len())?;
            if start == end && length == source.len() {
                return Ok((receiver.clone(), receiver));
            }
            let mut out = Buffer::empty();
            for i in 0..length {
                ctx.charge(1)?;
                let next = if i >= start && i < end {
                    value.clone()
                } else {
                    source.get(i).cloned().unwrap_or_default()
                };
                out.push(ctx, next)?;
            }
            let value = Value::from_array(ctx, out)?;
            Ok((value.clone(), value))
        }
        _ => Err(Error::new(ErrorKind::Type, "unsupported mutating method")),
    }
}

fn remove_end(
    ctx: &mut CallContext,
    receiver: Value,
    args: &[Value],
    front: bool,
) -> Result<(Value, Value)> {
    if args.len() > 1 {
        return Err(argument("pop/shift accepts at most one count"));
    }
    let source = receiver.as_array().unwrap();
    let count = if let Some(count) = args.first() {
        let n = sequence::integer(count)?;
        if n < 0 {
            return Err(argument("pop/shift count must be non-negative"));
        }
        usize::try_from(n).unwrap_or(usize::MAX).min(source.len())
    } else {
        1.min(source.len())
    };
    let (start, end) = if front {
        (count, source.len())
    } else {
        (0, source.len() - count)
    };
    let removed = if args.is_empty() {
        (if front { source.first() } else { source.last() })
            .cloned()
            .unwrap_or_default()
    } else {
        ctx.array(if front {
            &source[..count]
        } else {
            &source[end..]
        })?
    };
    Ok((receiver.keep_array_range(ctx, start, end)?, removed))
}

pub(crate) fn fill_span(
    ctx: &mut CallContext,
    args: &[Value],
    length: usize,
) -> Result<(usize, usize, usize)> {
    if args.len() > 2 {
        return Err(argument("fill accepts at most a start and length"));
    }
    let length = length as i128;
    if let Some(Value(Kind::Range(range))) = args.first() {
        ops::arity(args, 1)?;
        let start = range.start.map_or(0, i128::from);
        let start = if start < 0 { start + length } else { start };
        if start < 0 {
            return Err(argument("fill range out of bounds"));
        }
        let end = range.end.map_or(length - 1, i128::from);
        let end = if end < 0 { end + length } else { end };
        let end = (end + i128::from(range.end.is_none() || !range.exclusive)).max(start);
        if end > isize::MAX as i128 {
            return ctx.guard(ErrorKind::Arithmetic, "array.fill window is too large");
        }
        return Ok((
            size(ctx, start)?,
            size(ctx, end)?,
            size(ctx, end.max(length))?,
        ));
    }
    let start = match args.first() {
        None | Some(Value(Kind::Nil)) => 0,
        Some(value) => i128::from(sequence::integer(value)?),
    };
    let start = if start < 0 {
        (start + length).max(0)
    } else {
        start
    };
    let count = match args.get(1) {
        None | Some(Value(Kind::Nil)) => length - start,
        Some(value) => i128::from(sequence::integer(value)?),
    };
    if count < 0 {
        return Ok((0, 0, size(ctx, length)?));
    }
    let end = start + count;
    if end > isize::MAX as i128 {
        return ctx.guard(ErrorKind::Arithmetic, "array.fill window is too large");
    }
    Ok((
        size(ctx, start)?,
        size(ctx, end)?,
        size(ctx, end.max(length))?,
    ))
}

fn string(
    ctx: &mut CallContext,
    method: Method,
    receiver: &Value,
    args: &[Value],
) -> Result<Value> {
    let bytes = receiver.require_bytes()?;
    let strict = |v: &Value, message: &str| -> Result<()> {
        if !matches!(v.0, Kind::Bytes(_)) {
            return Err(Error::new(ErrorKind::Type, message));
        }
        Ok(())
    };
    match method {
        Method::Clear => {
            ops::arity(args, 0)?;
            ctx.bytes(b"")
        }
        Method::Replace => {
            if args.len() != 1 {
                return Err(argument("string.replace expects exactly one replacement"));
            }
            strict(&args[0], "string.replace replacement must be string")?;
            Ok(args[0].clone())
        }
        Method::Prepend => {
            let mut length = bytes.len();
            for arg in args {
                ctx.charge(1)?;
                strict(arg, "string.prepend expects string arguments")?;
                length = size(ctx, length as i128 + arg.require_bytes()?.len() as i128)?;
            }
            let mut out = Buffer::with_capacity(ctx, length)?;
            for arg in args {
                out.extend(ctx, arg.require_bytes()?)?;
            }
            out.extend(ctx, bytes)?;
            Value::from_bytes(ctx, out)
        }
        Method::Insert => {
            if args.len() != 2 {
                return Err(argument("string.insert expects an index and a string"));
            }
            let requested = sequence::integer(&args[0]).map_err(|mut error| {
                error.message = "string.insert index must be integer".into();
                error
            })?;
            strict(&args[1], "string.insert value must be string")?;
            let length = ops::runes(ctx, bytes)?.0 as i128;
            let index = i128::from(requested);
            let index = if index < 0 { length + index + 1 } else { index };
            if index < 0 || index > length {
                return Err(argument(&format!(
                    "string.insert index {requested} out of string"
                )));
            }
            let offset = sequence::rune_offset(ctx, bytes, index as usize)?;
            let inserted = args[1].require_bytes()?;
            let capacity = size(ctx, bytes.len() as i128 + inserted.len() as i128)?;
            let mut out = Buffer::with_capacity(ctx, capacity)?;
            out.extend(ctx, &bytes[..offset])?;
            out.extend(ctx, inserted)?;
            out.extend(ctx, &bytes[offset..])?;
            Value::from_bytes(ctx, out)
        }
        _ => Err(Error::new(
            ErrorKind::Name,
            "string method is not implemented",
        )),
    }
}
