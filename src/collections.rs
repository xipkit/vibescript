use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    bytecode::Method,
    hash::Hash,
    ops,
    sequence::integer,
    value::Kind,
};

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}
fn wrong_type() -> Error {
    Error::new(
        ErrorKind::Type,
        "unsupported collection receiver or argument",
    )
}

pub(crate) fn lookup(
    ctx: &mut CallContext,
    value: &Value,
    key: &Value,
    strict: bool,
) -> Result<Option<Value>> {
    ctx.charge(1)?;
    match &value.0 {
        Kind::Array(h) => {
            let n = integer(key)?;
            if strict && matches!(key.0, Kind::Float(f) if f.trunc() != f) {
                return Err(argument("array.fetch index must be integer"));
            }
            let n = if n < 0 {
                h.buffer.data.len() as i128 + i128::from(n)
            } else {
                i128::from(n)
            };
            Ok(usize::try_from(n)
                .ok()
                .and_then(|n| h.buffer.data.get(n))
                .cloned())
        }
        Kind::Hash(h) => Ok(h
            .find(ctx, key.require_bytes()?)?
            .map(|i| h.buffer.data[i].1.clone())),
        _ => Err(wrong_type()),
    }
}

pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    value: Value,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    match method {
        ValuesAt => {
            if !matches!(value.0, Kind::Array(_) | Kind::Hash(_)) {
                return Err(wrong_type());
            }
            let mut out = Buffer::empty();
            for selector in args {
                ctx.charge(1)?;
                if let (Kind::Array(array), Kind::Range(range)) = (&value.0, &selector.0) {
                    values_at_range(ctx, &array.buffer.data, range, &mut out)?;
                } else {
                    let selected = lookup(ctx, &value, selector, false)?.unwrap_or_default();
                    out.push(ctx, selected)?;
                }
            }
            Value::from_array(ctx, out)
        }
        Fetch => {
            if args.is_empty() || args.len() > 2 {
                return Err(argument("fetch expects a key and optional default"));
            }
            if let Some(found) = lookup(ctx, &value, &args[0], true)? {
                return Ok(found);
            }
            args.get(1)
                .cloned()
                .ok_or_else(|| argument("key or index not found"))
        }
        Dig => {
            if args.is_empty() {
                return Err(argument("dig expects at least one key"));
            }
            let receiver = match value.0 {
                Kind::Array(_) => "array",
                Kind::Hash(_) => "hash",
                _ => return Err(wrong_type()),
            };
            let mut current = value;
            for key in args {
                ctx.charge(1)?;
                if !matches!(current.0, Kind::Array(_) | Kind::Hash(_)) {
                    return Ok(Value::nil());
                }
                if matches!(current.0, Kind::Array(_)) {
                    // Whole floats select an element; fractional indexes are rejected.
                    let whole = !matches!(key.0, Kind::Float(f) if f.trunc() != f);
                    match integer(key) {
                        Ok(index) if whole => {
                            if index < 0 {
                                return Ok(Value::nil());
                            }
                        }
                        _ => {
                            return Err(Error::new(
                                ErrorKind::Type,
                                format!("{receiver}.dig array index must be integer"),
                            ));
                        }
                    }
                }
                current = lookup(ctx, &current, key, false)?.unwrap_or_default();
            }
            Ok(current)
        }
        Key | Member => {
            ops::arity(args, 1)?;
            let Kind::Hash(h) = &value.0 else {
                return Err(wrong_type());
            };
            Ok(Value::boolean(
                h.find(ctx, args[0].require_bytes()?)?.is_some(),
            ))
        }
        HasValue => {
            ops::arity(args, 1)?;
            let entries = value.as_hash().ok_or_else(wrong_type)?;
            for (_, v) in entries {
                if ops::equal(ctx, v, &args[0], 0)? {
                    return Ok(Value::boolean(true));
                }
            }
            Ok(Value::boolean(false))
        }
        Except => {
            let Kind::Hash(hash) = &value.0 else {
                return Err(wrong_type());
            };
            let mut excluded = Hash::empty();
            for key in args {
                ctx.charge(1)?;
                if hash.find(ctx, key.require_bytes()?)?.is_some() {
                    excluded.insert(ctx, key.clone(), Value::nil())?;
                }
            }
            let mut out = Hash::empty();
            for (key, value) in &hash.buffer.data {
                ctx.charge(1)?;
                if excluded.find(ctx, key.require_bytes()?)?.is_none() {
                    out.insert(ctx, key.clone(), value.clone())?;
                }
            }
            Value::from_hash(ctx, out)
        }
        Flatten if matches!(value.0, Kind::Hash(_)) => {
            if args.len() > 1 {
                return Err(argument("hash.flatten accepts at most a depth"));
            }
            let depth = args.first().map(integer).transpose()?.unwrap_or(1);
            let mut out = Buffer::empty();
            for (key, value) in value.as_hash().unwrap() {
                ctx.charge(1)?;
                if depth == 0 {
                    let pair = ctx.array(&[key.clone(), value.clone()])?;
                    if pair.depth() + 1 > MAX_VALUE_DEPTH {
                        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
                    }
                    out.push(ctx, pair)?;
                } else {
                    out.push(ctx, key.clone())?;
                    let remaining = if depth < 0 { -1 } else { depth - 1 };
                    flatten(ctx, std::slice::from_ref(value), remaining, 0, &mut out)?;
                }
            }
            Value::from_array(ctx, out)
        }
        RemapKeys => {
            ops::arity(args, 1)?;
            let entries = value.as_hash().ok_or_else(wrong_type)?;
            let Kind::Hash(mapping) = &args[0].0 else {
                return Err(wrong_type());
            };
            let mut out = Hash::empty();
            for (key, value) in entries {
                ctx.charge(1)?;
                let key = if let Some(index) = mapping.find(ctx, key.require_bytes()?)? {
                    ctx.bytes(mapping.buffer.data[index].1.require_bytes()?)?
                } else {
                    key.clone()
                };
                out.insert(ctx, key, value.clone())?;
            }
            Value::from_hash(ctx, out)
        }
        Compact if matches!(value.0, Kind::Hash(_)) => {
            ops::arity(args, 0)?;
            let mut out = Hash::empty();
            for (key, v) in value.as_hash().unwrap() {
                ctx.charge(1)?;
                if !matches!(v.0, Kind::Nil) {
                    out.insert(ctx, key.clone(), v.clone())?;
                }
            }
            Value::from_hash(ctx, out)
        }
        Slice if matches!(value.0, Kind::Hash(_)) => {
            let mut out = Hash::empty();
            for key in args {
                if let Some(v) = lookup(ctx, &value, key, false)? {
                    let key = ctx.bytes(key.require_bytes()?)?;
                    out.insert(ctx, key, v)?;
                }
            }
            Value::from_hash(ctx, out)
        }
        _ => array_method(ctx, method, value.as_array().ok_or_else(wrong_type)?, args),
    }
}

fn values_at_range(
    ctx: &mut CallContext,
    array: &[Value],
    range: &crate::range::Range,
    out: &mut Buffer<Value>,
) -> Result<()> {
    let length = array.len() as i128;
    let mut start = i128::from(range.start.unwrap_or(0));
    if start < 0 {
        start += length;
        if start < 0 {
            return Err(argument("array.values_at range starts out of bounds"));
        }
    }
    let mut end = range.end.map(i128::from).unwrap_or(length - 1);
    if end < 0 {
        end += length;
    }
    let count = (end - start + i128::from(range.end.is_none() || !range.exclusive)).max(0);
    if count > isize::MAX as i128 {
        return ctx.guard(
            ErrorKind::OutputLimit,
            "array.values_at window is too large",
        );
    }
    let count = count as usize;
    if count == 0 {
        return Ok(());
    }
    let Some((length, bytes)) = out.data.len().checked_add(count).and_then(|length| {
        length
            .checked_mul(size_of::<Value>())
            .map(|bytes| (length, bytes))
    }) else {
        return ctx.fail(ErrorKind::Memory, "array.values_at output size overflow");
    };
    if length > out.data.capacity() {
        ctx.check_memory(bytes)?;
    }
    for offset in 0..count {
        ctx.charge(1)?;
        let selected = usize::try_from(start + offset as i128)
            .ok()
            .and_then(|index| array.get(index))
            .cloned()
            .unwrap_or_default();
        out.push(ctx, selected)?;
    }
    Ok(())
}

fn array_method(
    ctx: &mut CallContext,
    method: Method,
    array: &[Value],
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    match method {
        Reverse => {
            ops::arity(args, 0)?;
            let mut out = Buffer::with_capacity(ctx, array.len())?;
            for v in array.iter().rev() {
                ctx.charge(1)?;
                out.data.push(v.clone());
            }
            Value::from_array(ctx, out)
        }
        Take | Drop => {
            ops::arity(args, 1)?;
            let n = integer(&args[0])?;
            if n < 0 {
                return Err(argument("count must be non-negative"));
            }
            let n = usize::try_from(n).unwrap_or(usize::MAX).min(array.len());
            ctx.array(if matches!(method, Take) {
                &array[..n]
            } else {
                &array[n..]
            })
        }
        Compact | Uniq => {
            ops::arity(args, 0)?;
            let mut out = Buffer::empty();
            for v in array {
                ctx.charge(1)?;
                if matches!(method, Compact) {
                    if !matches!(v.0, Kind::Nil) {
                        out.push(ctx, v.clone())?;
                    }
                } else if !crate::sets::contains(ctx, &out.data, v)? {
                    out.push(ctx, v.clone())?;
                }
            }
            Value::from_array(ctx, out)
        }
        Flatten => {
            if args.len() > 1 {
                return Err(argument("flatten accepts at most a depth"));
            }
            let depth = if let Some(v) = args.first().filter(|v| !matches!(v.0, Kind::Nil)) {
                integer(v)?
            } else {
                -1
            };
            let mut out = Buffer::empty();
            flatten(ctx, array, depth, 0, &mut out)?;
            Value::from_array(ctx, out)
        }
        Chunk | Window => {
            ops::arity(args, 1)?;
            let n = args[0].require_int()?;
            if n <= 0 {
                return Err(argument("window or chunk size must be positive"));
            }
            let n = usize::try_from(n).unwrap_or(usize::MAX);
            let mut out = Buffer::empty();
            let mut start = 0;
            while start < array.len() {
                ctx.charge(1)?;
                if matches!(method, Window) && n > array.len() - start {
                    break;
                }
                let end = start + n.min(array.len() - start);
                let part = ctx.array(&array[start..end])?;
                out.push(ctx, part)?;
                start = if matches!(method, Window) {
                    start + 1
                } else {
                    end
                };
            }
            Value::from_array(ctx, out)
        }
        Zip => {
            for v in args {
                ctx.charge(1)?;
                v.as_array().ok_or_else(wrong_type)?;
            }
            let mut out = Buffer::empty();
            for (i, v) in array.iter().enumerate() {
                ctx.charge(1)?;
                let mut row = Buffer::with_capacity(ctx, args.len() + 1)?;
                row.data.push(v.clone());
                for arg in args {
                    ctx.charge(1)?;
                    row.data
                        .push(arg.as_array().unwrap().get(i).cloned().unwrap_or_default());
                }
                let row = Value::from_array(ctx, row)?;
                out.push(ctx, row)?;
            }
            Value::from_array(ctx, out)
        }
        Transpose => {
            ops::arity(args, 0)?;
            let cols = if let Some(first) = array.first() {
                first.as_array().ok_or_else(wrong_type)?.len()
            } else {
                0
            };
            for row in array {
                ctx.charge(1)?;
                if row.as_array().ok_or_else(wrong_type)?.len() != cols {
                    return Err(argument("transpose rows have different lengths"));
                }
            }
            let mut out = Buffer::empty();
            for i in 0..cols {
                ctx.charge(1)?;
                let mut col = Buffer::with_capacity(ctx, array.len())?;
                for row in array {
                    ctx.charge(1)?;
                    col.data.push(row.as_array().unwrap()[i].clone());
                }
                let col = Value::from_array(ctx, col)?;
                out.push(ctx, col)?;
            }
            Value::from_array(ctx, out)
        }
        ToHash => {
            ops::arity(args, 0)?;
            let mut out = Hash::empty();
            for v in array {
                ctx.charge(1)?;
                let pair = v.as_array().ok_or_else(wrong_type)?;
                if pair.len() != 2 {
                    return Err(argument("to_h requires two-element pairs"));
                }
                let key = ctx.bytes(pair[0].require_bytes()?)?;
                out.insert(ctx, key, pair[1].clone())?;
            }
            Value::from_hash(ctx, out)
        }
        _ => Err(wrong_type()),
    }
}

/// A suspended array level of a flatten walk; borrows the input only.
struct Level<'a> {
    values: &'a [Value],
    remaining: i64,
    index: usize,
}

fn flatten<'a>(
    ctx: &mut CallContext,
    values: &'a [Value],
    remaining: i64,
    depth: usize,
    out: &mut Buffer<Value>,
) -> Result<()> {
    if depth > MAX_VALUE_DEPTH {
        return ctx.guard(ErrorKind::Recursion, "flatten nesting too deep");
    }
    let mut current = Level {
        values,
        remaining,
        index: 0,
    };
    let mut parents: Buffer<Level<'a>> = Buffer::empty();
    loop {
        let i = current.index;
        if i == current.values.len() {
            match parents.data.pop() {
                Some(parent) => current = parent,
                None => return Ok(()),
            }
            continue;
        }
        current.index += 1;
        ctx.charge(1)?;
        let values: &'a [Value] = current.values;
        let v = &values[i];
        if current.remaining != 0 {
            if let Some(nested) = v.as_array() {
                // A nested level sits one below its parent; the root is `depth`.
                if depth + parents.data.len() + 1 > MAX_VALUE_DEPTH {
                    return ctx.guard(ErrorKind::Recursion, "flatten nesting too deep");
                }
                let remaining = if current.remaining < 0 {
                    -1
                } else {
                    current.remaining - 1
                };
                let suspended = std::mem::replace(
                    &mut current,
                    Level {
                        values: nested,
                        remaining,
                        index: 0,
                    },
                );
                parents.push(ctx, suspended)?;
                continue;
            }
        }
        out.push(ctx, v.clone())?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallOptions, ErrorClass,
        ops::testing::{context, nested, on_small_stack},
    };

    fn ints(values: &[i64]) -> Value {
        Value::array(values.iter().copied().map(Value::int).collect())
    }

    fn flattened(ctx: &mut CallContext, value: &Value, args: &[Value]) -> Result<String> {
        method(ctx, Method::Flatten, value.clone(), args).map(|value| value.to_string())
    }

    #[test]
    fn flatten_depth_forms_preserve_their_outputs() {
        let mut ctx = CallContext::new(CallOptions::default());
        let array = Value::array(vec![
            Value::int(1),
            Value::array(vec![
                Value::int(2),
                Value::array(vec![Value::int(3), ints(&[4])]),
            ]),
        ]);
        for (args, expected) in [
            (vec![], "[1, 2, 3, 4]"),
            (vec![Value::nil()], "[1, 2, 3, 4]"),
            (vec![Value::int(-1)], "[1, 2, 3, 4]"),
            (vec![Value::int(0)], "[1, [2, [3, [4]]]]"),
            (vec![Value::int(1)], "[1, 2, [3, [4]]]"),
            (vec![Value::int(2)], "[1, 2, 3, [4]]"),
        ] {
            assert_eq!(flattened(&mut ctx, &array, &args).unwrap(), expected);
        }
        let hash = Value::hash(vec![(
            b"a".to_vec(),
            Value::array(vec![Value::int(1), ints(&[2])]),
        )]);
        for (args, expected) in [
            (vec![], "[a, [1, [2]]]"),
            (vec![Value::int(1)], "[a, [1, [2]]]"),
            (vec![Value::int(2)], "[a, 1, [2]]"),
            (vec![Value::int(3)], "[a, 1, 2]"),
            (vec![Value::int(-1)], "[a, 1, 2]"),
            (vec![Value::int(0)], "[[a, [1, [2]]]]"),
        ] {
            assert_eq!(flattened(&mut ctx, &hash, &args).unwrap(), expected);
        }
        let too_many = [Value::int(1), Value::int(2)];
        assert_eq!(
            flattened(&mut ctx, &array, &too_many).unwrap_err().kind,
            ErrorKind::Argument
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn flatten_has_exact_step_and_frame_memory_boundaries() {
        let array = Value::array(vec![
            Value::int(1),
            Value::array(vec![Value::int(2), ints(&[3])]),
        ]);
        let walk = |ctx: &mut CallContext| {
            let mut out = Buffer::empty();
            flatten(ctx, array.as_array().unwrap(), -1, 0, &mut out).map(|_| out.data.len())
        };
        let mut ctx = CallContext::new(CallOptions::default());
        assert_eq!(walk(&mut ctx).unwrap(), 3);
        let steps = ctx.stats().steps;
        let mut ctx = context(Some(steps), None);
        assert_eq!(walk(&mut ctx).unwrap(), 3);
        let mut ctx = context(Some(steps - 1), None);
        let error = walk(&mut ctx).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);

        let empty_levels = Value::array(vec![Value::array(vec![Value::array(vec![])])]);
        let walk = |ctx: &mut CallContext| {
            let mut out = Buffer::empty();
            flatten(ctx, empty_levels.as_array().unwrap(), -1, 0, &mut out).map(|_| out.data.len())
        };
        let frames = 8 * size_of::<Level>();
        let mut ctx = context(None, Some(frames));
        assert_eq!(walk(&mut ctx).unwrap(), 0);
        assert_eq!(ctx.stats().peak_memory_bytes, frames);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = context(None, Some(frames - 1));
        let error = walk(&mut ctx).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);
        // A depth of zero copies elements without suspending any level.
        let mut ctx = context(None, Some(0));
        let mut out = Buffer::untracked(Vec::with_capacity(4));
        flatten(&mut ctx, empty_levels.as_array().unwrap(), 0, 0, &mut out).unwrap();
        assert_eq!(out.data.len(), 1);
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
    }

    #[test]
    fn deep_flatten_walks_one_level_past_the_value_limit_on_a_small_stack() {
        let outcome = on_small_stack(|| {
            let mut ctx = CallContext::new(CallOptions::default());
            let within = nested(MAX_VALUE_DEPTH + 1, Value::int(1));
            let beyond = nested(MAX_VALUE_DEPTH + 2, Value::int(1));
            let mut out = Buffer::empty();
            let full = flatten(&mut ctx, within.as_array().unwrap(), -1, 0, &mut out)
                .map(|_| out.data.iter().map(|v| v.as_int()).collect::<Vec<_>>());
            drop(out);
            let mut out = Buffer::empty();
            let failed = flatten(&mut ctx, beyond.as_array().unwrap(), -1, 0, &mut out);
            drop(out);
            // Bounded depth never reaches the deep levels, so height is irrelevant.
            let mut out = Buffer::empty();
            let bounded = flatten(&mut ctx, beyond.as_array().unwrap(), 1, 0, &mut out)
                .map(|_| out.data.iter().map(Value::depth).collect::<Vec<_>>());
            for value in out.data.drain(..) {
                drop(value);
            }
            drop(out);
            let hash = Value::hash(vec![(
                b"k".to_vec(),
                nested(MAX_VALUE_DEPTH, Value::int(1)),
            )]);
            let hash_full = method(&mut ctx, Method::Flatten, hash.clone(), &[Value::int(-1)])
                .map(|value| value.to_string());
            let too_deep = Value::hash(vec![(
                b"k".to_vec(),
                nested(MAX_VALUE_DEPTH + 1, Value::int(1)),
            )]);
            let hash_failed = method(
                &mut ctx,
                Method::Flatten,
                too_deep.clone(),
                &[Value::int(-1)],
            )
            .map(|value| value.to_string());
            let stats = ctx.stats();
            for value in [within, beyond, hash, too_deep] {
                drop(value);
            }
            (full, failed, bounded, hash_full, hash_failed, stats)
        });
        let (full, failed, bounded, hash_full, hash_failed, stats) = outcome;
        assert_eq!(full.unwrap(), [Some(1)]);
        for error in [failed.unwrap_err(), hash_failed.unwrap_err()] {
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.class(), Some(ErrorClass::Limit));
            assert_eq!(error.message, "flatten nesting too deep");
        }
        assert_eq!(bounded.unwrap(), [MAX_VALUE_DEPTH]);
        assert_eq!(hash_full.unwrap(), "[k, 1]");
        assert_eq!(stats.retained_memory_bytes, 0);
    }
}
