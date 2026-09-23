//! Division of long integers through reciprocals. Newton's iteration on the
//! divisor's top half gives its reciprocal, and Barrett reduction turns each
//! quotient block into two products with it, so a division costs a constant
//! number of multiplications instead of a pass over the divisor for every
//! quotient word.

use super::{
    Magnitude, TRANSFORM, add_into, compare_magnitude, copy, long_divide, multiply,
    ntt::{self, Meter},
    shifted, significant, subtract, trim, work, zeros,
};
use crate::{CallContext, Result, budget::Buffer};
use std::cmp::Ordering;

/// Divisors and quotients shorter than this many words divide by the
/// schoolbook method.
pub(super) const THRESHOLD: usize = 256;
/// Reciprocals pay off once the longer of the divisor and the quotient has
/// this many words.
pub(super) const LONG: usize = 1024;
/// Estimates are provably within this many of the quotient; a block that
/// needs more corrections divides by the schoolbook method instead.
const CORRECTIONS: usize = 8;

/// Divides `a` by `b`, whose top word is nonzero and which is no longer
/// than `a`, returning the quotient and the trimmed remainder.
pub(super) fn divide(
    ctx: &mut CallContext,
    a: &[u32],
    b: &[u32],
) -> Result<(Buffer<u32>, Buffer<u32>)> {
    // Normalizing puts the divisor's top bit in its leading word; the
    // quotient is unchanged and the remainder shifts back at the end.
    let shift = b[b.len() - 1].leading_zeros();
    let divisor = shifted(ctx, b, shift, b.len())?;
    let mut dividend = shifted(ctx, a, shift, a.len() + 1)?;
    trim(ctx, &mut dividend.data)?;
    let (quotient, remainder) = normalized(ctx, &dividend.data, &divisor.data)?;
    let mut out = zeros(ctx, remainder.data.len())?;
    for (i, word) in out.data.iter_mut().enumerate() {
        work(ctx, i, remainder.data.len())?;
        let wide = (remainder.data.get(i + 1).copied().unwrap_or(0) as u64) << 32
            | remainder.data[i] as u64;
        *word = (wide >> shift) as u32;
    }
    trim(ctx, &mut out.data)?;
    Ok((quotient, out))
}

/// Divides trimmed `a` by normalized `b`.
fn normalized(ctx: &mut CallContext, a: &[u32], b: &[u32]) -> Result<(Buffer<u32>, Buffer<u32>)> {
    let n = b.len();
    if compare_magnitude(ctx, Magnitude::Words(a), Magnitude::Words(b))? == Ordering::Less {
        return Ok((Buffer::empty(), copy(ctx, Magnitude::Words(a))?));
    }
    let length = a.len() - n + 1;
    if length < n {
        return truncated(ctx, a, b, length);
    }
    // Long division with n-word digits: the top digit, of the words left
    // over, divides by truncation, and each further digit comes from the
    // remainder so far and the next n words of the dividend.
    let blocks = (length - 1) / n;
    let head = &a[blocks * n..];
    let inverse = reciprocal(ctx, b)?;
    let (top, mut remainder) = if head.len() >= 2 * n - 1 {
        reduce(ctx, head, b, &inverse.data)?
    } else {
        truncated(ctx, head, b, head.len() + 1 - n)?
    };
    let mut quotient = zeros(ctx, blocks * n + top.data.len())?;
    for (i, &word) in top.data.iter().enumerate() {
        work(ctx, i, top.data.len())?;
        quotient.data[blocks * n + i] = word;
    }
    for index in (0..blocks).rev() {
        let block = &a[index * n..(index + 1) * n];
        let mut current = Buffer::with_capacity(ctx, n + remainder.data.len())?;
        current.extend(ctx, block)?;
        current.extend(ctx, &remainder.data)?;
        let (digit, rest) = reduce(ctx, &current.data, b, &inverse.data)?;
        for (i, &word) in digit.data.iter().enumerate() {
            work(ctx, i, digit.data.len())?;
            quotient.data[index * n + i] = word;
        }
        remainder = rest;
    }
    Ok((quotient, remainder))
}

/// Divides trimmed `a` by normalized `b` for a quotient of at most
/// `length` words, no more than `b` has. The quotient of the top words of
/// both, one more than the quotient's, is within one of the true quotient.
fn truncated(
    ctx: &mut CallContext,
    a: &[u32],
    b: &[u32],
    length: usize,
) -> Result<(Buffer<u32>, Buffer<u32>)> {
    let drop = b.len() - (length + 1).min(b.len());
    let (top, head) = (&a[drop..], &b[drop..]);
    let inverse = reciprocal(ctx, head)?;
    let mut quotient = estimate(ctx, top, head, &inverse.data)?;
    // The estimate may exceed the quotient by one only when words were
    // dropped; starting one lower keeps the remainder nonnegative.
    if drop != 0 && !significant(&quotient.data).is_empty() {
        subtract(ctx, &mut quotient.data, Magnitude::Small(1))?;
    }
    correct(ctx, a, b, quotient)
}

/// Returns the quotient and remainder of `a < β^t b` by a normalized
/// t-word `b`, given `b`'s reciprocal.
fn reduce(
    ctx: &mut CallContext,
    a: &[u32],
    b: &[u32],
    inverse: &[u32],
) -> Result<(Buffer<u32>, Buffer<u32>)> {
    let quotient = estimate(ctx, a, b, inverse)?;
    correct(ctx, significant(a), b, quotient)
}

/// Returns Barrett's estimate of the quotient of `a < β^t b` by a
/// normalized t-word `b`, from `b`'s reciprocal: at most three below it,
/// since the reciprocal is at most two below β^(2t) / b.
fn estimate(ctx: &mut CallContext, a: &[u32], b: &[u32], inverse: &[u32]) -> Result<Buffer<u32>> {
    let t = b.len();
    let top = significant(a).get(t - 1..).unwrap_or_default();
    let estimate = multiply(ctx, Magnitude::Words(top), Magnitude::Words(inverse))?;
    let estimate = estimate.data.get(t + 1..).unwrap_or_default();
    copy(ctx, Magnitude::Words(significant(estimate)))
}

/// Completes a quotient estimate at most a few below the true quotient of
/// trimmed `a` by `b`, returning the quotient and the remainder.
fn correct(
    ctx: &mut CallContext,
    a: &[u32],
    b: &[u32],
    mut quotient: Buffer<u32>,
) -> Result<(Buffer<u32>, Buffer<u32>)> {
    let mut remainder = remainder(ctx, a, b, significant(&quotient.data))?;
    trim(ctx, &mut remainder.data)?;
    quotient.push(ctx, 0)?;
    for _ in 0..CORRECTIONS {
        if compare_magnitude(ctx, Magnitude::Words(&remainder.data), Magnitude::Words(b))?
            == Ordering::Less
        {
            trim(ctx, &mut quotient.data)?;
            return Ok((quotient, remainder));
        }
        subtract(ctx, &mut remainder.data, Magnitude::Words(b))?;
        trim(ctx, &mut remainder.data)?;
        add_into(ctx, &mut quotient.data, &[1])?;
    }
    debug_assert!(false, "a quotient estimate was too low");
    let (mut quotient, remainder) = long_divide(ctx, a, b)?;
    trim(ctx, &mut quotient.data)?;
    Ok((quotient, remainder))
}

/// Returns `a - q b` for a quotient estimate `q` at most a few below the
/// quotient of trimmed `a` by `b`. The difference is below β^(n + 1) for
/// an n-word `b`, so when the product is long it is computed modulo
/// β^L - 1 for a power-of-two L of at least n + 2 words, whose cyclic
/// product is about half as long as the whole one.
fn remainder(ctx: &mut CallContext, a: &[u32], b: &[u32], q: &[u32]) -> Result<Buffer<u32>> {
    let words = (b.len() + 2).next_power_of_two();
    if q.len().min(b.len()) < TRANSFORM || 2 * words >= ntt::size_for(2 * (q.len() + b.len()))? {
        let product = multiply(ctx, Magnitude::Words(q), Magnitude::Words(b))?;
        let mut remainder = copy(ctx, Magnitude::Words(a))?;
        subtract(
            ctx,
            &mut remainder.data,
            Magnitude::Words(significant(&product.data)),
        )?;
        return Ok(remainder);
    }
    let product = ntt::multiply_cyclic(ctx, q, b, words)?;
    let mut meter = Meter::default();
    // a is shorter than 2L words: fold its high part onto its low part.
    let mut remainder = zeros(ctx, words)?;
    let (low, high) = a.split_at(words.min(a.len()));
    meter.spend(ctx, low.len())?;
    remainder.data[..low.len()].copy_from_slice(low);
    add_wrapped(ctx, &mut meter, &mut remainder.data, high)?;
    subtract_wrapped(ctx, &mut meter, &mut remainder.data, &product.data)?;
    // β^L - 1 itself stands for zero.
    if remainder.data[0] == u32::MAX {
        meter.spend(ctx, words)?;
        if remainder.data.iter().all(|&word| word == u32::MAX) {
            remainder.data.fill(0);
        }
    }
    meter.settle(ctx)?;
    Ok(remainder)
}

/// Lowers `x`, a reciprocal of an n-word `a`'s top words, until
/// `a x < β^limit`, and returns β^limit - a x, which is then positive and
/// below 2 β^n. Being that small either way, it is exact modulo β^L - 1
/// for a power-of-two L of at least n + 2 words, whose cyclic product is
/// shorter than the whole one.
fn error(ctx: &mut CallContext, a: &[u32], x: &mut [u32], limit: usize) -> Result<Buffer<u32>> {
    let n = a.len();
    let words = (n + 2).next_power_of_two();
    if n.min(x.len()) >= TRANSFORM && 2 * words < ntt::size_for(2 * (n + x.len()))? {
        let product = ntt::multiply_cyclic(ctx, a, x, words)?;
        let mut meter = Meter::default();
        let mut error = zeros(ctx, words)?;
        error.data[limit % words] = 1;
        subtract_wrapped(ctx, &mut meter, &mut error.data, &product.data)?;
        for _ in 0..CORRECTIONS {
            let length = significant(&error.data).len();
            if length != 0 && length <= n + 1 {
                meter.settle(ctx)?;
                return Ok(error);
            }
            // The error is zero or negative: a x is at least β^limit.
            add_wrapped(ctx, &mut meter, &mut error.data, a)?;
            subtract(ctx, x, Magnitude::Small(1))?;
        }
        debug_assert!(false, "a reciprocal was too far above");
    }
    let mut t = multiply(ctx, Magnitude::Words(a), Magnitude::Words(x))?;
    for _ in 0..CORRECTIONS {
        if significant(&t.data).len() <= limit {
            break;
        }
        subtract(ctx, &mut t.data, Magnitude::Words(a))?;
        subtract(ctx, x, Magnitude::Small(1))?;
    }
    debug_assert!(significant(&t.data).len() <= limit);
    t.data.truncate(limit);
    let mut carry = 1u64;
    for (i, word) in t.data.iter_mut().enumerate() {
        work(ctx, i, limit)?;
        carry += !*word as u64;
        *word = carry as u32;
        carry >>= 32;
    }
    Ok(t)
}

/// Subtracts `b` from `a` modulo β^len - 1, where `a` holds len words and
/// `b` at most as many: a borrow out of the top takes one from the bottom.
fn subtract_wrapped(
    ctx: &mut CallContext,
    meter: &mut Meter,
    a: &mut [u32],
    b: &[u32],
) -> Result<()> {
    let mut borrow = 0u64;
    meter.spend(ctx, a.len())?;
    for (i, word) in a.iter_mut().enumerate() {
        let sub = b.get(i).copied().unwrap_or(0) as u64 + borrow;
        borrow = u64::from((*word as u64) < sub);
        *word = (*word as u64).wrapping_sub(sub) as u32;
    }
    if borrow != 0 {
        // The wrapped difference is at least one.
        subtract(ctx, a, Magnitude::Small(1))?;
    }
    Ok(())
}

/// Adds `b` into `a` modulo β^len - 1, where `a` holds len words and `b`
/// at most as many.
fn add_wrapped(ctx: &mut CallContext, meter: &mut Meter, a: &mut [u32], b: &[u32]) -> Result<()> {
    let mut carry = 0u64;
    meter.spend(ctx, a.len())?;
    for (i, word) in a.iter_mut().enumerate() {
        carry += *word as u64 + b.get(i).copied().unwrap_or(0) as u64;
        *word = carry as u32;
        carry >>= 32;
    }
    ntt::wrap(ctx, meter, a, carry as u128)
}

/// Returns X = β^n + x, with x below β^n, such that A X < β^(2n) <=
/// A (X + 2) for an n-word A whose top bit is set, where β = 2^32. The
/// top half's reciprocal, found recursively, is refined by one Newton step
/// (Brent and Zimmermann, Modern Computer Arithmetic, algorithm 3.5).
fn reciprocal(ctx: &mut CallContext, a: &[u32]) -> Result<Buffer<u32>> {
    let n = a.len();
    if n <= 2 {
        // X is the ceiling of β^(2n) / A, less one.
        let value = Magnitude::Words(a).window(0) as u128;
        let x = if n == 1 {
            u64::MAX as u128 / value
        } else {
            u128::MAX / value
        };
        let mut out = zeros(ctx, n + 1)?;
        for (i, word) in out.data.iter_mut().enumerate() {
            *word = (x >> (32 * i)) as u32;
        }
        return Ok(out);
    }
    let low = (n - 1) / 2;
    let high = n - low;
    let mut x = reciprocal(ctx, &a[low..])?;
    let error = error(ctx, a, &mut x.data, n + high)?;
    let u = multiply(
        ctx,
        Magnitude::Words(significant(error.data.get(low..).unwrap_or_default())),
        Magnitude::Words(&x.data),
    )?;
    let correction = u.data.get(2 * high - low..).unwrap_or_default();
    let mut out = zeros(ctx, n + 2)?;
    for (i, &word) in x.data.iter().enumerate() {
        work(ctx, i, x.data.len())?;
        out.data[low + i] = word;
    }
    add_into(ctx, &mut out.data, significant(correction))?;
    trim(ctx, &mut out.data)?;
    debug_assert_eq!(out.data.len(), n + 1);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integer::unlimited_context;

    #[test]
    fn reciprocals_are_within_two_of_the_quotient_of_a_power() {
        let mut ctx = unlimited_context();
        let mut seed = 0x6a09_e667_f3bc_c908u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 16) as u32
        };
        for n in (1..40).chain([
            63, 64, 65, 127, 128, 129, 255, 256, 300, 513, 700, 1000, 1500, 3000,
        ]) {
            let shapes: [Vec<u32>; 4] = [
                (0..n).map(|_| next()).collect(),
                vec![u32::MAX; n],
                (0..n)
                    .map(|i| if i + 1 == n { 1 << 31 } else { 0 })
                    .collect(),
                (0..n)
                    .map(|i| if i + 1 == n { 1 << 31 } else { u32::MAX })
                    .collect(),
            ];
            for mut a in shapes {
                a[n - 1] |= 1 << 31;
                let x = reciprocal(&mut ctx, &a).unwrap();
                assert_eq!(x.data.len(), n + 1, "{n} words");
                // A X < β^(2n) <= A (X + 2).
                let product =
                    multiply(&mut ctx, Magnitude::Words(&a), Magnitude::Words(&x.data)).unwrap();
                assert!(significant(&product.data).len() <= 2 * n, "{n} words");
                let mut next = x.data.clone();
                next.push(0);
                add_into(&mut ctx, &mut next, &[2]).unwrap();
                let product =
                    multiply(&mut ctx, Magnitude::Words(&a), Magnitude::Words(&next)).unwrap();
                assert!(significant(&product.data).len() > 2 * n, "{n} words");
            }
        }
    }
}
