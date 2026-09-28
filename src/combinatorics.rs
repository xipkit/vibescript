//! Array sampling, rotation and combinatorial materializers.
//!
//! `sample`, `shuffle`, `rotate`, `product`, `combination`, `permutation`,
//! `repeated_combination` and `repeated_permutation` build their whole result
//! natively without yielding to a block. Every dimension is validated first, the
//! row count is derived with checked arithmetic, and the step and memory budgets
//! are preflighted before any row is allocated. Intrinsic size guards raise
//! recoverable limit errors; exhausted quotas latch like every other charge.

use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::Buffer,
    sequence,
    value::{Heap, Kind},
};
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Member {
    Sample,
    Shuffle,
    Rotate,
    Product,
    Combination,
    Permutation,
    RepeatedCombination,
    RepeatedPermutation,
}

impl Member {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "sample" => Self::Sample,
            "shuffle" => Self::Shuffle,
            "rotate" => Self::Rotate,
            "product" => Self::Product,
            "combination" => Self::Combination,
            "permutation" => Self::Permutation,
            "repeated_combination" => Self::RepeatedCombination,
            "repeated_permutation" => Self::RepeatedPermutation,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            Self::Sample => "array.sample",
            Self::Shuffle => "array.shuffle",
            Self::Rotate => "array.rotate",
            Self::Product => "array.product",
            Self::Combination => "array.combination",
            Self::Permutation => "array.permutation",
            Self::RepeatedCombination => "array.repeated_combination",
            Self::RepeatedPermutation => "array.repeated_permutation",
        }
    }
}

fn argument(message: String) -> Error {
    Error::new(ErrorKind::Argument, message)
}

fn too_large<T>(ctx: &mut CallContext, label: &str) -> Result<T> {
    ctx.guard(ErrorKind::Arithmetic, &format!("{label} result too large"))
}

fn empty(ctx: &mut CallContext) -> Result<Value> {
    Value::from_array(ctx, Buffer::empty())
}

/// Dispatches the array combinatorics members; other receivers and names are
/// left to later dispatch stages.
pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    let Some(array) = receiver.as_array() else {
        return Ok(None);
    };
    let Some(member) = Member::parse(name) else {
        return Ok(None);
    };
    ctx.checkpoint()?;
    let label = member.label();
    if member == Member::Shuffle && !args.is_empty() {
        return Err(argument(format!("{label} does not take arguments")));
    }
    if keywords {
        return Err(argument(format!("{label} does not take keyword arguments")));
    }
    if block {
        return Err(argument(format!("{label} does not accept a block")));
    }
    let value = match member {
        Member::Sample => sample(ctx, label, array, args)?,
        Member::Shuffle => shuffle(ctx, array)?,
        Member::Rotate => rotate(ctx, label, array, args)?,
        Member::Product => product(ctx, label, array, args)?,
        Member::Combination => combination(ctx, label, array, args, false)?,
        Member::RepeatedCombination => combination(ctx, label, array, args, true)?,
        Member::Permutation => permutation(ctx, label, array, args, false)?,
        Member::RepeatedPermutation => permutation(ctx, label, array, args, true)?,
    };
    Ok(Some(value))
}

/// Converts a sample count like the reference: integers and truncated finite
/// floats are accepted, negative results are rejected separately from other
/// kinds so the message distinguishes them.
fn sample_count(label: &str, value: &Value) -> Result<usize> {
    let negative = || argument(format!("{label} count must be non-negative"));
    let invalid = || argument(format!("{label} count must be integer"));
    match &value.0 {
        Kind::Int(n) => {
            if *n < 0 {
                Err(negative())
            } else {
                Ok(usize::try_from(*n).unwrap_or(usize::MAX))
            }
        }
        Kind::Big(n) => {
            if n.negative {
                Err(negative())
            } else {
                Err(invalid())
            }
        }
        Kind::Float(n) => {
            if !n.is_finite() || *n > i64::MAX as f64 || *n < i64::MIN as f64 {
                return Err(invalid());
            }
            let truncated = n.trunc();
            if truncated < 0.0 {
                Err(negative())
            } else if truncated >= usize::MAX as f64 {
                Ok(usize::MAX)
            } else {
                Ok(truncated as usize)
            }
        }
        _ => Err(invalid()),
    }
}

/// Reads the single tuple length; a negative length selects an empty result.
fn length_argument(label: &str, args: &[Value]) -> Result<Option<u64>> {
    if args.len() != 1 {
        return Err(argument(format!("{label} expects exactly one length")));
    }
    let length = sequence::integer(&args[0])
        .map_err(|_| argument(format!("{label} length must be integer")))?;
    Ok(u64::try_from(length).ok())
}

fn checked_mul(ctx: &mut CallContext, label: &str, left: u64, right: u64) -> Result<u64> {
    match left.checked_mul(right).filter(|n| *n <= i64::MAX as u64) {
        Some(product) => Ok(product),
        None => too_large(ctx, label),
    }
}

/// Bounds a tuple materialization before anything is allocated: the reference
/// work estimate (one unit per row plus one per row slot) must fit the step
/// quota, and the outer backing plus every row's header and backing must fit
/// the memory quota. Overflowing the native width is an intrinsic size guard.
fn preflight(ctx: &mut CallContext, label: &str, count: u64, length: u64) -> Result<usize> {
    let Some(work) = count
        .checked_mul(length)
        .and_then(|slots| slots.checked_add(count))
        .filter(|work| *work <= i64::MAX as u64)
    else {
        return too_large(ctx, label);
    };
    ctx.check_steps(work)?;
    let slot = size_of::<Value>() as u64;
    let header = Heap::<Value>::header_bytes() as u64;
    let bytes = length
        .checked_mul(slot)
        .and_then(|backing| backing.checked_add(header + slot))
        .and_then(|row| row.checked_mul(count))
        .and_then(|rows| rows.checked_add(header))
        .and_then(|bytes| usize::try_from(bytes).ok());
    let Some(bytes) = bytes else {
        return too_large(ctx, label);
    };
    ctx.check_memory(bytes)?;
    match usize::try_from(count) {
        Ok(count) => Ok(count),
        Err(_) => too_large(ctx, label),
    }
}

/// Publishes one output row aliasing the receiver's elements at `indices`.
fn publish_row(ctx: &mut CallContext, array: &[Value], indices: &[usize]) -> Result<Value> {
    let mut row = Buffer::with_capacity(ctx, indices.len())?;
    for &index in indices {
        ctx.charge(1)?;
        row.data.push(array[index].clone());
    }
    Value::from_array(ctx, row)
}

fn zeroed(ctx: &mut CallContext, length: usize) -> Result<Buffer<usize>> {
    let mut indices = Buffer::with_capacity(ctx, length)?;
    for _ in 0..length {
        ctx.charge(1)?;
        indices.data.push(0);
    }
    Ok(indices)
}

// Sparse Fisher-Yates swaps use explicit buckets so their full allocation and
// every collision probe participate in the call's resource accounting.
struct Swaps(Buffer<(usize, usize)>);

impl Swaps {
    fn new(ctx: &mut CallContext, capacity: usize) -> Result<Self> {
        let mut entries = Buffer::with_capacity(ctx, capacity)?;
        for _ in 0..capacity {
            ctx.charge(1)?;
            entries.data.push((usize::MAX, 0));
        }
        Ok(Self(entries))
    }

    fn slot(&self, ctx: &mut CallContext, key: usize) -> Result<usize> {
        let mut hash = DefaultHasher::new();
        key.hash(&mut hash);
        let mask = self.0.data.len() - 1;
        let mut slot = hash.finish() as usize & mask;
        loop {
            ctx.charge(1)?;
            let stored = self.0.data[slot].0;
            if stored == usize::MAX || stored == key {
                return Ok(slot);
            }
            slot = (slot + 1) & mask;
        }
    }

    fn value(&self, slot: usize, fallback: usize) -> usize {
        let (key, value) = self.0.data[slot];
        if key == usize::MAX { fallback } else { value }
    }
}

fn sample(ctx: &mut CallContext, label: &str, array: &[Value], args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(argument(format!("{label} accepts at most one count")));
    }
    if args.is_empty() {
        if array.is_empty() {
            return Ok(Value::nil());
        }
        let index = crate::random::bounded(ctx, array.len() as u64)?;
        return Ok(array[index as usize].clone());
    }
    let count = sample_count(label, &args[0])?.min(array.len());
    if count == 0 {
        return empty(ctx);
    }
    let Some(capacity) = count
        .checked_mul(2)
        .and_then(usize::checked_next_power_of_two)
    else {
        return ctx.fail(ErrorKind::Memory, "sample scratch size overflow");
    };
    let Some(bytes) = capacity
        .checked_mul(size_of::<(usize, usize)>())
        .and_then(|scratch| {
            count
                .checked_mul(size_of::<Value>())
                .and_then(|out| scratch.checked_add(out))
        })
        .and_then(|bytes| bytes.checked_add(Heap::<Value>::header_bytes()))
    else {
        return ctx.fail(ErrorKind::Memory, "sample scratch size overflow");
    };
    ctx.check_memory(bytes)?;
    let mut swaps = Swaps::new(ctx, capacity)?;
    let mut out = Buffer::with_capacity(ctx, count)?;
    for i in 0..count {
        ctx.charge(1)?;
        let offset = crate::random::bounded(ctx, (array.len() - i) as u64)? as usize;
        let j = i + offset;
        let target = swaps.slot(ctx, j)?;
        let selected = swaps.value(target, j);
        let source = swaps.slot(ctx, i)?;
        let displaced = swaps.value(source, i);
        swaps.0.data[target] = (j, displaced);
        out.data.push(array[selected].clone());
    }
    Value::from_array(ctx, out)
}

fn shuffle(ctx: &mut CallContext, array: &[Value]) -> Result<Value> {
    let mut out = Buffer::with_capacity(ctx, array.len())?;
    out.extend(ctx, array)?;
    for i in (1..out.data.len()).rev() {
        ctx.charge(1)?;
        let j = crate::random::bounded(ctx, i as u64 + 1)? as usize;
        out.data.swap(i, j);
    }
    Value::from_array(ctx, out)
}

fn rotate(ctx: &mut CallContext, label: &str, array: &[Value], args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(argument(format!("{label} accepts at most one count")));
    }
    let offset = match args.first() {
        None => 1,
        Some(value) => sequence::integer(value)
            .map_err(|_| argument(format!("{label} count must be integer")))?,
    };
    if array.is_empty() {
        return empty(ctx);
    }
    let length = array.len();
    let shift = offset.rem_euclid(length as i64) as usize;
    let mut out = Buffer::with_capacity(ctx, length)?;
    for i in 0..length {
        ctx.charge(1)?;
        out.data.push(array[(shift + i) % length].clone());
    }
    Value::from_array(ctx, out)
}

fn product(ctx: &mut CallContext, label: &str, array: &[Value], args: &[Value]) -> Result<Value> {
    let mut dims: Buffer<&[Value]> = Buffer::with_capacity(ctx, args.len() + 1)?;
    dims.data.push(array);
    for arg in args {
        ctx.charge(1)?;
        let Some(values) = arg.as_array() else {
            return Err(Error::new(
                ErrorKind::Type,
                format!("{label} arguments must be arrays"),
            ));
        };
        dims.data.push(values);
    }
    let mut count = 1u64;
    for dim in &dims.data {
        ctx.charge(1)?;
        if dim.is_empty() {
            return empty(ctx);
        }
        count = checked_mul(ctx, label, count, dim.len() as u64)?;
    }
    let width = dims.data.len();
    let count = preflight(ctx, label, count, width as u64)?;
    let mut indices = zeroed(ctx, width)?;
    let mut out = Buffer::with_capacity(ctx, count)?;
    for _ in 0..count {
        ctx.charge(1)?;
        let mut row = Buffer::with_capacity(ctx, width)?;
        for (dim, &index) in dims.data.iter().zip(&indices.data) {
            ctx.charge(1)?;
            row.data.push(dim[index].clone());
        }
        let row = Value::from_array(ctx, row)?;
        out.push(ctx, row)?;
        for i in (0..width).rev() {
            ctx.charge(1)?;
            indices.data[i] += 1;
            if indices.data[i] < dims.data[i].len() {
                break;
            }
            indices.data[i] = 0;
        }
    }
    Value::from_array(ctx, out)
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Computes `n choose k` exactly, reducing by the running greatest common
/// divisor so the intermediate never exceeds the result.
fn combination_count(ctx: &mut CallContext, label: &str, n: u64, k: u64) -> Result<u64> {
    if k > n {
        return Ok(0);
    }
    let k = k.min(n - k);
    let mut count = 1u64;
    for i in 1..=k {
        ctx.charge(1)?;
        let mut numerator = n - k + i;
        let mut denominator = i;
        let g = gcd(numerator, denominator);
        numerator /= g;
        denominator /= g;
        let g = gcd(count, denominator);
        count /= g;
        denominator /= g;
        if denominator != 1 {
            return too_large(ctx, label);
        }
        count = checked_mul(ctx, label, count, numerator)?;
    }
    Ok(count)
}

fn advance_combination(ctx: &mut CallContext, indices: &mut [usize], n: usize) -> Result<()> {
    let k = indices.len();
    for i in (0..k).rev() {
        ctx.charge(1)?;
        if indices[i] != i + n - k {
            indices[i] += 1;
            for j in i + 1..k {
                ctx.charge(1)?;
                indices[j] = indices[j - 1] + 1;
            }
            return Ok(());
        }
    }
    Ok(())
}

fn advance_repeated_combination(
    ctx: &mut CallContext,
    indices: &mut [usize],
    n: usize,
) -> Result<()> {
    for i in (0..indices.len()).rev() {
        ctx.charge(1)?;
        if indices[i] < n - 1 {
            let next = indices[i] + 1;
            for index in &mut indices[i..] {
                ctx.charge(1)?;
                *index = next;
            }
            return Ok(());
        }
    }
    Ok(())
}

fn combination(
    ctx: &mut CallContext,
    label: &str,
    array: &[Value],
    args: &[Value],
    repeated: bool,
) -> Result<Value> {
    let Some(length) = length_argument(label, args)? else {
        return empty(ctx);
    };
    let n = array.len() as u64;
    let count = if repeated {
        if array.is_empty() && length > 0 {
            return empty(ctx);
        }
        if length == 0 {
            1
        } else {
            let Some(total) = n
                .checked_add(length - 1)
                .filter(|total| *total <= i64::MAX as u64)
            else {
                return too_large(ctx, label);
            };
            combination_count(ctx, label, total, length)?
        }
    } else {
        if length > n {
            return empty(ctx);
        }
        combination_count(ctx, label, n, length)?
    };
    let count = preflight(ctx, label, count, length)?;
    let Ok(length) = usize::try_from(length) else {
        return too_large(ctx, label);
    };
    let mut indices = zeroed(ctx, length)?;
    if !repeated {
        for (i, index) in indices.data.iter_mut().enumerate() {
            ctx.charge(1)?;
            *index = i;
        }
    }
    let mut out = Buffer::with_capacity(ctx, count)?;
    for emitted in 0..count {
        ctx.charge(1)?;
        let row = publish_row(ctx, array, &indices.data)?;
        out.push(ctx, row)?;
        if emitted + 1 == count || length == 0 {
            continue;
        }
        if repeated {
            advance_repeated_combination(ctx, &mut indices.data, array.len())?;
        } else {
            advance_combination(ctx, &mut indices.data, array.len())?;
        }
    }
    Value::from_array(ctx, out)
}

fn permutation(
    ctx: &mut CallContext,
    label: &str,
    array: &[Value],
    args: &[Value],
    repeated: bool,
) -> Result<Value> {
    let length = if !repeated && args.is_empty() {
        array.len() as u64
    } else {
        let Some(length) = length_argument(label, args)? else {
            return empty(ctx);
        };
        length
    };
    let n = array.len() as u64;
    let mut count = 1u64;
    if repeated {
        if array.is_empty() && length > 0 {
            return empty(ctx);
        }
        // A single element repeats without growing the count; wider receivers
        // overflow within 64 doublings, so this loop is bounded either way.
        if n > 1 {
            for _ in 0..length {
                ctx.charge(1)?;
                count = checked_mul(ctx, label, count, n)?;
            }
        }
    } else {
        if length > n {
            return empty(ctx);
        }
        for offset in 0..length {
            ctx.charge(1)?;
            count = checked_mul(ctx, label, count, n - length + 1 + offset)?;
        }
    }
    let count = preflight(ctx, label, count, length)?;
    let Ok(length) = usize::try_from(length) else {
        return too_large(ctx, label);
    };
    let mut out = Buffer::with_capacity(ctx, count)?;
    if length == 0 {
        ctx.charge(1)?;
        let row = empty(ctx)?;
        out.data.push(row);
        return Value::from_array(ctx, out);
    }
    if repeated {
        let mut indices = zeroed(ctx, length)?;
        for _ in 0..count {
            ctx.charge(1)?;
            let row = publish_row(ctx, array, &indices.data)?;
            out.push(ctx, row)?;
            for i in (0..length).rev() {
                ctx.charge(1)?;
                indices.data[i] += 1;
                if indices.data[i] < array.len() {
                    break;
                }
                indices.data[i] = 0;
            }
        }
        return Value::from_array(ctx, out);
    }
    // Depth-first selection of unused elements, kept iterative with explicit
    // per-depth cursors so deep tuples never grow the native stack. The
    // emission order matches the reference's recursive walk.
    let n = array.len();
    let mut choice = zeroed(ctx, length)?;
    let mut cursor = zeroed(ctx, length)?;
    let mut used: Buffer<bool> = Buffer::with_capacity(ctx, n)?;
    for _ in 0..n {
        ctx.charge(1)?;
        used.data.push(false);
    }
    let mut depth = 0;
    loop {
        ctx.charge(1)?;
        if depth == length {
            let row = publish_row(ctx, array, &choice.data)?;
            out.push(ctx, row)?;
            if depth == 0 {
                break;
            }
            depth -= 1;
            used.data[choice.data[depth]] = false;
            continue;
        }
        let mut i = cursor.data[depth];
        while i < n && used.data[i] {
            ctx.charge(1)?;
            i += 1;
        }
        if i == n {
            if depth == 0 {
                break;
            }
            depth -= 1;
            used.data[choice.data[depth]] = false;
            continue;
        }
        choice.data[depth] = i;
        used.data[i] = true;
        cursor.data[depth] = i + 1;
        depth += 1;
        if depth < length {
            cursor.data[depth] = 0;
        }
    }
    Value::from_array(ctx, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, ErrorClass, Limits};

    fn ints(values: &[i64]) -> Value {
        Value::array(values.iter().copied().map(Value::int).collect())
    }

    fn rows(value: &Value) -> Vec<Vec<i64>> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                row.as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_int().unwrap())
                    .collect()
            })
            .collect()
    }

    fn invoke(
        ctx: &mut CallContext,
        name: &str,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value> {
        call(ctx, name, receiver, args, false, false).map(Option::unwrap)
    }

    #[test]
    fn tuple_members_emit_reference_order_and_release_scratch() {
        let mut ctx = CallContext::new(CallOptions::default());
        let three = ints(&[1, 2, 3]);
        assert_eq!(
            rows(&invoke(&mut ctx, "combination", &three, &[Value::int(2)]).unwrap()),
            [[1, 2], [1, 3], [2, 3]]
        );
        assert_eq!(
            rows(&invoke(&mut ctx, "permutation", &three, &[Value::int(2)]).unwrap()),
            [[1, 2], [1, 3], [2, 1], [2, 3], [3, 1], [3, 2]]
        );
        assert_eq!(
            rows(&invoke(&mut ctx, "permutation", &three, &[]).unwrap()),
            [
                [1, 2, 3],
                [1, 3, 2],
                [2, 1, 3],
                [2, 3, 1],
                [3, 1, 2],
                [3, 2, 1]
            ]
        );
        let two = ints(&[1, 2]);
        assert_eq!(
            rows(&invoke(&mut ctx, "repeated_combination", &two, &[Value::int(2)]).unwrap()),
            [[1, 1], [1, 2], [2, 2]]
        );
        assert_eq!(
            rows(&invoke(&mut ctx, "repeated_permutation", &two, &[Value::int(2)]).unwrap()),
            [[1, 1], [1, 2], [2, 1], [2, 2]]
        );
        assert_eq!(
            rows(&invoke(&mut ctx, "product", &two, &[ints(&[3, 4])]).unwrap()),
            [[1, 3], [1, 4], [2, 3], [2, 4]]
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn counts_and_preflight_reject_overflow_before_allocating() {
        let mut ctx = CallContext::new(CallOptions::default());
        assert_eq!(combination_count(&mut ctx, "t", 20, 10).unwrap(), 184_756);
        assert_eq!(combination_count(&mut ctx, "t", 5, 6).unwrap(), 0);
        assert_eq!(combination_count(&mut ctx, "t", 0, 0).unwrap(), 1);
        let error = combination_count(&mut ctx, "t", 200, 100).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Arithmetic);
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(error.message, "t result too large");
        let peak = ctx.stats().peak_memory_bytes;
        let error = preflight(&mut ctx, "t", u64::MAX / 2, 3).unwrap_err();
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().peak_memory_bytes, peak);
        ctx.charge(1).unwrap();

        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(10),
                memory_bytes: Some(1 << 30),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let error = preflight(&mut ctx, "t", 1_000, 3).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().steps, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);

        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: None,
                memory_bytes: Some(4096),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let error = preflight(&mut ctx, "t", 1_000, 3).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
    }

    #[test]
    fn sample_counts_follow_reference_conversions() {
        assert_eq!(sample_count("t", &Value::int(3)).unwrap(), 3);
        assert_eq!(sample_count("t", &Value::float(2.9)).unwrap(), 2);
        assert_eq!(sample_count("t", &Value::float(-0.5)).unwrap(), 0);
        for (value, message) in [
            (Value::int(-1), "t count must be non-negative"),
            (Value::float(-1.5), "t count must be non-negative"),
            (Value::float(f64::NAN), "t count must be integer"),
            (Value::float(f64::INFINITY), "t count must be integer"),
            (Value::nil(), "t count must be integer"),
            (Value::bytes(b"2"), "t count must be integer"),
        ] {
            let error = sample_count("t", &value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Argument);
            assert_eq!(error.message, message);
        }
    }

    #[test]
    fn partial_results_are_released_after_quota_failures() {
        let context = |options| {
            let mut ctx = CallContext::new(options);
            ctx.random_source = Some(std::sync::Arc::new(|_, output| {
                output.fill(0);
                Ok(output.len())
            }));
            ctx
        };
        let receiver = ints(&[1, 2, 3, 4, 5, 6]);
        for (name, args) in [
            ("combination", vec![Value::int(3)]),
            ("permutation", vec![Value::int(3)]),
            ("repeated_combination", vec![Value::int(3)]),
            ("repeated_permutation", vec![Value::int(3)]),
            ("product", vec![receiver.clone(), receiver.clone()]),
            ("rotate", vec![]),
            ("shuffle", vec![]),
            ("sample", vec![Value::int(6)]),
        ] {
            let mut probe = context(CallOptions::default());
            invoke(&mut probe, name, &receiver, &args).unwrap();
            let stats = probe.stats();
            for kind in [ErrorKind::Steps, ErrorKind::Memory] {
                let maximum = if kind == ErrorKind::Steps {
                    stats.steps
                } else {
                    stats.peak_memory_bytes as u64
                };
                for limit in (0..32).map(|i| maximum * i / 32).chain([maximum - 1]) {
                    let mut ctx = context(CallOptions {
                        limits: Limits {
                            steps: (kind == ErrorKind::Steps).then_some(limit),
                            memory_bytes: (kind == ErrorKind::Memory)
                                .then(|| usize::try_from(limit).unwrap()),
                            ..Limits::default()
                        },
                        ..CallOptions::default()
                    });
                    let error = invoke(&mut ctx, name, &receiver, &args).unwrap_err();
                    assert_eq!(error.kind, kind, "{name} at {limit}");
                    assert_eq!(ctx.stats().retained_memory_bytes, 0, "{name} at {limit}");
                    assert_eq!(ctx.charge(0).unwrap_err().kind, kind, "{name} at {limit}");
                }
            }
            let mut ctx = context(CallOptions {
                limits: Limits {
                    steps: Some(stats.steps),
                    memory_bytes: Some(stats.peak_memory_bytes),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let value = invoke(&mut ctx, name, &receiver, &args).unwrap();
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0, "{name}");
        }
    }

    #[test]
    fn zero_length_permutations_need_no_receiver_sized_scratch() {
        let receiver = Value::array(vec![Value::nil(); 10_000]);
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(16),
                memory_bytes: Some(1024),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let result = invoke(&mut ctx, "permutation", &receiver, &[Value::int(0)]).unwrap();
        assert_eq!(rows(&result), [Vec::<i64>::new()]);
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn cancelled_contexts_stop_before_reading_entropy_or_building() {
        let empty = Value::array(Vec::new());
        for name in [
            "sample",
            "shuffle",
            "rotate",
            "product",
            "combination",
            "permutation",
            "repeated_combination",
            "repeated_permutation",
        ] {
            let args = if matches!(
                name,
                "combination" | "repeated_combination" | "repeated_permutation"
            ) {
                vec![Value::int(0)]
            } else {
                vec![]
            };
            let options = CallOptions::default();
            options.cancellation.cancel();
            let mut ctx = CallContext::new(options);
            let error = invoke(&mut ctx, name, &empty, &args).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Cancelled, "{name}");
            assert_eq!(ctx.stats().peak_memory_bytes, 0, "{name}");
        }
    }
}
