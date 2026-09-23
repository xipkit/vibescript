//! Number-theoretic transforms modulo the prime p = 2^64 - 2^32 + 1, which has
//! roots of unity of every power-of-two order up to 2^32 and reduces without
//! division. Convolving the coefficients of two long integers through these
//! transforms costs O(n log n) word operations instead of Karatsuba's
//! O(n^1.59). Every butterfly, pointwise product and packing step counts as
//! one word operation, charged like the schoolbook loops at one step per
//! sixteen.

use crate::{CallContext, Error, ErrorKind, Result, budget::Buffer};

/// The prime modulus.
const P: u64 = 0xffff_ffff_0000_0001;
/// 2^64 mod P.
const EPSILON: u64 = 0xffff_ffff;
/// A generator of the multiplicative group mod P.
const GENERATOR: u64 = 7;
/// The largest transform: P - 1 is divisible by 2^32.
const LARGEST: usize = 1 << 32;

fn add(a: u64, b: u64) -> u64 {
    let (sum, over) = a.overflowing_add(b);
    let (reduced, under) = sum.overflowing_sub(P);
    if over || !under { reduced } else { sum }
}

fn sub(a: u64, b: u64) -> u64 {
    let (difference, under) = a.overflowing_sub(b);
    if under {
        difference.wrapping_add(P)
    } else {
        difference
    }
}

pub(super) fn mul(a: u64, b: u64) -> u64 {
    reduce(a as u128 * b as u128)
}

/// Reduces a 128-bit value, using 2^64 = 2^32 - 1 and 2^96 = -1 (mod P).
fn reduce(value: u128) -> u64 {
    let low = value as u64;
    let high = (value >> 64) as u64;
    let (mut t, under) = low.overflowing_sub(high >> 32);
    if under {
        // The wrapped difference exceeds 2^64 - 2^32, so this cannot underflow.
        t = t.wrapping_sub(EPSILON);
    }
    let (mut r, over) = t.overflowing_add((high & EPSILON) * EPSILON);
    if over {
        // The wrapped sum is at most 2^64 - 2^33, so this stays below P.
        r = r.wrapping_add(EPSILON);
    }
    if r >= P { r - P } else { r }
}

fn power(mut base: u64, mut exponent: u64) -> u64 {
    let mut result = 1;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = mul(result, base);
        }
        base = mul(base, base);
        exponent >>= 1;
    }
    result
}

/// Returns the inverse of a power-of-two transform size: `size` times
/// (P - 1) / size is -1.
pub(super) fn inverse_size(size: usize) -> u64 {
    P - (P - 1) / size as u64
}

/// Returns the smallest supported transform size holding `length` values.
pub(super) fn size_for(length: usize) -> Result<usize> {
    let size = length.max(2).next_power_of_two();
    if size > LARGEST {
        return Err(Error::new(ErrorKind::Memory, "integer size overflow"));
    }
    Ok(size)
}

/// Returns the transform size and piece count for a convolution of `a` and
/// `b` coefficients whose longer factor is cut into pieces sharing the
/// shorter one's spectrum, choosing the size with the fewest word
/// operations.
pub(super) fn pieces(a: usize, b: usize) -> Result<(usize, usize)> {
    let (long, short) = (a.max(b), a.min(b));
    let largest = size_for(long + short - 1)?;
    let mut size = size_for(2 * short)?.min(largest);
    let mut best = (usize::MAX, size, 1);
    loop {
        let count = long.div_ceil(size + 1 - short);
        let cost = transform_cost(size) * count + spectrum_cost(size);
        if cost < best.0 {
            best = (cost, size, count);
        }
        if size >= largest {
            return Ok((best.1, best.2));
        }
        size *= 2;
    }
}

/// Word operations for one product against a cached spectrum: zeroing,
/// two transforms, the pointwise product and the carry pass.
pub(super) fn transform_cost(size: usize) -> usize {
    size * (size.trailing_zeros() as usize + 3)
}

/// Word operations for a scaled spectrum.
pub(super) fn spectrum_cost(size: usize) -> usize {
    size * (size.trailing_zeros() as usize / 2 + 2)
}

/// Counts word operations and charges one step per sixteen before they run,
/// carrying the remainder so that many short passes are not overcharged.
#[derive(Default)]
pub(super) struct Meter {
    pending: u64,
}

impl Meter {
    pub(super) fn spend(&mut self, ctx: &mut CallContext, operations: usize) -> Result<()> {
        self.pending += operations as u64;
        if self.pending >= 16 {
            ctx.charge(self.pending / 16)?;
            self.pending %= 16;
        }
        Ok(())
    }

    /// Charges the operations still pending.
    pub(super) fn settle(&mut self, ctx: &mut CallContext) -> Result<()> {
        if self.pending != 0 {
            self.pending = 0;
            ctx.charge(1)?;
        }
        Ok(())
    }
}

/// Twiddle factors for every transform size up to the largest ensured so
/// far: entry `h + k` holds w^k for the root w of order 2h.
pub(super) struct Roots {
    table: Buffer<u64>,
}

impl Roots {
    pub(super) fn new() -> Self {
        Self {
            table: Buffer::empty(),
        }
    }

    /// Extends the table to cover transforms of `size` values.
    pub(super) fn ensure(
        &mut self,
        ctx: &mut CallContext,
        meter: &mut Meter,
        size: usize,
    ) -> Result<()> {
        if self.table.data.len() >= size {
            return Ok(());
        }
        self.table.ensure(ctx, size)?;
        if self.table.data.is_empty() {
            // Entry 0 is unused, so block h starts at index h.
            self.table.data.push(0);
        }
        while self.table.data.len() < size {
            let half = self.table.data.len();
            let root = power(GENERATOR, (P - 1) / (2 * half) as u64);
            meter.spend(ctx, half)?;
            let mut twiddle = 1;
            for _ in 0..half {
                self.table.data.push(twiddle);
                twiddle = mul(twiddle, root);
            }
        }
        Ok(())
    }

    fn block(&self, half: usize) -> &[u64] {
        &self.table.data[half..2 * half]
    }
}

/// Transforms `data`, whose length is a power of two, in place by decimation
/// in frequency. The spectrum comes out in bit-reversed order, which
/// [`inverse`] expects.
pub(super) fn forward(
    ctx: &mut CallContext,
    meter: &mut Meter,
    roots: &Roots,
    data: &mut [u64],
) -> Result<()> {
    let mut half = data.len() / 2;
    while half != 0 {
        meter.spend(ctx, data.len() / 2)?;
        let twiddles = roots.block(half);
        for block in data.chunks_exact_mut(2 * half) {
            let (low, high) = block.split_at_mut(half);
            for ((x, y), &w) in low.iter_mut().zip(high.iter_mut()).zip(twiddles) {
                let (a, b) = (*x, *y);
                *x = add(a, b);
                *y = mul(sub(a, b), w);
            }
        }
        half /= 2;
    }
    Ok(())
}

/// Inverts [`forward`] by decimation in time, taking a bit-reversed spectrum
/// to values in natural order, multiplied by the transform size.
pub(super) fn inverse(
    ctx: &mut CallContext,
    meter: &mut Meter,
    roots: &Roots,
    data: &mut [u64],
) -> Result<()> {
    let mut half = 1;
    while half < data.len() {
        meter.spend(ctx, data.len() / 2)?;
        let twiddles = roots.block(half);
        for block in data.chunks_exact_mut(2 * half) {
            let (low, high) = block.split_at_mut(half);
            let (a, b) = (low[0], high[0]);
            low[0] = add(a, b);
            high[0] = sub(a, b);
            // The inverse root w^-k is -w^(h-k), since w^h = -1.
            for ((x, y), &w) in low[1..]
                .iter_mut()
                .zip(high[1..].iter_mut())
                .zip(twiddles[1..].iter().rev())
            {
                let t = mul(*y, w);
                let a = *x;
                *x = sub(a, t);
                *y = add(a, t);
            }
        }
        half *= 2;
    }
    Ok(())
}

/// Multiplies `data` by `other` pointwise, and by `scale` unless it is one.
pub(super) fn pointwise(
    ctx: &mut CallContext,
    meter: &mut Meter,
    data: &mut [u64],
    other: &[u64],
    scale: u64,
) -> Result<()> {
    if scale == 1 {
        meter.spend(ctx, data.len())?;
        for (x, &y) in data.iter_mut().zip(other) {
            *x = mul(*x, y);
        }
    } else {
        meter.spend(ctx, 2 * data.len())?;
        for (x, &y) in data.iter_mut().zip(other) {
            *x = mul(mul(*x, y), scale);
        }
    }
    Ok(())
}

/// Returns a zeroed transform buffer of `size` values.
pub(super) fn zeroed(ctx: &mut CallContext, meter: &mut Meter, size: usize) -> Result<Buffer<u64>> {
    let mut data = Buffer::with_capacity(ctx, size)?;
    meter.spend(ctx, size)?;
    data.data.resize(size, 0);
    Ok(data)
}

/// Spreads words into 16-bit coefficients times `scale` at the start of
/// `data`, zeroing the rest.
fn spread(
    ctx: &mut CallContext,
    meter: &mut Meter,
    data: &mut [u64],
    words: &[u32],
    scale: u64,
) -> Result<()> {
    meter.spend(ctx, data.len())?;
    for (pair, &word) in data.chunks_exact_mut(2).zip(words) {
        pair[0] = (word & 0xffff) as u64;
        pair[1] = (word >> 16) as u64;
    }
    if scale != 1 {
        meter.spend(ctx, 2 * words.len())?;
        for value in &mut data[..2 * words.len()] {
            *value = mul(*value, scale);
        }
    }
    data[2 * words.len()..].fill(0);
    Ok(())
}

/// Adds the convolution of 16-bit coefficients in `data`, `length` words
/// long, into `out`, propagating carries through the rest of `out`.
fn accumulate(
    ctx: &mut CallContext,
    meter: &mut Meter,
    out: &mut [u32],
    data: &[u64],
    length: usize,
) -> Result<()> {
    meter.spend(ctx, length)?;
    let mut carry = 0u128;
    for (word, pair) in out.iter_mut().zip(data.chunks_exact(2)).take(length) {
        carry += pair[0] as u128 + ((pair[1] as u128) << 16) + *word as u128;
        *word = carry as u32;
        carry >>= 32;
    }
    let mut index = length;
    while carry != 0 {
        if (index - length) % 16 == 0 {
            meter.spend(ctx, 16)?;
        }
        carry += out[index] as u128;
        out[index] = carry as u32;
        carry >>= 32;
        index += 1;
    }
    Ok(())
}

/// Adds `a * b` into `out`, which must hold the sum. A long operand is cut
/// into pieces that each share a transform with the shorter one, whose
/// scaled spectrum is computed once.
pub(super) fn multiply_into(
    ctx: &mut CallContext,
    out: &mut [u32],
    a: &[u32],
    b: &[u32],
) -> Result<()> {
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };
    let square = a.as_ptr() == b.as_ptr() && a.len() == b.len();
    let mut meter = Meter::default();
    let mut roots = Roots::new();
    let (size, _) = pieces(2 * a.len(), 2 * b.len())?;
    let piece = size / 2 - b.len();
    roots.ensure(ctx, &mut meter, size)?;
    let mut spectrum = zeroed(ctx, &mut meter, size)?;
    spread(ctx, &mut meter, &mut spectrum.data, b, inverse_size(size))?;
    forward(ctx, &mut meter, &roots, &mut spectrum.data)?;
    if square {
        // The spectrum carries one factor of 1/size; the square needs one.
        meter.spend(ctx, 2 * size)?;
        for value in &mut spectrum.data {
            *value = mul(mul(*value, *value), size as u64);
        }
        inverse(ctx, &mut meter, &roots, &mut spectrum.data)?;
        accumulate(ctx, &mut meter, out, &spectrum.data, 2 * a.len())?;
        return meter.settle(ctx);
    }
    let mut data = zeroed(ctx, &mut meter, size)?;
    for (index, chunk) in a.chunks(piece).enumerate() {
        spread(ctx, &mut meter, &mut data.data, chunk, 1)?;
        forward(ctx, &mut meter, &roots, &mut data.data)?;
        pointwise(ctx, &mut meter, &mut data.data, &spectrum.data, 1)?;
        inverse(ctx, &mut meter, &roots, &mut data.data)?;
        accumulate(
            ctx,
            &mut meter,
            &mut out[index * piece..],
            &data.data,
            chunk.len() + b.len(),
        )?;
    }
    meter.settle(ctx)
}

/// Returns `a * b` modulo β^words - 1, where β = 2^32, for operands of at
/// most `words` words and a power-of-two `words`, as `words` words: the
/// cyclic convolution of their 16-bit coefficients, whose carry out of the
/// top word wraps around to the bottom since β^words is one.
pub(super) fn multiply_cyclic(
    ctx: &mut CallContext,
    a: &[u32],
    b: &[u32],
    words: usize,
) -> Result<Buffer<u32>> {
    let size = size_for(2 * words)?;
    let mut meter = Meter::default();
    let mut roots = Roots::new();
    roots.ensure(ctx, &mut meter, size)?;
    let mut spectrum = zeroed(ctx, &mut meter, size)?;
    spread(ctx, &mut meter, &mut spectrum.data, b, inverse_size(size))?;
    forward(ctx, &mut meter, &roots, &mut spectrum.data)?;
    let mut data = zeroed(ctx, &mut meter, size)?;
    spread(ctx, &mut meter, &mut data.data, a, 1)?;
    forward(ctx, &mut meter, &roots, &mut data.data)?;
    pointwise(ctx, &mut meter, &mut data.data, &spectrum.data, 1)?;
    drop(spectrum);
    inverse(ctx, &mut meter, &roots, &mut data.data)?;
    let mut out = Buffer::with_capacity(ctx, words)?;
    meter.spend(ctx, words)?;
    let mut carry = 0u128;
    for pair in data.data.chunks_exact(2) {
        carry += pair[0] as u128 + ((pair[1] as u128) << 16);
        out.data.push(carry as u32);
        carry >>= 32;
    }
    wrap(ctx, &mut meter, &mut out.data, carry)?;
    meter.settle(ctx)?;
    Ok(out)
}

/// Adds `carry` at the bottom of `words`, wrapping any carry out of the
/// top back to the bottom, as arithmetic modulo β^len - 1 does.
pub(super) fn wrap(
    ctx: &mut CallContext,
    meter: &mut Meter,
    words: &mut [u32],
    mut carry: u128,
) -> Result<()> {
    while carry != 0 {
        for (i, word) in words.iter_mut().enumerate() {
            if i % 16 == 0 {
                meter.spend(ctx, 16)?;
            }
            carry += *word as u128;
            *word = carry as u32;
            carry >>= 32;
            if carry == 0 {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn field_arithmetic_matches_wide_reference() {
        let p = P as u128;
        let samples = [
            0,
            1,
            2,
            EPSILON,
            EPSILON + 1,
            P - 1,
            P - 2,
            P - EPSILON,
            1 << 32,
            (1 << 63) + 12345,
            0x1234_5678_9abc_def0,
            0xffff_fffe_ffff_ffff,
        ];
        for &a in &samples {
            for &b in &samples {
                assert_eq!(add(a, b) as u128, (a as u128 + b as u128) % p);
                assert_eq!(sub(a, b) as u128, (a as u128 + p - b as u128) % p);
                assert_eq!(mul(a, b) as u128, (a as u128 * b as u128) % p);
            }
        }
        for high in [0u128, 1, EPSILON as u128, u64::MAX as u128] {
            for low in [0u128, 1, P as u128 - 1, u64::MAX as u128] {
                let value = high << 64 | low;
                assert_eq!(reduce(value) as u128, value % p);
            }
        }
        // 7 generates the group, so its 2^32-nd root of unity has exact order.
        let root = power(GENERATOR, (P - 1) >> 32);
        assert_eq!(power(root, 1 << 31), P - 1);
        assert_eq!(mul(inverse_size(1 << 20), 1 << 20), 1);
    }

    #[test]
    fn transforms_convolve_like_schoolbook() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut meter = Meter::default();
        let mut roots = Roots::new();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for size in [2usize, 4, 8, 64, 1024] {
            roots.ensure(&mut ctx, &mut meter, size).unwrap();
            let a: Vec<u64> = (0..size / 2).map(|_| next() % P).collect();
            let b: Vec<u64> = (0..size / 2).map(|_| next() % P).collect();
            let mut x = a.clone();
            x.resize(size, 0);
            let mut y = b.clone();
            y.resize(size, 0);
            forward(&mut ctx, &mut meter, &roots, &mut x).unwrap();
            forward(&mut ctx, &mut meter, &roots, &mut y).unwrap();
            pointwise(&mut ctx, &mut meter, &mut x, &y, inverse_size(size)).unwrap();
            inverse(&mut ctx, &mut meter, &roots, &mut x).unwrap();
            let mut expected = vec![0u64; size];
            for (i, &u) in a.iter().enumerate() {
                for (j, &v) in b.iter().enumerate() {
                    expected[i + j] = add(expected[i + j], mul(u, v));
                }
            }
            assert_eq!(x, expected, "size {size}");
        }
    }
}
