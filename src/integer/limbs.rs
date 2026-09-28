//! Converts words to digits in a radix that is not a power of two by
//! dividing and conquering in the target radix. Chunks of words convert to
//! limbs, each a power of the radix, and neighboring chunks recombine as
//! `high * 2^(32 m) + low`, where 2^(32 m) itself is held in limbs and
//! squared from one level to the next. Only multiplication is needed, so the
//! digits are exact by construction, and with transform products the whole
//! conversion costs O(n log^2 n) word operations.

use super::ntt::{self, Meter, Roots};
use crate::{CallContext, Error, ErrorKind, Result, budget::Buffer};

/// Magnitudes longer than this many words convert by halves.
pub(super) const DIRECT: usize = 32;
/// Powers at least this many limbs long multiply through transforms.
const TRANSFORM: usize = 24;
/// Limb products and their column sums stay below this bound, so a
/// transform recovers them exactly and carries fit in a word.
const BOUND: u64 = 1 << 63;

/// A limb base: the largest power of a radix within some bit width.
trait Base: Copy {
    fn radix(self) -> u32;
    /// The number of digits in a limb.
    fn digits(self) -> usize;
    fn base(self) -> u64;
    /// Returns `value / base` and `value % base`.
    fn split(self, value: u64) -> (u64, u32);
}

/// Decimal limbs of `D` digits, divided by a constant.
#[derive(Clone, Copy)]
struct Decimal<const D: u32>;

impl<const D: u32> Base for Decimal<D> {
    fn radix(self) -> u32 {
        10
    }

    fn digits(self) -> usize {
        D as usize
    }

    fn base(self) -> u64 {
        10u64.pow(D)
    }

    fn split(self, value: u64) -> (u64, u32) {
        (value / 10u64.pow(D), (value % 10u64.pow(D)) as u32)
    }
}

/// Limbs of any other radix.
#[derive(Clone, Copy)]
struct Radix {
    radix: u32,
    digits: usize,
    base: u64,
}

impl Radix {
    /// Returns the largest power of `radix` no larger than `2^bits`.
    fn new(radix: u32, bits: u32) -> Self {
        let (mut base, mut digits) = (radix as u64, 1);
        while base * radix as u64 <= 1 << bits {
            base *= radix as u64;
            digits += 1;
        }
        Self {
            radix,
            digits,
            base,
        }
    }
}

impl Base for Radix {
    fn radix(self) -> u32 {
        self.radix
    }

    fn digits(self) -> usize {
        self.digits
    }

    fn base(self) -> u64 {
        self.base
    }

    fn split(self, value: u64) -> (u64, u32) {
        (value / self.base, (value % self.base) as u32)
    }
}

/// Appends the digits of a magnitude longer than [`DIRECT`] words, most
/// significant first, in a radix that is not a power of two. Limbs are as
/// wide as the magnitude's length allows, since wider limbs mean shorter
/// transforms.
pub(super) fn format(
    ctx: &mut CallContext,
    words: &[u32],
    radix: u32,
    out: &mut Buffer<u8>,
) -> Result<()> {
    let length = words.len();
    if radix == 10 {
        if fits(Decimal::<7>.base(), length) {
            return Converter::new(Decimal::<7>).run(ctx, words, out);
        }
        if fits(Decimal::<6>.base(), length) {
            return Converter::new(Decimal::<6>).run(ctx, words, out);
        }
    }
    for bits in [24, 20, 16] {
        let base = Radix::new(radix, bits);
        if fits(base.base, length) {
            return Converter::new(base).run(ctx, words, out);
        }
    }
    Err(Error::new(ErrorKind::Memory, "integer size overflow"))
}

/// Reports whether the column sums of a `words`-word magnitude's products
/// stay below [`BOUND`] in `base`. A product's shorter factor, whose length
/// bounds the terms in each sum, is at most half the limbs.
fn fits(base: u64, words: usize) -> bool {
    let limbs = words as u64 * 32 / base.ilog2() as u64 + 2;
    (limbs / 2 + 1).saturating_mul(base * base) < BOUND
}

/// A power of 2^32 in limbs, with its scaled spectrum once it is long.
struct Scale {
    limbs: Buffer<u32>,
    spectrum: Buffer<u64>,
}

struct Converter<B> {
    base: B,
    meter: Meter,
    roots: Roots,
}

impl<B: Base> Converter<B> {
    fn new(base: B) -> Self {
        Self {
            base,
            meter: Meter::default(),
            roots: Roots::new(),
        }
    }

    fn run(mut self, ctx: &mut CallContext, words: &[u32], out: &mut Buffer<u8>) -> Result<()> {
        // A leaf of as many words as a limb has whole bits is just below
        // 2^(32 m) with m < 32 limbs, so products of two chunks nearly fill
        // their power-of-two transforms at every level.
        let leaf = self.base.base().ilog2() as usize;
        let mut scale = {
            let mut one = [0u32; 32];
            one[leaf] = 1;
            let mut limbs = Buffer::with_capacity(ctx, self.width(leaf + 1))?;
            self.horner(ctx, &one[..=leaf], &mut limbs.data)?;
            Scale {
                limbs,
                spectrum: Buffer::empty(),
            }
        };
        // Every chunk is below the scale, so it fits the scale's width.
        let mut stride = scale.limbs.data.len();
        let mut count = words.len().div_ceil(leaf);
        let mut level = Buffer::with_capacity(ctx, count * stride)?;
        for chunk in words.chunks(leaf) {
            let start = level.data.len();
            self.horner(ctx, chunk, &mut level.data)?;
            self.pad(ctx, &mut level.data, start + stride)?;
        }
        while count > 1 {
            let pairs = count.div_ceil(2);
            // A lone product at the top shares no spectrum of the scale.
            if pairs > 1 && scale.limbs.data.len() >= TRANSFORM {
                self.transform(ctx, &mut scale)?;
            }
            // The next level's chunks fit the square of this scale.
            let next = if pairs > 1 {
                Some(self.square(ctx, &scale)?)
            } else {
                None
            };
            let width = next
                .as_ref()
                .map_or(2 * scale.limbs.data.len(), |next| next.limbs.data.len());
            let mut combined = Buffer::with_capacity(ctx, pairs * width)?;
            for pair in level.data.chunks(2 * stride) {
                let (low, high) = pair.split_at(stride.min(pair.len()));
                let high = self.significant(ctx, high)?;
                self.combine(ctx, &mut combined.data, low, high, &scale, width)?;
            }
            level = combined;
            stride = width;
            count = pairs;
            if let Some(next) = next {
                scale = next;
            }
        }
        let limbs = self.significant(ctx, &level.data)?;
        self.emit(ctx, limbs, out)?;
        self.meter.settle(ctx)
    }

    /// Returns an upper bound on the limbs of a value of `words` words.
    fn width(&self, words: usize) -> usize {
        words * 32 / self.base.base().ilog2() as usize + 1
    }

    /// Converts words, least significant first, by Horner's rule, appending
    /// the limbs to `limbs`.
    fn horner(&mut self, ctx: &mut CallContext, words: &[u32], limbs: &mut Vec<u32>) -> Result<()> {
        let start = limbs.len();
        for &word in words.iter().rev() {
            self.meter.spend(ctx, limbs.len() - start + 1)?;
            let mut carry = word as u64;
            for limb in &mut limbs[start..] {
                let (quotient, remainder) = self.base.split((*limb as u64) << 32 | carry);
                *limb = remainder;
                carry = quotient;
            }
            while carry != 0 {
                let (quotient, remainder) = self.base.split(carry);
                limbs.push(remainder);
                carry = quotient;
            }
        }
        Ok(())
    }

    /// Pads `limbs` with zeros to `length`.
    fn pad(&mut self, ctx: &mut CallContext, limbs: &mut Vec<u32>, length: usize) -> Result<()> {
        debug_assert!(limbs.len() <= length);
        self.meter.spend(ctx, length - limbs.len())?;
        limbs.resize(length, 0);
        Ok(())
    }

    /// Drops high zero limbs, charging the limbs examined.
    fn significant<'a>(&mut self, ctx: &mut CallContext, limbs: &'a [u32]) -> Result<&'a [u32]> {
        let length = limbs
            .iter()
            .rposition(|&limb| limb != 0)
            .map_or(0, |i| i + 1);
        self.meter.spend(ctx, limbs.len() - length + 1)?;
        Ok(&limbs[..length])
    }

    /// Returns the spectrum of `limbs` in a transform of `size` values,
    /// scaled by 1/size so that products invert without rescaling.
    fn spectrum(
        &mut self,
        ctx: &mut CallContext,
        limbs: &[u32],
        size: usize,
    ) -> Result<Buffer<u64>> {
        self.roots.ensure(ctx, &mut self.meter, size)?;
        let mut data = ntt::zeroed(ctx, &mut self.meter, size)?;
        self.meter.spend(ctx, limbs.len())?;
        let inverse = ntt::inverse_size(size);
        for (value, &limb) in data.data.iter_mut().zip(limbs) {
            *value = ntt::mul(limb as u64, inverse);
        }
        ntt::forward(ctx, &mut self.meter, &self.roots, &mut data.data)?;
        Ok(data)
    }

    /// Computes the spectrum of a long scale for its square and products.
    fn transform(&mut self, ctx: &mut CallContext, scale: &mut Scale) -> Result<()> {
        let size = ntt::size_for(2 * scale.limbs.data.len() - 1)?;
        scale.spectrum = self.spectrum(ctx, &scale.limbs.data, size)?;
        Ok(())
    }

    /// Returns the square of a scale.
    fn square(&mut self, ctx: &mut CallContext, scale: &Scale) -> Result<Scale> {
        let length = scale.limbs.data.len();
        let mut limbs = Buffer::with_capacity(ctx, 2 * length)?;
        let spectrum = &scale.spectrum.data;
        if spectrum.is_empty() {
            self.schoolbook(
                ctx,
                &mut limbs.data,
                &[],
                &scale.limbs.data,
                &scale.limbs.data,
                2 * length,
            )?;
        } else {
            // The spectrum carries one factor of 1/size; the square needs one.
            let size = spectrum.len() as u64;
            let mut data = Buffer::with_capacity(ctx, spectrum.len())?;
            self.meter.spend(ctx, 2 * spectrum.len())?;
            data.data.extend(
                spectrum
                    .iter()
                    .map(|&value| ntt::mul(ntt::mul(value, value), size)),
            );
            ntt::inverse(ctx, &mut self.meter, &self.roots, &mut data.data)?;
            self.carry(ctx, &mut limbs.data, &data.data, &[], 2 * length)?;
        }
        let length = self.significant(ctx, &limbs.data)?.len();
        limbs.data.truncate(length);
        Ok(Scale {
            limbs,
            spectrum: Buffer::empty(),
        })
    }

    /// Appends `high * scale + low` to `out` as exactly `width` limbs,
    /// by whichever product method performs the fewest word operations.
    fn combine(
        &mut self,
        ctx: &mut CallContext,
        out: &mut Vec<u32>,
        low: &[u32],
        high: &[u32],
        scale: &Scale,
        width: usize,
    ) -> Result<()> {
        let start = out.len();
        let spectrum = &scale.spectrum.data;
        let power = &scale.limbs.data;
        if high.is_empty() {
            self.meter.spend(ctx, low.len())?;
            out.extend_from_slice(low);
        } else {
            let schoolbook = high.len() * power.len();
            let (size, pieces) = ntt::pieces(high.len(), power.len())?;
            let cached = if spectrum.is_empty() {
                usize::MAX
            } else {
                ntt::transform_cost(spectrum.len())
            };
            let fresh = ntt::transform_cost(size) * pieces + ntt::spectrum_cost(size);
            if schoolbook <= cached.min(fresh) {
                self.schoolbook(ctx, out, low, high, power, width)?;
            } else if cached <= fresh {
                let mut data = ntt::zeroed(ctx, &mut self.meter, spectrum.len())?;
                self.meter.spend(ctx, high.len())?;
                for (value, &limb) in data.data.iter_mut().zip(high) {
                    *value = limb as u64;
                }
                ntt::forward(ctx, &mut self.meter, &self.roots, &mut data.data)?;
                ntt::pointwise(ctx, &mut self.meter, &mut data.data, spectrum, 1)?;
                ntt::inverse(ctx, &mut self.meter, &self.roots, &mut data.data)?;
                self.carry(ctx, out, &data.data, low, width)?;
            } else {
                self.transformed(ctx, out, low, high, power, width)?;
            }
        }
        self.pad(ctx, out, start + width)
    }

    /// Appends `width` limbs of `a * b + low` through transforms, cutting
    /// the longer factor into pieces that share the shorter one's spectrum.
    fn transformed(
        &mut self,
        ctx: &mut CallContext,
        out: &mut Vec<u32>,
        low: &[u32],
        a: &[u32],
        b: &[u32],
        width: usize,
    ) -> Result<()> {
        let (long, short) = if a.len() < b.len() { (b, a) } else { (a, b) };
        let (size, _) = ntt::pieces(long.len(), short.len())?;
        let piece = size + 1 - short.len();
        let spectrum = self.spectrum(ctx, short, size)?;
        let start = out.len();
        self.meter.spend(ctx, width)?;
        out.extend_from_slice(low);
        out.resize(start + width, 0);
        let mut data = ntt::zeroed(ctx, &mut self.meter, size)?;
        for (index, chunk) in long.chunks(piece).enumerate() {
            if index != 0 {
                self.meter.spend(ctx, size)?;
                data.data.fill(0);
            }
            self.meter.spend(ctx, chunk.len())?;
            for (value, &limb) in data.data.iter_mut().zip(chunk) {
                *value = limb as u64;
            }
            ntt::forward(ctx, &mut self.meter, &self.roots, &mut data.data)?;
            ntt::pointwise(ctx, &mut self.meter, &mut data.data, &spectrum.data, 1)?;
            ntt::inverse(ctx, &mut self.meter, &self.roots, &mut data.data)?;
            let values = &data.data[..chunk.len() + short.len() - 1];
            self.accumulate(ctx, &mut out[start + index * piece..], values)?;
        }
        Ok(())
    }

    /// Adds `values` into `limbs`, normalizing carries through the rest.
    fn accumulate(
        &mut self,
        ctx: &mut CallContext,
        limbs: &mut [u32],
        values: &[u64],
    ) -> Result<()> {
        self.meter.spend(ctx, values.len())?;
        let mut carry = 0u64;
        for (limb, &value) in limbs.iter_mut().zip(values) {
            let (quotient, remainder) = self.base.split(*limb as u64 + value + carry);
            *limb = remainder;
            carry = quotient;
        }
        let mut index = values.len();
        while carry != 0 {
            self.meter.spend(ctx, 1)?;
            let (quotient, remainder) = self.base.split(limbs[index] as u64 + carry);
            limbs[index] = remainder;
            carry = quotient;
            index += 1;
        }
        Ok(())
    }

    /// Appends `width` limbs of `values + low`, normalizing carries.
    fn carry(
        &mut self,
        ctx: &mut CallContext,
        out: &mut Vec<u32>,
        values: &[u64],
        low: &[u32],
        width: usize,
    ) -> Result<()> {
        self.meter.spend(ctx, width)?;
        let mut carry = 0u64;
        for i in 0..width {
            let value =
                values.get(i).copied().unwrap_or(0) + low.get(i).copied().unwrap_or(0) as u64;
            let (quotient, remainder) = self.base.split(value + carry);
            out.push(remainder);
            carry = quotient;
        }
        debug_assert_eq!(carry, 0);
        Ok(())
    }

    /// Appends `width` limbs of `a * b + low` by column sums.
    fn schoolbook(
        &mut self,
        ctx: &mut CallContext,
        out: &mut Vec<u32>,
        low: &[u32],
        a: &[u32],
        b: &[u32],
        width: usize,
    ) -> Result<()> {
        let mut carry = 0u64;
        for k in 0..width {
            let mut sum = carry + low.get(k).copied().unwrap_or(0) as u64;
            if k + 1 < a.len() + b.len() {
                let first = (k + 1).saturating_sub(b.len());
                let last = k.min(a.len() - 1);
                self.meter.spend(ctx, last + 2 - first)?;
                for (&x, &y) in a[first..=last]
                    .iter()
                    .zip(b[k - last..=k - first].iter().rev())
                {
                    sum += x as u64 * y as u64;
                }
            } else {
                self.meter.spend(ctx, 1)?;
            }
            let (quotient, remainder) = self.base.split(sum);
            out.push(remainder);
            carry = quotient;
        }
        debug_assert_eq!(carry, 0);
        Ok(())
    }

    /// Appends the digits of nonzero limbs, most significant first.
    fn emit(&mut self, ctx: &mut CallContext, limbs: &[u32], out: &mut Buffer<u8>) -> Result<()> {
        let (radix, digits) = (self.base.radix(), self.base.digits());
        let mut top = limbs[limbs.len() - 1];
        let mut leading = 0;
        while top != 0 {
            top /= radix;
            leading += 1;
        }
        let count = leading + (limbs.len() - 1) * digits;
        out.ensure(ctx, out.data.len() + count)?;
        let mut scratch = [0u8; 32];
        for (index, &limb) in limbs.iter().rev().enumerate() {
            let width = if index == 0 { leading } else { digits };
            self.meter.spend(ctx, width)?;
            let mut value = limb;
            for slot in scratch[..width].iter_mut().rev() {
                *slot = b"0123456789abcdefghijklmnopqrstuvwxyz"[(value % radix) as usize];
                value /= radix;
            }
            out.data.extend_from_slice(&scratch[..width]);
        }
        Ok(())
    }
}
