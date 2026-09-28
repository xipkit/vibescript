//! Conversions between words and digit strings that stay subquadratic in the
//! number of digits: power-of-two radixes move bits directly, and parsing in
//! other radixes splits the digits in halves around a power of the radix.

use super::{Magnitude, multiply, trim, work};
use crate::{
    CallContext, Result,
    budget::{Buffer, CHUNK},
};

/// Values at most this many words or digit chunks long convert directly.
pub(super) const DIRECT: usize = 32;

/// Returns how many digits fit in a word and the radix raised to that count.
fn chunk(radix: u32) -> (usize, u32) {
    let mut base = radix;
    let mut width = 1;
    while let Some(next) = base.checked_mul(radix) {
        base = next;
        width += 1;
    }
    (width, base)
}

fn digit(byte: u8, radix: u32) -> u32 {
    (byte as char).to_digit(radix).unwrap()
}

/// Charges `length` bytes of digit work in chunks as `index` reaches each one.
fn digit_work(ctx: &mut CallContext, index: usize, length: usize) -> Result<()> {
    if index % CHUNK == 0 {
        ctx.work_bytes((length - index).min(CHUNK))?;
    }
    Ok(())
}

/// Powers of a chunk base: entry `k` is `base^(2^k)`, each the square of the last.
struct Powers {
    base: u32,
    table: Buffer<Buffer<u32>>,
}

impl Powers {
    fn new(base: u32) -> Self {
        Self {
            base,
            table: Buffer::empty(),
        }
    }

    fn get(&mut self, ctx: &mut CallContext, level: usize) -> Result<&[u32]> {
        while self.table.data.len() <= level {
            let next = match self.table.data.last() {
                None => {
                    let mut words = Buffer::with_capacity(ctx, 1)?;
                    words.data.push(self.base);
                    words
                }
                Some(last) => {
                    let mut square = multiply(
                        ctx,
                        Magnitude::Words(&last.data),
                        Magnitude::Words(&last.data),
                    )?;
                    trim(ctx, &mut square.data)?;
                    square
                }
            };
            self.table.push(ctx, next)?;
        }
        Ok(&self.table.data[level].data)
    }
}

/// Converts validated digits, most significant first, to trimmed words.
pub(super) fn parse(ctx: &mut CallContext, digits: &[u8], radix: u32) -> Result<Buffer<u32>> {
    if radix.is_power_of_two() {
        return parse_bits(ctx, digits, radix.trailing_zeros());
    }
    let (width, base) = chunk(radix);
    // Chunk values, least significant first.
    let mut chunks = Buffer::with_capacity(ctx, digits.len().div_ceil(width))?;
    let mut end = digits.len();
    while end > 0 {
        let start = end.saturating_sub(width);
        work(ctx, chunks.data.len(), chunks.data.capacity())?;
        let value = digits[start..end]
            .iter()
            .fold(0, |value, &byte| value * radix + digit(byte, radix));
        chunks.data.push(value);
        end = start;
    }
    combine(ctx, &chunks.data, &mut Powers::new(base))
}

/// Returns the value of chunks, least significant first, in the powers' base.
fn combine(ctx: &mut CallContext, chunks: &[u32], powers: &mut Powers) -> Result<Buffer<u32>> {
    if chunks.len() <= DIRECT {
        let mut words = Buffer::with_capacity(ctx, chunks.len())?;
        for &value in chunks.iter().rev() {
            ctx.charge(1)?;
            let mut carry = value as u64;
            for word in &mut words.data {
                carry += *word as u64 * powers.base as u64;
                *word = carry as u32;
                carry >>= 32;
            }
            if carry != 0 {
                words.data.push(carry as u32);
            }
        }
        return Ok(words);
    }
    // The low half holds a power-of-two count of chunks, at least half of them.
    let level = (chunks.len() - 1).ilog2() as usize;
    let (low, high) = chunks.split_at(1 << level);
    let low = combine(ctx, low, powers)?;
    let high = combine(ctx, high, powers)?;
    if high.data.is_empty() {
        return Ok(low);
    }
    let power = powers.get(ctx, level)?;
    let mut words = multiply(ctx, Magnitude::Words(&high.data), Magnitude::Words(power))?;
    super::add_into(ctx, &mut words.data, &low.data)?;
    trim(ctx, &mut words.data)?;
    Ok(words)
}

/// Packs digits of a power-of-two radix straight into words.
fn parse_bits(ctx: &mut CallContext, digits: &[u8], bits: u32) -> Result<Buffer<u32>> {
    let total = digits.len() * bits as usize;
    let mut words = super::zeros(ctx, total.div_ceil(32))?;
    for (index, &byte) in digits.iter().rev().enumerate() {
        digit_work(ctx, index, digits.len())?;
        let position = index * bits as usize;
        let value = (digit(byte, 1 << bits) as u64) << (position % 32);
        words.data[position / 32] |= value as u32;
        if value >> 32 != 0 {
            words.data[position / 32 + 1] |= (value >> 32) as u32;
        }
    }
    trim(ctx, &mut words.data)?;
    Ok(words)
}

/// Appends the digits of a nonzero magnitude in a power-of-two radix,
/// most significant first, unpacked straight from its bits.
pub(super) fn format_bits(
    ctx: &mut CallContext,
    words: &[u32],
    bits: u32,
    out: &mut Buffer<u8>,
) -> Result<()> {
    let magnitude = Magnitude::Words(words);
    let count = magnitude.bits().div_ceil(bits as usize);
    out.ensure(ctx, out.data.len() + count)?;
    for index in (0..count).rev() {
        digit_work(ctx, count - 1 - index, count)?;
        let position = index * bits as usize;
        let value = (magnitude.window(position) & ((1 << bits) - 1)) as usize;
        out.data
            .push(b"0123456789abcdefghijklmnopqrstuvwxyz"[value]);
    }
    Ok(())
}
