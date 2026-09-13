use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    value::Kind,
};
use std::{cmp::Ordering, mem::size_of, sync::Arc};

const CHUNK: usize = 256;

pub(crate) fn unlimited_context() -> CallContext {
    CallContext::new(crate::CallOptions {
        limits: crate::Limits {
            steps: None,
            memory_bytes: None,
            ..crate::Limits::default()
        },
        ..crate::CallOptions::default()
    })
}

#[derive(Debug)]
pub(crate) struct Big {
    pub negative: bool,
    words: Arc<Vec<u32>>,
    zeros: usize,
    _storage: Option<Charge>,
    header: Option<Charge>,
}

impl Big {
    fn header_bytes() -> usize {
        size_of::<Self>() + size_of::<Vec<u32>>() + 4 * size_of::<usize>()
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) || ctx.options.limits.memory_bytes.is_none() {
            return Ok(value.clone());
        }
        let storage = ctx.reserve(value.words.capacity() * size_of::<u32>())?;
        let header = ctx.reserve(Self::header_bytes())?;
        Ok(Arc::new(Self {
            negative: value.negative,
            words: value.words.clone(),
            zeros: value.zeros,
            _storage: storage,
            header,
        }))
    }

    pub fn to_float(&self) -> f64 {
        let magnitude = Magnitude::Words(&self.words);
        let bits = magnitude.bits();
        let shift = bits.saturating_sub(53);
        let mut significand = magnitude.window(shift) & ((1u64 << 53) - 1);
        if shift != 0 {
            let round = magnitude.word((shift - 1) / 32) >> ((shift - 1) % 32) & 1;
            if round != 0 && (self.zeros < shift - 1 || significand & 1 != 0) {
                significand += 1;
            }
        }
        let value = if bits > 1024 {
            f64::INFINITY
        } else {
            (significand as f64) * 2.0f64.powi(shift as i32)
        };
        if self.negative { -value } else { value }
    }
}

#[derive(Clone, Copy)]
enum Magnitude<'a> {
    Small(u64),
    Words(&'a [u32]),
}

impl Magnitude<'_> {
    fn len(self) -> usize {
        match self {
            Self::Small(0) => 0,
            Self::Small(n) => {
                if n > u32::MAX as u64 {
                    2
                } else {
                    1
                }
            }
            Self::Words(words) => words.len(),
        }
    }

    fn word(self, index: usize) -> u32 {
        match self {
            Self::Small(n) => match index {
                0 => n as u32,
                1 => (n >> 32) as u32,
                _ => 0,
            },
            Self::Words(words) => words.get(index).copied().unwrap_or(0),
        }
    }

    fn bits(self) -> usize {
        let len = self.len();
        if len == 0 {
            0
        } else {
            (len - 1) * 32 + 32 - self.word(len - 1).leading_zeros() as usize
        }
    }

    fn window(self, shift: usize) -> u64 {
        let index = shift / 32;
        let offset = shift % 32;
        let value = self.word(index) as u128
            | (self.word(index + 1) as u128) << 32
            | (self.word(index + 2) as u128) << 64;
        (value >> offset) as u64
    }
}

fn parts(value: &Value) -> (bool, Magnitude<'_>) {
    match &value.0 {
        Kind::Int(n) => (*n < 0, Magnitude::Small(n.unsigned_abs())),
        Kind::Big(n) => (n.negative, Magnitude::Words(&n.words)),
        _ => unreachable!(),
    }
}

fn work(ctx: &mut CallContext, index: usize, length: usize) -> Result<()> {
    if index % CHUNK == 0 {
        ctx.work_bytes((length - index).min(CHUNK) * size_of::<u32>())?;
    }
    Ok(())
}

fn copy(ctx: &mut CallContext, value: Magnitude<'_>) -> Result<Buffer<u32>> {
    let mut out = Buffer::with_capacity(ctx, value.len())?;
    for i in 0..value.len() {
        work(ctx, i, value.len())?;
        out.data.push(value.word(i));
    }
    Ok(out)
}

fn zeros(ctx: &mut CallContext, length: usize) -> Result<Buffer<u32>> {
    let mut out = Buffer::with_capacity(ctx, length)?;
    for start in (0..length).step_by(CHUNK) {
        let count = (length - start).min(CHUNK);
        ctx.work_bytes(count * size_of::<u32>())?;
        out.data.resize(start + count, 0);
    }
    Ok(out)
}

fn trim(ctx: &mut CallContext, words: &mut Vec<u32>) -> Result<()> {
    let mut removed = 0;
    while words.last() == Some(&0) {
        if removed % 16 == 0 {
            ctx.charge(1)?;
        }
        words.pop();
        removed += 1;
    }
    Ok(())
}

fn finish(ctx: &mut CallContext, negative: bool, mut words: Buffer<u32>) -> Result<Value> {
    trim(ctx, &mut words.data)?;
    if words.data.len() <= 2 {
        let magnitude = Magnitude::Words(&words.data).window(0);
        if magnitude <= i64::MAX as u64 {
            let n = magnitude as i64;
            return Ok(Value::int(if negative { -n } else { n }));
        }
        if negative && magnitude == 1u64 << 63 {
            return Ok(Value::int(i64::MIN));
        }
    }
    if words.data.len() < words.data.capacity() / 2 {
        words = copy(ctx, Magnitude::Words(&words.data))?;
    }
    let mut low_zero_bits = 0;
    for (i, &word) in words.data.iter().enumerate() {
        work(ctx, i, words.data.len())?;
        low_zero_bits += word.trailing_zeros() as usize;
        if word != 0 {
            break;
        }
    }
    let header = ctx.reserve(Big::header_bytes())?;
    let (data, storage) = words.into_parts();
    Ok(Value(Kind::Big(Arc::new(Big {
        negative,
        words: Arc::new(data),
        zeros: low_zero_bits,
        _storage: storage,
        header,
    }))))
}

fn compare_magnitude(
    ctx: &mut CallContext,
    a: Magnitude<'_>,
    b: Magnitude<'_>,
) -> Result<Ordering> {
    if a.len() != b.len() {
        return Ok(a.len().cmp(&b.len()));
    }
    for i in 0..a.len() {
        work(ctx, i, a.len())?;
        let order = a.word(a.len() - 1 - i).cmp(&b.word(a.len() - 1 - i));
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(Ordering::Equal)
}

pub(crate) fn compare(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Ordering> {
    let (an, a) = parts(a);
    let (bn, b) = parts(b);
    if an != bn {
        return Ok(bn.cmp(&an));
    }
    let order = compare_magnitude(ctx, a, b)?;
    Ok(if an { order.reverse() } else { order })
}

pub(crate) fn compare_float(
    ctx: &mut CallContext,
    integer: &Value,
    float: f64,
) -> Result<Option<Ordering>> {
    if float.is_nan() {
        return Ok(None);
    }
    if float.is_infinite() {
        return Ok(Some(if float.is_sign_negative() {
            Ordering::Greater
        } else {
            Ordering::Less
        }));
    }
    let (negative, magnitude) = parts(integer);
    let bits = float.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = bits & ((1u64 << 52) - 1) | if exponent == 0 { 0 } else { 1u64 << 52 };
    let shift = exponent.max(1) - 1075;
    let mut words = [0u32; 34];
    let (length, fractional) = if shift >= 0 {
        let start = shift as usize / 32;
        let wide = (mantissa as u128) << (shift as usize % 32);
        for i in 0..3 {
            words[start + i] = (wide >> (i * 32)) as u32;
        }
        (start + 3, false)
    } else {
        let shift = (-shift) as u32;
        let whole = mantissa.checked_shr(shift).unwrap_or(0);
        words[0] = whole as u32;
        words[1] = (whole >> 32) as u32;
        (
            2,
            shift >= 64 && mantissa != 0 || shift < 64 && mantissa & ((1u64 << shift) - 1) != 0,
        )
    };
    let length = words[..length]
        .iter()
        .rposition(|&n| n != 0)
        .map_or(0, |i| i + 1);
    let float_negative = float < 0.0;
    if negative != float_negative {
        return Ok(Some(float_negative.cmp(&negative)));
    }
    let mut order = compare_magnitude(ctx, magnitude, Magnitude::Words(&words[..length]))?;
    if order == Ordering::Equal && fractional {
        order = Ordering::Less;
    }
    Ok(Some(if negative { order.reverse() } else { order }))
}

fn add(ctx: &mut CallContext, a: Magnitude<'_>, b: Magnitude<'_>) -> Result<Buffer<u32>> {
    let length = a.len().max(b.len());
    let mut out = Buffer::with_capacity(ctx, length + 1)?;
    let mut carry = 0u64;
    for i in 0..length {
        work(ctx, i, length)?;
        carry += a.word(i) as u64 + b.word(i) as u64;
        out.data.push(carry as u32);
        carry >>= 32;
    }
    if carry != 0 {
        out.data.push(carry as u32);
    }
    Ok(out)
}

fn subtract(ctx: &mut CallContext, a: &mut [u32], b: Magnitude<'_>) -> Result<()> {
    let mut borrow = 0u64;
    for i in 0..a.len() {
        work(ctx, i, a.len())?;
        let sub = b.word(i) as u64 + borrow;
        let word = a[i] as u64;
        a[i] = word.wrapping_sub(sub) as u32;
        borrow = u64::from(word < sub);
    }
    debug_assert_eq!(borrow, 0);
    Ok(())
}

fn multiply(ctx: &mut CallContext, a: Magnitude<'_>, b: Magnitude<'_>) -> Result<Buffer<u32>> {
    if a.len() == 0 || b.len() == 0 {
        return Ok(Buffer::empty());
    }
    let length = a
        .len()
        .checked_add(b.len())
        .ok_or_else(|| Error::new(ErrorKind::Memory, "integer size overflow"))?;
    let mut out = zeros(ctx, length)?;
    for i in 0..a.len() {
        ctx.charge(1)?;
        let mut carry = 0u64;
        for j in 0..b.len() {
            work(ctx, j, b.len())?;
            carry += a.word(i) as u64 * b.word(j) as u64 + out.data[i + j] as u64;
            out.data[i + j] = carry as u32;
            carry >>= 32;
        }
        out.data[i + b.len()] = carry as u32;
    }
    Ok(out)
}

fn divide_small(ctx: &mut CallContext, words: &mut Vec<u32>, divisor: u32) -> Result<u32> {
    let mut remainder = 0u64;
    for i in 0..words.len() {
        work(ctx, i, words.len())?;
        let index = words.len() - 1 - i;
        let value = remainder << 32 | words[index] as u64;
        words[index] = (value / divisor as u64) as u32;
        remainder = value % divisor as u64;
    }
    trim(ctx, words)?;
    Ok(remainder as u32)
}

fn divide(
    ctx: &mut CallContext,
    a: Magnitude<'_>,
    b: Magnitude<'_>,
) -> Result<(Buffer<u32>, Buffer<u32>)> {
    if b.len() == 0 {
        return Err(Error::new(ErrorKind::Arithmetic, "division by zero"));
    }
    if compare_magnitude(ctx, a, b)? == Ordering::Less {
        return Ok((Buffer::empty(), copy(ctx, a)?));
    }
    if b.len() == 1 {
        let mut quotient = copy(ctx, a)?;
        let remainder = divide_small(ctx, &mut quotient.data, b.word(0))?;
        return Ok((quotient, copy(ctx, Magnitude::Small(remainder as u64))?));
    }
    let mut quotient = zeros(ctx, a.len())?;
    let mut remainder = Buffer::with_capacity(ctx, b.len() + 1)?;
    for bit in (0..a.bits()).rev() {
        ctx.charge(1)?;
        let mut carry = a.word(bit / 32) >> (bit % 32) & 1;
        let length = remainder.data.len();
        for i in 0..length {
            work(ctx, i, length)?;
            let word = remainder.data[i];
            remainder.data[i] = word << 1 | carry;
            carry = word >> 31;
        }
        if carry != 0 {
            remainder.data.push(carry);
        }
        if compare_magnitude(ctx, Magnitude::Words(&remainder.data), b)? != Ordering::Less {
            subtract(ctx, &mut remainder.data, b)?;
            trim(ctx, &mut remainder.data)?;
            quotient.data[bit / 32] |= 1 << (bit % 32);
        }
    }
    Ok((quotient, remainder))
}

pub(crate) fn negate(ctx: &mut CallContext, value: &Value, absolute: bool) -> Result<Value> {
    let (negative, magnitude) = parts(value);
    let words = copy(ctx, magnitude)?;
    finish(ctx, !absolute && !negative, words)
}

pub(crate) fn binary(ctx: &mut CallContext, op: &str, a: &Value, b: &Value) -> Result<Value> {
    if op == "**" {
        return power(ctx, a, b);
    }
    let (an, am) = parts(a);
    let (mut bn, bm) = parts(b);
    match op {
        "+" | "-" => {
            bn ^= op == "-" && bm.len() != 0;
            if an == bn {
                let words = add(ctx, am, bm)?;
                finish(ctx, an, words)
            } else {
                let (negative, larger, smaller) =
                    if compare_magnitude(ctx, am, bm)? == Ordering::Less {
                        (bn, bm, am)
                    } else {
                        (an, am, bm)
                    };
                let mut words = copy(ctx, larger)?;
                subtract(ctx, &mut words.data, smaller)?;
                finish(ctx, negative, words)
            }
        }
        "*" => {
            let words = multiply(ctx, am, bm)?;
            finish(ctx, an != bn, words)
        }
        "/" | "%" | "remainder" => {
            let (mut quotient, mut remainder) = divide(ctx, am, bm)?;
            if op != "remainder" && !remainder.data.is_empty() && an != bn {
                quotient = add(ctx, Magnitude::Words(&quotient.data), Magnitude::Small(1))?;
                let mut corrected = copy(ctx, bm)?;
                subtract(ctx, &mut corrected.data, Magnitude::Words(&remainder.data))?;
                remainder = corrected;
            }
            if op == "/" {
                finish(ctx, an != bn, quotient)
            } else {
                finish(ctx, if op == "remainder" { an } else { bn }, remainder)
            }
        }
        _ => Err(Error::new(ErrorKind::Type, "unsupported integer operator")),
    }
}

fn power(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Value> {
    let (negative, exponent) = parts(b);
    if negative {
        return crate::ops::float_power(a.as_float().unwrap(), b.as_float().unwrap());
    }
    if exponent.len() == 0 {
        return Ok(Value::int(1));
    }
    if let Kind::Int(base @ (-1..=1)) = a.0 {
        return Ok(Value::int(if base == -1 && exponent.word(0) & 1 == 0 {
            1
        } else {
            base
        }));
    }
    let Some(mut exponent) = b.as_int().map(|n| n as u64) else {
        return Err(Error::new(ErrorKind::Arithmetic, "exponent is too large"));
    };
    let bits = parts(a).1.bits() as u128;
    let projected = (bits - 1) * exponent as u128 + 1;
    let bytes = usize::try_from(projected.div_ceil(32) * 4)
        .map_err(|_| Error::new(ErrorKind::Memory, "integer size overflow"))?;
    // Reject impossible growth before starting repeated squaring.
    drop(ctx.reserve(bytes)?);
    ctx.charge(u64::try_from(projected.div_ceil(256)).unwrap_or(u64::MAX))?;
    let mut result = Value::int(1);
    let mut base = a.clone();
    while exponent != 0 {
        ctx.charge(1)?;
        if exponent & 1 != 0 {
            result = binary(ctx, "*", &result, &base)?;
        }
        exponent >>= 1;
        if exponent != 0 {
            base = binary(ctx, "*", &base, &base)?;
        }
    }
    Ok(result)
}

pub(crate) fn odd(value: &Value) -> bool {
    parts(value).1.word(0) & 1 != 0
}

pub(crate) fn bits(value: &Value) -> usize {
    parts(value).1.bits()
}

pub(crate) fn divmod(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<(Value, Value)> {
    let (an, am) = parts(a);
    let (bn, bm) = parts(b);
    let (mut quotient, mut remainder) = divide(ctx, am, bm)?;
    if !remainder.data.is_empty() && an != bn {
        quotient = add(ctx, Magnitude::Words(&quotient.data), Magnitude::Small(1))?;
        let mut corrected = copy(ctx, bm)?;
        subtract(ctx, &mut corrected.data, Magnitude::Words(&remainder.data))?;
        remainder = corrected;
    }
    Ok((
        finish(ctx, an != bn, quotient)?,
        finish(ctx, bn, remainder)?,
    ))
}

pub(crate) fn from_float(ctx: &mut CallContext, value: f64) -> Result<Value> {
    if !value.is_finite() {
        return Err(Error::new(
            ErrorKind::Arithmetic,
            "cannot convert a non-finite float to integer",
        ));
    }
    if value >= i64::MIN as f64 && value < 9223372036854775808.0 {
        return Ok(Value::int(value as i64));
    }
    let bits = value.abs().to_bits();
    let shift = ((bits >> 52) & 0x7ff) as usize - 1075;
    let mantissa = bits & ((1u64 << 52) - 1) | 1u64 << 52;
    let start = shift / 32;
    let wide = (mantissa as u128) << (shift % 32);
    let mut words = zeros(ctx, start + 3)?;
    for i in 0..3 {
        words.data[start + i] = (wide >> (i * 32)) as u32;
    }
    finish(ctx, value < 0.0, words)
}

pub(crate) fn parse(ctx: &mut CallContext, text: &[u8], radix: u32) -> Result<Value> {
    let digits = text.len() - usize::from(matches!(text.first(), Some(b'-' | b'+')));
    if digits > 100_000 {
        return Err(Error::new(
            ErrorKind::Argument,
            "integer conversion exceeds 100000 digits",
        ));
    }
    parse_digits(ctx, text, radix)
}

pub(crate) fn parse_digits(ctx: &mut CallContext, text: &[u8], radix: u32) -> Result<Value> {
    if !(2..=36).contains(&radix) {
        return Err(Error::new(
            ErrorKind::Argument,
            "integer base must be between 2 and 36",
        ));
    }
    let negative = text.first() == Some(&b'-');
    let text = if matches!(text.first(), Some(b'-' | b'+')) {
        &text[1..]
    } else {
        text
    };
    if text.is_empty() {
        return Err(Error::new(ErrorKind::Argument, "invalid integer"));
    }
    let mut words = Buffer::empty();
    let mut compact = Some(0u64);
    for &byte in text {
        ctx.charge(1)?;
        let digit = (byte as char)
            .to_digit(radix)
            .ok_or_else(|| Error::new(ErrorKind::Argument, "invalid integer digit"))?;
        if let Some(value) = compact {
            if let Some(next) = value
                .checked_mul(radix as u64)
                .and_then(|n| n.checked_add(digit as u64))
            {
                compact = Some(next);
                continue;
            }
            words = copy(ctx, Magnitude::Small(value))?;
            compact = None;
        }
        let mut carry = digit as u64;
        let length = words.data.len();
        for i in 0..length {
            work(ctx, i, length)?;
            carry += words.data[i] as u64 * radix as u64;
            words.data[i] = carry as u32;
            carry >>= 32;
        }
        if carry != 0 {
            words.push(ctx, carry as u32)?;
        }
    }
    if let Some(value) = compact {
        if value <= i64::MAX as u64 {
            return Ok(Value::int(if negative {
                -(value as i64)
            } else {
                value as i64
            }));
        }
        if negative && value == 1u64 << 63 {
            return Ok(Value::int(i64::MIN));
        }
        words = copy(ctx, Magnitude::Small(value))?;
    }
    finish(ctx, negative, words)
}

pub(crate) fn format(ctx: &mut CallContext, value: &Value, radix: u32) -> Result<Buffer<u8>> {
    if !(2..=36).contains(&radix) {
        return Err(Error::new(
            ErrorKind::Argument,
            "integer base must be between 2 and 36",
        ));
    }
    let (negative, magnitude) = parts(value);
    let capacity = magnitude.bits() / radix.ilog2() as usize + 2;
    let mut out = Buffer::with_capacity(ctx, capacity)?;
    let mut words = copy(ctx, magnitude)?;
    let mut power = radix;
    let mut width = 1;
    while let Some(next) = power.checked_mul(radix) {
        power = next;
        width += 1;
    }
    while !words.data.is_empty() {
        ctx.charge(1)?;
        let mut digits = divide_small(ctx, &mut words.data, power)?;
        for _ in 0..width {
            out.push(
                ctx,
                b"0123456789abcdefghijklmnopqrstuvwxyz"[(digits % radix) as usize],
            )?;
            digits /= radix;
            if words.data.is_empty() && digits == 0 {
                break;
            }
        }
    }
    if out.data.is_empty() {
        out.push(ctx, b'0')?;
    }
    if negative {
        out.push(ctx, b'-')?;
    }
    let length = out.data.len() / 2;
    for i in 0..length {
        work(ctx, i, length)?;
        let end = out.data.len() - 1 - i;
        out.data.swap(i, end);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn imported_backing_has_independent_budget_lifetimes() {
        let host = Value::parse_integer(&"f".repeat(8192), 16).unwrap();
        let mut first = CallContext::new(CallOptions::default());
        let original = first.import(&host).unwrap();
        let mut second = CallContext::new(CallOptions::default());
        let imported = second.import(&original).unwrap();
        let (Kind::Big(a), Kind::Big(b)) = (&original.0, &imported.0) else {
            panic!()
        };
        assert!(Arc::ptr_eq(&a.words, &b.words));
        let retained = first.stats().retained_memory_bytes;
        assert!(retained >= 4096);
        assert_eq!(second.stats().retained_memory_bytes, retained);
        let alias = first.import(&original).unwrap();
        assert_eq!(first.stats().retained_memory_bytes, retained);
        drop(original);
        assert_eq!(first.stats().retained_memory_bytes, retained);
        drop(alias);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(imported);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn compact_parsing_uses_no_tracked_heap_storage() {
        let mut ctx = CallContext::new(CallOptions::default());
        for text in [
            "0",
            "-0",
            "1234",
            "-9223372036854775808",
            "9223372036854775807",
        ] {
            let value = parse(&mut ctx, text.as_bytes(), 10).unwrap();
            assert!(value.as_int().is_some());
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
        }
    }
}
