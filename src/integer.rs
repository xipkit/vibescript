use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    value::Kind,
};
use std::{cmp::Ordering, mem::size_of, sync::Arc};

mod division;
mod limbs;
mod ntt;
mod radix;

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
        if ctx.owns(&value.header) {
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

    /// Returns the float equal to this integer, when one exists.
    pub(crate) fn exact_float(&self) -> Option<f64> {
        let bits = Magnitude::Words(&self.words).bits();
        (bits <= 1024 && bits - self.zeros <= 53).then(|| self.to_float())
    }

    /// Returns the magnitude's little-endian words, without high zero words.
    pub(crate) fn words(&self) -> &[u32] {
        &self.words
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

impl<'a> Magnitude<'a> {
    /// Returns the words, using `scratch` to hold a compact magnitude's.
    fn slice<'b>(self, scratch: &'b mut [u32; 2]) -> &'b [u32]
    where
        'a: 'b,
    {
        match self {
            Self::Small(n) => {
                *scratch = [n as u32, (n >> 32) as u32];
                &scratch[..self.len()]
            }
            Self::Words(words) => words,
        }
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

/// Operands shorter than this many words multiply by the schoolbook method.
const KARATSUBA: usize = 64;
/// Operands at least this many words long multiply through number-theoretic
/// transforms.
const TRANSFORM: usize = 128;

fn multiply(ctx: &mut CallContext, a: Magnitude<'_>, b: Magnitude<'_>) -> Result<Buffer<u32>> {
    if a.len() == 0 || b.len() == 0 {
        return Ok(Buffer::empty());
    }
    let length = a
        .len()
        .checked_add(b.len())
        .ok_or_else(|| Error::new(ErrorKind::Memory, "integer size overflow"))?;
    let mut out = zeros(ctx, length)?;
    let (mut x, mut y) = ([0; 2], [0; 2]);
    multiply_into(ctx, &mut out.data, a.slice(&mut x), b.slice(&mut y))?;
    Ok(out)
}

/// Returns `a * b` in a new buffer of `a.len() + b.len()` words.
fn product(ctx: &mut CallContext, a: &[u32], b: &[u32]) -> Result<Buffer<u32>> {
    let mut out = zeros(ctx, a.len() + b.len())?;
    multiply_into(ctx, &mut out.data, a, b)?;
    Ok(out)
}

/// Adds `a * b` into `out`, which must hold the sum.
///
/// Short operands use the schoolbook method, charged one step per row and
/// one per sixteen word products. Longer ones split Karatsuba-style into
/// three half-size products, so an n-word product costs O(n^1.59) work, and
/// the longest convolve through number-theoretic transforms in O(n log n).
fn multiply_into(ctx: &mut CallContext, out: &mut [u32], a: &[u32], b: &[u32]) -> Result<()> {
    // `b` is the shorter operand.
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    if b.is_empty() {
        return Ok(());
    }
    if b.len() >= TRANSFORM {
        return ntt::multiply_into(ctx, out, a, b);
    }
    if b.len() < KARATSUBA {
        for (i, &y) in b.iter().enumerate() {
            ctx.charge(1)?;
            let mut carry = 0u64;
            for (j, &x) in a.iter().enumerate() {
                work(ctx, j, a.len())?;
                carry += x as u64 * y as u64 + out[i + j] as u64;
                out[i + j] = carry as u32;
                carry >>= 32;
            }
            if carry != 0 {
                add_into(ctx, &mut out[i + a.len()..], &[carry as u32])?;
            }
        }
        return Ok(());
    }
    if a.len() >= 2 * b.len() {
        for (index, chunk) in a.chunks(b.len()).enumerate() {
            multiply_into(ctx, &mut out[index * b.len()..], chunk, b)?;
        }
        return Ok(());
    }
    let half = b.len() / 2;
    let (a0, a1) = a.split_at(half);
    let (b0, b1) = b.split_at(half);
    let low = product(ctx, a0, b0)?;
    let high = product(ctx, a1, b1)?;
    let mut middle = {
        let x = sum(ctx, a0, a1)?;
        let y = sum(ctx, b0, b1)?;
        product(ctx, significant(&x.data), significant(&y.data))?
    };
    // (a0 + a1)(b0 + b1) - a0 b0 - a1 b1 = a0 b1 + a1 b0 >= 0.
    subtract(
        ctx,
        &mut middle.data,
        Magnitude::Words(significant(&low.data)),
    )?;
    subtract(
        ctx,
        &mut middle.data,
        Magnitude::Words(significant(&high.data)),
    )?;
    add_into(ctx, out, significant(&low.data))?;
    add_into(ctx, &mut out[half..], significant(&middle.data))?;
    add_into(ctx, &mut out[2 * half..], significant(&high.data))
}

/// Returns `a + b` in a new buffer one word longer than the longer operand.
fn sum(ctx: &mut CallContext, a: &[u32], b: &[u32]) -> Result<Buffer<u32>> {
    add(ctx, Magnitude::Words(a), Magnitude::Words(b))
}

/// Drops high zero words without charging; callers charge the words they then read.
fn significant(words: &[u32]) -> &[u32] {
    let length = words
        .iter()
        .rposition(|&word| word != 0)
        .map_or(0, |i| i + 1);
    &words[..length]
}

/// Adds `value` into `out`, propagating the carry; `out` must hold the sum.
fn add_into(ctx: &mut CallContext, out: &mut [u32], value: &[u32]) -> Result<()> {
    let mut carry = 0u64;
    for (i, &word) in value.iter().enumerate() {
        work(ctx, i, value.len())?;
        carry += out[i] as u64 + word as u64;
        out[i] = carry as u32;
        carry >>= 32;
    }
    // A carry ripples through words that were all ones, at the same rate.
    let mut i = value.len();
    while carry != 0 {
        if (i - value.len()) % 16 == 0 {
            ctx.charge(1)?;
        }
        carry += out[i] as u64;
        out[i] = carry as u32;
        carry >>= 32;
        i += 1;
    }
    Ok(())
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
        return Err(Error::new(ErrorKind::Arithmetic, "division by zero")
            .with_class(crate::ErrorClass::ZeroDivision));
    }
    if compare_magnitude(ctx, a, b)? == Ordering::Less {
        return Ok((Buffer::empty(), copy(ctx, a)?));
    }
    if b.len() == 1 {
        let mut quotient = copy(ctx, a)?;
        let remainder = divide_small(ctx, &mut quotient.data, b.word(0))?;
        return Ok((quotient, copy(ctx, Magnitude::Small(remainder as u64))?));
    }
    let (mut x, mut y) = ([0; 2], [0; 2]);
    let (a, b) = (a.slice(&mut x), b.slice(&mut y));
    let quotient = a.len() - b.len();
    if b.len().min(quotient) >= division::THRESHOLD && b.len().max(quotient) >= division::LONG {
        return division::divide(ctx, a, b);
    }
    long_divide(ctx, a, b)
}

/// Shifts `words` left by `shift` bits below 32 into a new buffer of
/// `length` words, which must hold the result.
fn shifted(ctx: &mut CallContext, words: &[u32], shift: u32, length: usize) -> Result<Buffer<u32>> {
    let mut out = zeros(ctx, length)?;
    for (i, &word) in words.iter().enumerate() {
        work(ctx, i, words.len())?;
        let wide = (word as u64) << shift;
        out.data[i] |= wide as u32;
        if (wide >> 32) != 0 {
            out.data[i + 1] = (wide >> 32) as u32;
        }
    }
    Ok(out)
}

/// Divides by a divisor of at least two words with Knuth's algorithm D,
/// estimating one quotient word at a time from the leading words. Each
/// quotient word costs one step and a charged pass over the divisor, so the
/// work is proportional to the product of the quotient and divisor lengths.
fn long_divide(ctx: &mut CallContext, a: &[u32], b: &[u32]) -> Result<(Buffer<u32>, Buffer<u32>)> {
    let n = b.len();
    let m = a.len() - n;
    // Normalizing puts the divisor's top bit in its leading word.
    let shift = b[n - 1].leading_zeros();
    let divisor = shifted(ctx, b, shift, n)?;
    let mut rest = shifted(ctx, a, shift, a.len() + 1)?;
    let (v, u) = (&divisor.data, &mut rest.data);
    let mut quotient = zeros(ctx, m + 1)?;
    for j in (0..=m).rev() {
        ctx.charge(1)?;
        let top = (u[j + n] as u64) << 32 | u[j + n - 1] as u64;
        let mut estimate = top / v[n - 1] as u64;
        let mut remainder = top % v[n - 1] as u64;
        while estimate > u32::MAX as u64
            || estimate * v[n - 2] as u64 > (remainder << 32 | u[j + n - 2] as u64)
        {
            estimate -= 1;
            remainder += v[n - 1] as u64;
            if remainder > u32::MAX as u64 {
                break;
            }
        }
        let mut borrow = 0i64;
        for i in 0..n {
            work(ctx, i, n)?;
            let product = estimate * v[i] as u64;
            let difference = u[i + j] as i64 - borrow - (product & u32::MAX as u64) as i64;
            u[i + j] = difference as u32;
            borrow = (product >> 32) as i64 - (difference >> 32);
        }
        let difference = u[j + n] as i64 - borrow;
        u[j + n] = difference as u32;
        if difference < 0 {
            // The estimate was one too large: add the divisor back.
            estimate -= 1;
            let mut carry = 0u64;
            for i in 0..n {
                work(ctx, i, n)?;
                carry += u[i + j] as u64 + v[i] as u64;
                u[i + j] = carry as u32;
                carry >>= 32;
            }
            u[j + n] = u[j + n].wrapping_add(carry as u32);
        }
        quotient.data[j] = estimate as u32;
    }
    let mut remainder = zeros(ctx, n)?;
    for i in 0..n {
        work(ctx, i, n)?;
        let wide = (u[i + 1] as u64) << 32 | u[i] as u64;
        remainder.data[i] = (wide >> shift) as u32;
    }
    trim(ctx, &mut remainder.data)?;
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
        "//" | "%" | "remainder" => {
            let (mut quotient, mut remainder) = divide(ctx, am, bm)?;
            if op != "remainder" && !remainder.data.is_empty() && an != bn {
                quotient = add(ctx, Magnitude::Words(&quotient.data), Magnitude::Small(1))?;
                let mut corrected = copy(ctx, bm)?;
                subtract(ctx, &mut corrected.data, Magnitude::Words(&remainder.data))?;
                remainder = corrected;
            }
            if op == "//" {
                finish(ctx, an != bn, quotient)
            } else {
                finish(ctx, if op == "remainder" { an } else { bn }, remainder)
            }
        }
        _ => Err(Error::new(ErrorKind::Type, "unsupported integer operator")),
    }
}

/// Divides two integers to the float nearest their exact quotient, rounding
/// half to even, as `/` does (ADR-008). A zero divisor raises
/// `ZeroDivisionError` and a quotient beyond the float range raises; one
/// too small for a float is zero.
pub(crate) fn true_divide(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Value> {
    // The binary exponent range and precision of a float.
    const MIN_EXP: i64 = -1021;
    const MAX_EXP: i64 = 1024;
    const DIGITS: i64 = 53;
    let (an, am) = parts(a);
    let (bn, bm) = parts(b);
    if bm.len() == 0 {
        return Err(Error::new(ErrorKind::Arithmetic, "division by zero")
            .with_class(crate::ErrorClass::ZeroDivision));
    }
    let signed = |value: f64| Value::float(if an != bn { -value } else { value });
    let (a_bits, b_bits) = (am.bits() as i64, bm.bits() as i64);
    // Both operands are exact floats, so one float division rounds correctly.
    if a_bits <= DIGITS && b_bits <= DIGITS {
        return Ok(signed(am.window(0) as f64 / bm.window(0) as f64));
    }
    let overflow = || {
        Error::new(
            ErrorKind::Arithmetic,
            "integer division result is out of float range",
        )
    };
    let diff = a_bits - b_bits;
    if diff > MAX_EXP {
        return Err(overflow());
    }
    if diff < MIN_EXP - DIGITS - 1 {
        return Ok(signed(0.0));
    }
    // Scale the dividend so the integer quotient keeps two or three bits
    // beyond the float's precision, or its subnormal precision, and note
    // whether anything the scaling or the division drops is nonzero.
    let shift = diff.max(MIN_EXP) - DIGITS - 2;
    let (scaled, mut inexact) = if shift <= 0 {
        (shift_left(ctx, am, (-shift) as usize)?, false)
    } else {
        shift_right(ctx, am, shift as usize)?
    };
    let (quotient, remainder) = divide(ctx, Magnitude::Words(&scaled.data), bm)?;
    inexact |= remainder.data.iter().any(|&word| word != 0);
    let quotient = Magnitude::Words(&quotient.data);
    let quotient_bits = quotient.bits() as i64;
    let extra = quotient_bits.max(MIN_EXP - shift) - DIGITS;
    let mask = 1u64 << (extra - 1);
    let mut low = quotient.window(0) | u64::from(inexact);
    if low & mask != 0 && low & (3 * mask - 1) != 0 {
        low += mask;
    }
    low &= !(2 * mask - 1);
    // At most 54 significant bits remain, so the conversion is exact.
    let value = low as f64;
    if shift + quotient_bits >= MAX_EXP
        && (shift + quotient_bits > MAX_EXP || value == power_of_two(quotient_bits))
    {
        return Err(overflow());
    }
    // Two exact steps, since 2^shift alone may not be a normal float.
    let half = shift / 2;
    Ok(signed(
        value * power_of_two(half) * power_of_two(shift - half),
    ))
}

/// 2^`exponent`, for an exponent of a normal float.
fn power_of_two(exponent: i64) -> f64 {
    f64::from_bits(((exponent + 1023) as u64) << 52)
}

/// `value` shifted left by `bits`.
fn shift_left(ctx: &mut CallContext, value: Magnitude<'_>, bits: usize) -> Result<Buffer<u32>> {
    let (words, offset) = (bits / 32, (bits % 32) as u32);
    let mut out = zeros(ctx, words + value.len() + 1)?;
    for i in 0..value.len() {
        work(ctx, i, value.len())?;
        let wide = (value.word(i) as u64) << offset;
        out.data[words + i] |= wide as u32;
        out.data[words + i + 1] = (wide >> 32) as u32;
    }
    trim(ctx, &mut out.data)?;
    Ok(out)
}

/// `value` shifted right by `bits`, and whether any bit shifted out was set.
fn shift_right(
    ctx: &mut CallContext,
    value: Magnitude<'_>,
    bits: usize,
) -> Result<(Buffer<u32>, bool)> {
    let (words, offset) = (bits / 32, (bits % 32) as u32);
    let mut lost = false;
    for i in 0..words.min(value.len()) {
        work(ctx, i, value.len())?;
        lost |= value.word(i) != 0;
    }
    lost |= offset != 0 && value.word(words) & ((1 << offset) - 1) != 0;
    let length = value.len().saturating_sub(words);
    let mut out = zeros(ctx, length)?;
    for i in 0..length {
        work(ctx, i, length)?;
        let wide = (value.word(words + i + 1) as u64) << 32 | value.word(words + i) as u64;
        out.data[i] = (wide >> offset) as u32;
    }
    trim(ctx, &mut out.data)?;
    Ok((out, lost))
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
        return ctx.guard(
            ErrorKind::Arithmetic,
            "integer exponentiation exponent is too large",
        );
    };
    let bits = parts(a).1.bits() as u128;
    let projected = (bits - 1) * exponent as u128 + 1;
    let bytes = usize::try_from(projected.div_ceil(32) * 4)
        .map_err(|_| Error::new(ErrorKind::Memory, "integer size overflow"))?;
    // Reject impossible growth before starting repeated squaring.
    drop(ctx.reserve(bytes)?);
    ctx.charge(u64::try_from(projected.div_ceil(256)).unwrap_or(u64::MAX))?;
    let (negative, magnitude) = parts(a);
    let low_zero_bits = match &a.0 {
        Kind::Big(big) => big.zeros,
        _ => magnitude.word(0).trailing_zeros() as usize,
    };
    if low_zero_bits + 1 == magnitude.bits() {
        // A power of two raised to a power is a single bit, set directly.
        let bit = (projected - 1) as usize;
        let mut words = zeros(ctx, bit / 32 + 1)?;
        words.data[bit / 32] = 1 << (bit % 32);
        return finish(ctx, negative && exponent & 1 != 0, words);
    }
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

/// Converts a float truncated toward zero; `method` names a non-finite value's
/// failure as Go does.
pub(crate) fn from_float(ctx: &mut CallContext, value: f64, method: &str) -> Result<Value> {
    if !value.is_finite() {
        return Err(Error::new(
            ErrorKind::Arithmetic,
            format!("{method} result out of int64 range"),
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
        return ctx.guard(
            ErrorKind::Argument,
            "integer conversion exceeds 100000 digits",
        );
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
    let mut compact = Some(0u64);
    for &byte in text {
        ctx.charge(1)?;
        let digit = (byte as char)
            .to_digit(radix)
            .ok_or_else(|| Error::new(ErrorKind::Argument, "invalid integer digit"))?;
        compact = compact
            .and_then(|value| value.checked_mul(radix as u64))
            .and_then(|value| value.checked_add(digit as u64));
    }
    let Some(value) = compact else {
        let words = radix::parse(ctx, text, radix)?;
        return finish(ctx, negative, words);
    };
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
    let words = copy(ctx, Magnitude::Small(value))?;
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
    if radix.is_power_of_two() && magnitude.len() > radix::DIRECT {
        // Long magnitudes read their digits' bits directly.
        if negative {
            out.data.push(b'-');
        }
        let mut scratch = [0; 2];
        radix::format_bits(
            ctx,
            magnitude.slice(&mut scratch),
            radix.trailing_zeros(),
            &mut out,
        )?;
        return Ok(out);
    }
    if magnitude.len() > limbs::DIRECT {
        // Long magnitudes convert by halves in limbs of the radix.
        if negative {
            out.data.push(b'-');
        }
        let mut scratch = [0; 2];
        limbs::format(ctx, magnitude.slice(&mut scratch), radix, &mut out)?;
        return Ok(out);
    }
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

    /// Deterministic xorshift words, so failures reproduce.
    struct Words(u64);

    impl Words {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 16) as u32
        }

        /// Returns `length` words shaped to reach carry, borrow and correction paths.
        fn take(&mut self, length: usize) -> Vec<u32> {
            let shape = self.next() % 4;
            let mut words: Vec<u32> = (0..length)
                .map(|_| match shape {
                    0 => u32::MAX,
                    1 => self.next() | 0x8000_0000,
                    2 => self.next() & 0x0000_ffff,
                    _ => self.next(),
                })
                .collect();
            if let Some(last) = words.last_mut() {
                *last |= 1;
            }
            words
        }
    }

    fn schoolbook(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = vec![0u32; a.len() + b.len()];
        for (i, &x) in a.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &y) in b.iter().enumerate() {
                carry += x as u64 * y as u64 + out[i + j] as u64;
                out[i + j] = carry as u32;
                carry >>= 32;
            }
            out[i + b.len()] = carry as u32;
        }
        while out.last() == Some(&0) {
            out.pop();
        }
        out
    }

    fn trimmed(mut words: Vec<u32>) -> Vec<u32> {
        while words.last() == Some(&0) {
            words.pop();
        }
        words
    }

    #[test]
    fn split_products_and_long_division_agree_with_schoolbook_arithmetic() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut words = Words(0x9e37_79b9_7f4a_7c15);
        let sizes = [1, 2, 3, 31, 63, 64, 65, 100, 127, 128, 129, 200, 257, 400];
        for &x in &sizes {
            for &y in &sizes {
                let (a, b) = (words.take(x), words.take(y));
                let product =
                    multiply(&mut ctx, Magnitude::Words(&a), Magnitude::Words(&b)).unwrap();
                let expected = schoolbook(&a, &b);
                assert_eq!(trimmed(product.data.clone()), expected, "{x} x {y}");
                // Dividing the product plus a smaller remainder recovers both.
                let remainder = trimmed(words.take(y.min(x)));
                let remainder = if compare_magnitude(
                    &mut ctx,
                    Magnitude::Words(&remainder),
                    Magnitude::Words(&b),
                )
                .unwrap()
                    == Ordering::Less
                {
                    remainder
                } else {
                    Vec::new()
                };
                let mut dividend = product.data.clone();
                add_into(&mut ctx, &mut dividend, &remainder).unwrap();
                let dividend = trimmed(dividend);
                let (quotient, rest) =
                    divide(&mut ctx, Magnitude::Words(&dividend), Magnitude::Words(&b)).unwrap();
                assert_eq!(trimmed(quotient.data), trimmed(a.clone()), "{x} / {y}");
                assert_eq!(rest.data, remainder, "{x} % {y}");
            }
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn transform_products_agree_with_schoolbook_arithmetic() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut words = Words(0x51a3_c0de_1234_9876);
        for (x, y) in [
            (1, 1),
            (2, 1),
            (5, 3),
            (64, 64),
            (100, 7),
            (256, 256),
            (257, 255),
            (1000, 300),
            (3000, 40),
            (4096, 9),
            (513, 513),
        ] {
            let (a, b) = (words.take(x), words.take(y));
            // Products add into whatever the output already holds.
            let base = words.take(x + y);
            let mut out = base.clone();
            out.push(0);
            ntt::multiply_into(&mut ctx, &mut out, &a, &b).unwrap();
            let mut expected = schoolbook(&a, &b);
            expected.resize(x + y + 1, 0);
            add_into(&mut ctx, &mut expected, &base).unwrap();
            assert_eq!(trimmed(out), trimmed(expected), "{x} x {y}");
            let mut out = vec![0; 2 * x];
            ntt::multiply_into(&mut ctx, &mut out, &a, &a).unwrap();
            assert_eq!(trimmed(out), schoolbook(&a, &a), "{x} squared");
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn reciprocal_division_agrees_with_schoolbook_division() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = None;
        let mut words = Words(0x3c6e_f372_fe94_f82b);
        // Divisor and quotient lengths from a few words to beyond the
        // transform and division thresholds, with quotients shorter than,
        // as long as and longer than their divisors.
        let (small, long) = (division::THRESHOLD, division::LONG);
        for (y, q) in [
            (2, 1),
            (2, 5),
            (3, 2),
            (5, 5),
            (7, 20),
            (20, 7),
            (130, 129),
            (129, 400),
            (300, 299),
            (300, 301),
            (small, long),
            (long, small),
            (long + 1, small - 1),
            (small - 1, long + 1),
            (1100, 300),
            (2048, 2048),
            (600, 2500),
        ] {
            for shape in 0..4 {
                let mut b = words.take(y);
                match shape {
                    // The largest normalizing shift.
                    0 => b[y - 1] = 1,
                    // All ones.
                    1 => b.fill(u32::MAX),
                    // A power of 2^32.
                    2 => {
                        b.fill(0);
                        b[y - 1] = 1;
                    }
                    _ => {}
                }
                let quotient = words.take(q);
                let product =
                    multiply(&mut ctx, Magnitude::Words(&quotient), Magnitude::Words(&b)).unwrap();
                for remainder in [Vec::new(), trimmed(words.take(y - 1)), {
                    // The largest remainder, one below the divisor.
                    let mut largest = b.clone();
                    subtract(&mut ctx, &mut largest, Magnitude::Small(1)).unwrap();
                    trimmed(largest)
                }] {
                    let mut dividend = product.data.clone();
                    dividend.push(0);
                    add_into(&mut ctx, &mut dividend, &remainder).unwrap();
                    let dividend = trimmed(dividend);
                    let (fast, rest) = division::divide(&mut ctx, &dividend, &b).unwrap();
                    assert_eq!(trimmed(fast.data), quotient, "{y}, {q}, {shape}");
                    assert_eq!(rest.data, remainder, "{y}, {q}, {shape}");
                    let (dispatched, rest) =
                        divide(&mut ctx, Magnitude::Words(&dividend), Magnitude::Words(&b))
                            .unwrap();
                    assert_eq!(trimmed(dispatched.data), quotient, "{y}, {q}, {shape}");
                    assert_eq!(rest.data, remainder, "{y}, {q}, {shape}");
                }
            }
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn reciprocal_division_charges_its_work_and_releases_it_on_failure() {
        let mut words = Words(0x9b05_688c_2b3e_6c1f);
        let (a, b) = (words.take(6000), words.take(2500));
        let mut ctx = unlimited_context();
        ctx.options.limits.memory_bytes = Some(1 << 30);
        let (quotient, remainder) =
            divide(&mut ctx, Magnitude::Words(&a), Magnitude::Words(&b)).unwrap();
        let (steps, peak) = (ctx.stats().steps, ctx.stats().peak_memory_bytes);
        let (expected, rest) = long_divide(&mut unlimited_context(), &a, &b).unwrap();
        assert_eq!(trimmed(quotient.data), trimmed(expected.data));
        assert_eq!(remainder.data, rest.data);
        for kind in [ErrorKind::Steps, ErrorKind::Memory, ErrorKind::Cancelled] {
            let mut ctx = CallContext::new(CallOptions::default());
            match kind {
                ErrorKind::Steps => ctx.options.limits.steps = Some(steps / 4),
                ErrorKind::Memory => ctx.options.limits.memory_bytes = Some(peak / 4),
                _ => ctx.cancellation().cancel(),
            }
            let error = divide(&mut ctx, Magnitude::Words(&a), Magnitude::Words(&b)).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
    }

    #[test]
    fn digit_conversions_round_trip_in_every_radix() {
        let mut ctx = CallContext::new(CallOptions {
            limits: crate::Limits {
                steps: None,
                ..crate::Limits::default()
            },
            ..CallOptions::default()
        });
        let mut words = Words(0x2545_f491_4f6c_dd1d);
        for radix in 2..=36 {
            // Long strings in a few radixes reach the transformed powers.
            let lengths: &[usize] = if matches!(radix, 3 | 10 | 36) {
                &[3, 33, 70, 150, 700, 2100]
            } else {
                &[3, 33, 70, 150]
            };
            for &length in lengths {
                let digits = (0..length * 12)
                    .map(|i| {
                        let digit = if i == 0 {
                            1 + words.next() % (radix - 1)
                        } else {
                            words.next() % radix
                        };
                        char::from_digit(digit, radix).unwrap() as u8
                    })
                    .collect::<Vec<_>>();
                let value = parse(&mut ctx, &digits, radix).unwrap();
                let text = format(&mut ctx, &value, radix).unwrap();
                assert_eq!(text.data, digits, "radix {radix}");
                // The reference path: one digit at a time.
                let mut expected = Vec::<u32>::new();
                for &byte in &digits {
                    let mut carry = (byte as char).to_digit(radix).unwrap() as u64;
                    for word in &mut expected {
                        carry += *word as u64 * radix as u64;
                        *word = carry as u32;
                        carry >>= 32;
                    }
                    if carry != 0 {
                        expected.push(carry as u32);
                    }
                }
                let (_, magnitude) = parts(&value);
                let actual = (0..magnitude.len())
                    .map(|i| magnitude.word(i))
                    .collect::<Vec<_>>();
                assert_eq!(actual, expected, "radix {radix}");
                let negative = negate(&mut ctx, &value, false).unwrap();
                let text = format(&mut ctx, &negative, radix).unwrap();
                assert_eq!(text.data[0], b'-');
                assert_eq!(&text.data[1..], digits);
            }
        }
        // Zero chunks inside and at the ends convert without losing padding.
        for text in [
            "1".to_owned() + &"0".repeat(5000),
            "9".repeat(3000) + &"0".repeat(3000) + "7",
        ] {
            let value = parse(&mut ctx, text.as_bytes(), 10).unwrap();
            assert_eq!(format(&mut ctx, &value, 10).unwrap().data, text.as_bytes());
        }
    }

    #[test]
    fn powers_of_two_raise_to_a_single_bit() {
        let mut ctx = CallContext::new(CallOptions::default());
        for (base, exponent) in [
            (2, 64),
            (-2, 65),
            (-2, 64),
            (8, 100),
            (1 << 40, 3),
            (i64::MIN, 3),
        ] {
            let fast = binary(&mut ctx, "**", &Value::int(base), &Value::int(exponent)).unwrap();
            let mut slow = Value::int(1);
            for _ in 0..exponent {
                slow = binary(&mut ctx, "*", &slow, &Value::int(base)).unwrap();
            }
            assert_eq!(
                compare(&mut ctx, &fast, &slow).unwrap(),
                Ordering::Equal,
                "{base}**{exponent}"
            );
        }
    }

    /// The quadratic conversion: one short division per chunk of digits.
    fn reference_digits(words: &[u32], radix: u32) -> Vec<u8> {
        let (mut power, mut width) = (radix, 1);
        while let Some(next) = power.checked_mul(radix) {
            power = next;
            width += 1;
        }
        let mut words = trimmed(words.to_vec());
        let mut digits = Vec::new();
        while !words.is_empty() {
            let mut remainder = 0u64;
            for word in words.iter_mut().rev() {
                let value = remainder << 32 | *word as u64;
                *word = (value / power as u64) as u32;
                remainder = value % power as u64;
            }
            words = trimmed(words);
            for _ in 0..width {
                digits.push(char::from_digit(remainder as u32 % radix, radix).unwrap() as u8);
                remainder /= radix as u64;
                if words.is_empty() && remainder == 0 {
                    break;
                }
            }
        }
        if digits.is_empty() {
            digits.push(b'0');
        }
        digits.reverse();
        digits
    }

    fn big(words: Vec<u32>) -> Value {
        let mut ctx = unlimited_context();
        finish(&mut ctx, false, Buffer::untracked(words)).unwrap()
    }

    fn words_of(value: &Value) -> Vec<u32> {
        parts(value).1.slice(&mut [0; 2]).to_vec()
    }

    #[test]
    fn divided_conversions_agree_with_short_division_in_every_radix() {
        let mut ctx = unlimited_context();
        let mut words = Words(0x0bad_5eed_c0ff_ee00);
        // Word lengths around the direct cutoff, leaf multiples of 13, 19,
        // 20 and 23 words, and powers of two.
        let lengths = [
            33, 34, 38, 39, 40, 46, 47, 60, 64, 65, 76, 77, 92, 93, 128, 129, 184, 185, 304, 305,
            368, 369, 511, 512, 513, 736, 737,
        ];
        for radix in (3..=36).filter(|radix: &u32| !radix.is_power_of_two()) {
            let lengths: &[usize] = if radix == 10 {
                &lengths
            } else {
                &lengths[..lengths.len() - 8]
            };
            for &length in lengths {
                let mut shapes = vec![words.take(length), vec![u32::MAX; length], {
                    let mut single = vec![0; length];
                    single[length - 1] = 1;
                    single
                }];
                // Powers of the radix and their neighbours carry through
                // every limb.
                let digits = (length * 32) as f64 / (radix as f64).log2();
                let power = [b"1".as_slice(), &vec![b'0'; digits as usize - 1]].concat();
                let power = parse(&mut ctx, &power, radix).unwrap();
                for delta in [-1, 0, 1] {
                    let value = binary(&mut ctx, "+", &power, &Value::int(delta)).unwrap();
                    shapes.push(words_of(&value));
                }
                for shape in shapes {
                    let expected = reference_digits(&shape, radix);
                    let value = big(shape);
                    let text = format(&mut ctx, &value, radix).unwrap();
                    assert_eq!(text.data, expected, "radix {radix}, {length} words");
                    let negative = negate(&mut ctx, &value, false).unwrap();
                    let text = format(&mut ctx, &negative, radix).unwrap();
                    assert_eq!(text.data[0], b'-');
                    assert_eq!(&text.data[1..], expected, "radix {radix}, {length} words");
                }
            }
        }
    }

    #[test]
    fn divided_conversions_charge_their_work_and_release_it_on_failure() {
        let mut words = Words(0x1357_9bdf_2468_ace0);
        let value = big(words.take(4096));
        let expected = reference_digits(&words_of(&value), 10);
        let mut ctx = unlimited_context();
        ctx.options.limits.memory_bytes = Some(1 << 30);
        let text = format(&mut ctx, &value, 10).unwrap();
        assert_eq!(text.data, expected);
        let (steps, peak) = (ctx.stats().steps, ctx.stats().peak_memory_bytes);
        drop(text);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        // Scratch stays within a small multiple of the digits.
        assert!(peak < 8 * expected.len(), "{peak} bytes");
        for kind in [ErrorKind::Steps, ErrorKind::Memory, ErrorKind::Cancelled] {
            let mut ctx = CallContext::new(CallOptions::default());
            match kind {
                ErrorKind::Steps => ctx.options.limits.steps = Some(steps / 2),
                ErrorKind::Memory => ctx.options.limits.memory_bytes = Some(peak / 2),
                _ => ctx.cancellation().cancel(),
            }
            let error = format(&mut ctx, &value, 10).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
    }

    #[test]
    fn long_conversions_stay_subquadratic_and_observe_limits() {
        let cost = |digits: usize| {
            let mut ctx = CallContext::new(CallOptions::default());
            ctx.options.limits.steps = None;
            let text = "7".repeat(digits);
            parse(&mut ctx, text.as_bytes(), 10).unwrap();
            ctx.stats().steps
        };
        let (small, large) = (cost(20_000), cost(40_000));
        // Quadrupling would be the schoolbook conversion; Karatsuba gives about three.
        assert!(large < small * 7 / 2, "{small} then {large}");
        // A 20,000-digit literal parses within the default quota.
        assert!(small < 250_000);
        let text = "7".repeat(20_000);
        for kind in [ErrorKind::Steps, ErrorKind::Memory, ErrorKind::Cancelled] {
            let mut ctx = CallContext::new(CallOptions::default());
            match kind {
                ErrorKind::Steps => ctx.options.limits.steps = Some(50_000),
                ErrorKind::Memory => ctx.options.limits.memory_bytes = Some(16_384),
                _ => ctx.cancellation().cancel(),
            }
            let error = parse(&mut ctx, text.as_bytes(), 10).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        let value = parse(&mut ctx, text.as_bytes(), 10).unwrap();
        let retained = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(retained + 30_000);
        assert_eq!(
            format(&mut ctx, &value, 10).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, retained);
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
