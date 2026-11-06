use super::*;
use crate::checked::{Access, Checked, convert, violation};
use crate::numeric::{to_usize, unsigned_bits};
use std::fmt::{self, Write};

struct Digits {
    bytes: [u8; 2048],
    length: usize,
    split: usize,
    zeros: usize,
}

impl Digits {
    fn new() -> Self {
        Self {
            bytes: [0; 2048],
            length: 0,
            split: 0,
            zeros: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Access<()> {
        let end = self.length + bytes.len();
        self.bytes
            .span_mut(self.length, end)?
            .copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }

    fn radix(&mut self, mut value: u64, base: u64, uppercase: bool) -> Access<()> {
        let alphabet = if uppercase {
            b"0123456789ABCDEF"
        } else {
            b"0123456789abcdef"
        };
        let start = self.length;
        loop {
            self.push(&[*alphabet.at(convert::<usize, _>(value % base)?)?])?;
            value /= base;
            if value == 0 {
                break;
            }
        }
        self.bytes.span_mut(start, self.length)?.reverse();
        Ok(())
    }

    fn exponent(&mut self, letter: u8, exponent: i32) -> Access<()> {
        self.push(&[letter, if exponent < 0 { b'-' } else { b'+' }])?;
        if exponent.unsigned_abs() < 10 {
            self.push(b"0")?;
        }
        self.radix(exponent.unsigned_abs() as u64, 10, false)
    }

    fn write(&self, ctx: &mut CallContext, output: &mut Output) -> Result<()> {
        output.write(ctx, self.bytes.span(0, self.split)?)?;
        output.repeat(ctx, b'0', self.zeros)?;
        output.write(ctx, self.bytes.span(self.split, self.length)?)
    }
}

impl Write for Digits {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > self.bytes.len() - self.length {
            return Err(fmt::Error);
        }
        self.push(text.as_bytes()).map_err(|_| fmt::Error)?;
        Ok(())
    }
}

pub(super) fn integer(
    ctx: &mut CallContext,
    output: &mut Output,
    value: &Value,
    verb: char,
    mut field: Field,
) -> Result<()> {
    if verb == 'c' {
        let Kind::Int(value) = value.0 else {
            return Err(crate::checked::internal(
                "integer format requires an integer",
            ));
        };
        let rune = u32::try_from(value)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or('\u{fffd}');
        let mut bytes = [0; 4];
        return pad(ctx, output, rune.encode_utf8(&mut bytes).as_bytes(), field);
    }
    if verb == 'U' {
        let Kind::Int(value) = value.0 else {
            return Err(crate::checked::internal(
                "integer format requires an integer",
            ));
        };
        return unicode(ctx, output, unsigned_bits(value), field);
    }
    let big = matches!(value.0, Kind::Big(_));
    if verb == 'v' && !big {
        field.flags &= !(SHARP | PLUS);
    }
    let base = match verb {
        'b' => 2,
        'o' | 'O' => 8,
        'x' | 'X' => 16,
        _ => 10,
    };
    let mut small = Digits::new();
    let mut large;
    let (digits, negative) = match &value.0 {
        Kind::Int(n) => {
            small.radix(n.unsigned_abs(), base, verb == 'X')?;
            (small.bytes.span(0, small.length)?, *n < 0)
        }
        Kind::Big(n) => {
            large = crate::integer::format(ctx, value, convert::<u32, _>(base)?)?;
            if verb == 'X' {
                for bytes in large.data.chunks_mut(CHUNK) {
                    ctx.work_bytes(bytes.len())?;
                    bytes.make_ascii_uppercase();
                }
            }
            (large.tail(usize::from(n.negative))?, n.negative)
        }
        _ => {
            return Err(crate::checked::internal(
                "integer format requires an integer",
            ));
        }
    };
    integral(ctx, output, digits, negative, big, verb, field)
}

pub(super) fn unsigned(
    ctx: &mut CallContext,
    output: &mut Output,
    value: u64,
    verb: char,
    field: Field,
) -> Result<()> {
    let mut digits = Digits::new();
    digits.radix(value, if verb == 'x' { 16 } else { 10 }, false)?;
    integral(
        ctx,
        output,
        digits.bytes.span(0, digits.length)?,
        false,
        false,
        verb,
        field,
    )
}

fn integral(
    ctx: &mut CallContext,
    output: &mut Output,
    digits: &[u8],
    negative: bool,
    big: bool,
    verb: char,
    field: Field,
) -> Result<()> {
    if digits == b"0" && field.precision == Some(0) {
        return output.repeat(ctx, b' ', field.width.unwrap_or(0));
    }
    let sign: &[u8] = if negative {
        b"-"
    } else if field.flag(PLUS) {
        b"+"
    } else if field.flag(SPACE) {
        b" "
    } else {
        b""
    };
    let mut zeros = field.precision.unwrap_or(0).saturating_sub(digits.len());
    if !big && field.precision.is_none() && field.flag(ZERO) && !field.flag(MINUS) {
        zeros = field
            .width
            .unwrap_or(0)
            .saturating_sub(sign.len() + digits.len());
    }
    let sharp_octal = field.flag(SHARP) && (big || (zeros == 0 && *digits.at(0)? != b'0'));
    let prefix: &[u8] = match verb {
        'b' if field.flag(SHARP) => b"0b",
        'x' if field.flag(SHARP) => b"0x",
        'X' if field.flag(SHARP) => b"0X",
        'o' if sharp_octal => b"0",
        'O' if !big && sharp_octal => b"0o0",
        'O' => b"0o",
        _ => b"",
    };
    let mut padding = field
        .width
        .unwrap_or(0)
        .saturating_sub(sign.len() + prefix.len() + zeros + digits.len());
    if big && field.precision.is_none() && field.flag(ZERO) && !field.flag(MINUS) {
        zeros += padding;
        padding = 0;
    }
    if !field.flag(MINUS) {
        output.repeat(ctx, b' ', padding)?;
    }
    output.write(ctx, sign)?;
    output.write(ctx, prefix)?;
    output.repeat(ctx, b'0', zeros)?;
    output.write(ctx, digits)?;
    if field.flag(MINUS) {
        output.repeat(ctx, b' ', padding)?;
    }
    Ok(())
}

pub(super) fn debug_big(
    ctx: &mut CallContext,
    output: &mut Output,
    value: &Value,
    mut field: Field,
) -> Result<()> {
    let Kind::Big(big) = &value.0 else {
        return Err(crate::checked::internal("big integer expected"));
    };
    let sharp = field.flag(SHARP);
    let names = sharp || field.flag(PLUS);
    field.flags &= !(SHARP | PLUS);
    output.write(ctx, if sharp { b"&big.Int{" } else { b"&{" })?;
    if names {
        output.write(ctx, b"neg:")?;
    }
    pad(
        ctx,
        output,
        if big.negative { b"true" } else { b"false" },
        field,
    )?;
    output.write(ctx, if sharp { b", " } else { b" " })?;
    if names {
        output.write(ctx, b"abs:")?;
    }
    output.write(ctx, if sharp { b"big.nat{" } else { b"[" })?;
    let bytes = crate::integer::format(ctx, value, 16)?;
    let bytes = bytes.tail(usize::from(big.negative))?;
    if sharp {
        field.flags |= SHARP;
    }
    // Go uses 64-bit words on its WebAssembly targets.
    let word_bytes = if cfg!(target_family = "wasm") {
        8
    } else {
        std::mem::size_of::<usize>()
    };
    for (index, word) in bytes.rchunks(2 * word_bytes).enumerate() {
        ctx.charge(1)?;
        if index > 0 {
            output.write(ctx, if sharp { b", " } else { b" " })?;
        }
        let word = u64::from_str_radix(
            std::str::from_utf8(word).map_err(|_| violation("integer digits must be UTF-8"))?,
            16,
        )
        .map_err(|_| violation("hexadecimal word expected"))?;
        unsigned(ctx, output, word, if sharp { 'x' } else { 'd' }, field)?;
    }
    output.write(ctx, if sharp { b"}}" } else { b"]}" })
}

fn unicode(ctx: &mut CallContext, output: &mut Output, value: u64, field: Field) -> Result<()> {
    let mut digits = Digits::new();
    digits.radix(value, 16, true)?;
    let zeros = field
        .precision
        .unwrap_or(4)
        .max(4)
        .saturating_sub(digits.length);
    let rune = u32::try_from(value)
        .ok()
        .and_then(char::from_u32)
        .filter(|c| field.flag(SHARP) && crate::printable::is_print(*c));
    let length = 2 + zeros + digits.length + 4 * usize::from(rune.is_some());
    let padding = field.width.unwrap_or(0).saturating_sub(length);
    if !field.flag(MINUS) {
        output.repeat(ctx, b' ', padding)?;
    }
    output.write(ctx, b"U+")?;
    output.repeat(ctx, b'0', zeros)?;
    output.write(ctx, digits.bytes.span(0, digits.length)?)?;
    if let Some(rune) = rune {
        let mut bytes = [0; 4];
        output.write(ctx, b" '")?;
        output.write(ctx, rune.encode_utf8(&mut bytes).as_bytes())?;
        output.write(ctx, b"'")?;
    }
    if field.flag(MINUS) {
        output.repeat(ctx, b' ', padding)?;
    }
    Ok(())
}

pub(super) fn float(
    ctx: &mut CallContext,
    output: &mut Output,
    value: f64,
    verb: char,
    mut field: Field,
) -> Result<()> {
    if verb == 'v' {
        field.flags &= !(SHARP | PLUS);
    }
    let negative = value.is_sign_negative() && !value.is_nan();
    let sign: &[u8] = if negative {
        b"-"
    } else if field.flag(PLUS) {
        b"+"
    } else if field.flag(SPACE) {
        b" "
    } else if value.is_infinite() {
        b"+"
    } else {
        b""
    };
    let mut digits = if !value.is_finite() {
        field.flags &= !ZERO;
        let mut digits = Digits::new();
        digits.push(if value.is_nan() { b"NaN" } else { b"Inf" })?;
        digits
    } else {
        ctx.charge(1)?;
        match verb {
            'f' | 'F' => fixed(value.abs(), field)?,
            'e' | 'E' => scientific(value.abs(), verb == 'E', field)?,
            'g' | 'G' | 'v' => general(value.abs(), verb == 'G', field)?,
            'x' | 'X' => hexadecimal(value.abs(), verb == 'X', field)?,
            'b' => binary(value.abs())?,
            _ => {
                return Err(crate::checked::internal("unsupported float format verb"));
            }
        }
    };
    if digits.zeros == 0 {
        digits.split = digits.length;
    }
    let padding = field
        .width
        .unwrap_or(0)
        .saturating_sub(sign.len() + digits.length + digits.zeros);
    if !field.flag(MINUS) && !field.flag(ZERO) {
        output.repeat(ctx, b' ', padding)?;
    }
    output.write(ctx, sign)?;
    if !field.flag(MINUS) && field.flag(ZERO) {
        output.repeat(ctx, b'0', padding)?;
    }
    digits.write(ctx, output)?;
    if field.flag(MINUS) {
        output.repeat(ctx, b' ', padding)?;
    }
    Ok(())
}

fn fixed(value: f64, field: Field) -> Access<Digits> {
    let precision = field.precision.unwrap_or(6);
    let bounded = precision.min(1074);
    let mut digits = Digits::new();
    write!(digits, "{value:.bounded$}")
        .map_err(|_| violation("fixed float exceeds digit buffer"))?;
    if precision == 0 && field.flag(SHARP) {
        digits.push(b".")?;
    }
    digits.split = digits.length;
    digits.zeros = precision - bounded;
    Ok(digits)
}

fn scientific(value: f64, uppercase: bool, field: Field) -> Access<Digits> {
    let precision = field.precision.unwrap_or(6);
    let bounded = precision.min(1074);
    let mut raw = Digits::new();
    write!(raw, "{value:.bounded$e}")
        .map_err(|_| violation("scientific float exceeds digit buffer"))?;
    let (end, exponent) = exponent(&raw)?;
    let mut digits = Digits::new();
    digits.push(raw.bytes.span(0, end)?)?;
    if precision == 0 && field.flag(SHARP) {
        digits.push(b".")?;
    }
    digits.split = digits.length;
    digits.zeros = precision - bounded;
    digits.exponent(if uppercase { b'E' } else { b'e' }, exponent)?;
    Ok(digits)
}

fn exponent(raw: &Digits) -> Access<(usize, i32)> {
    let offset = raw
        .bytes
        .span(0, raw.length)?
        .iter()
        .position(|b| *b == b'e')
        .ok_or(violation("scientific exponent expected"))?;
    let exponent = std::str::from_utf8(raw.bytes.span(offset + 1, raw.length)?)
        .map_err(|_| violation("scientific exponent must be UTF-8"))?
        .parse()
        .map_err(|_| violation("scientific exponent must be an integer"))?;
    Ok((offset, exponent))
}

fn general(value: f64, uppercase: bool, field: Field) -> Access<Digits> {
    let mut raw = Digits::new();
    if let Some(precision) = field.precision {
        let bounded = precision.max(1).saturating_sub(1).min(1074);
        write!(raw, "{value:.bounded$e}")
            .map_err(|_| violation("general float exceeds digit buffer"))?;
    } else {
        write!(raw, "{value:e}").map_err(|_| violation("general float exceeds digit buffer"))?;
    }
    let (end, exponent) = exponent(&raw)?;
    let mut significand = Digits::new();
    for byte in raw.bytes.span(0, end)? {
        if *byte != b'.' {
            significand.push(&[*byte])?;
        }
    }
    while significand.length > 1 && *significand.bytes.at(significand.length - 1)? == b'0' {
        significand.length -= 1;
    }
    let precision = field.precision.map_or(6, |n| n.max(1));
    let scientific = exponent < -4 || exponent >= i32::try_from(precision).unwrap_or(i32::MAX);
    let mut digits = Digits::new();
    let significant = if scientific {
        digits.push(&significand.bytes[..1])?;
        if significand.length > 1 || field.flag(SHARP) {
            digits.push(b".")?;
        }
        digits.push(significand.bytes.span(1, significand.length)?)?;
        significand.length
    } else if exponent < 0 {
        digits.push(b"0.")?;
        for _ in 0..-exponent - 1 {
            digits.push(b"0")?;
        }
        digits.push(significand.bytes.span(0, significand.length)?)?;
        significand.length
    } else {
        let integer = to_usize(exponent) + 1;
        let used = integer.min(significand.length);
        digits.push(significand.bytes.span(0, used)?)?;
        for _ in used..integer {
            digits.push(b"0")?;
        }
        if used < significand.length || field.flag(SHARP) {
            digits.push(b".")?;
        }
        digits.push(significand.bytes.span(used, significand.length)?)?;
        integer.max(significand.length)
    };
    digits.split = digits.length;
    if field.flag(SHARP) {
        // fmt keeps zero significant digits for an explicitly requested %.0g.
        digits.zeros = field.precision.unwrap_or(6).saturating_sub(significant);
    }
    if scientific {
        digits.exponent(if uppercase { b'E' } else { b'e' }, exponent)?;
    }
    Ok(digits)
}

fn hexadecimal(value: f64, uppercase: bool, field: Field) -> Access<Digits> {
    let bits = value.to_bits();
    let mut mantissa = bits & ((1u64 << 52) - 1);
    let mut exponent = ((bits >> 52) & 2047) as i32 - 1023;
    if bits >> 52 != 0 {
        mantissa |= 1u64 << 52;
    } else if mantissa != 0 {
        let shift = mantissa.leading_zeros() - 11;
        mantissa <<= shift;
        exponent = (-1022i32).saturating_sub_unsigned(shift);
    } else {
        exponent = 0;
    }
    let precision = field.precision.unwrap_or(13);
    if precision < 13 {
        let shift = 52 - precision * 4;
        let remainder = mantissa & ((1u64 << shift) - 1);
        let halfway = 1u64 << (shift - 1);
        let mut rounded = mantissa >> shift;
        if remainder > halfway || (remainder == halfway && rounded & 1 != 0) {
            rounded += 1;
        }
        mantissa = rounded << shift;
        if mantissa >= 1u64 << 53 {
            mantissa >>= 1;
            exponent += 1;
        }
    }
    let alphabet = if uppercase {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut digits = Digits::new();
    digits.push(if uppercase { b"0X" } else { b"0x" })?;
    digits.push(&[*alphabet.at((mantissa >> 52) as usize)?])?;
    let mut fraction = precision.min(13);
    if field.precision.is_none() {
        while fraction > 0 && (mantissa >> ((13 - fraction) * 4)) & 15 == 0 {
            fraction -= 1;
        }
    }
    if fraction > 0 || field.flag(SHARP) || precision > 13 {
        digits.push(b".")?;
    }
    for index in 0..fraction {
        digits.push(&[*alphabet.at(((mantissa >> (48 - index * 4)) & 15) as usize)?])?;
    }
    digits.split = digits.length;
    digits.zeros = precision.saturating_sub(13);
    if field.flag(SHARP) && !uppercase {
        // Go fmt counts the x in 0x as a significant digit when restoring zeros.
        let desired = field.precision.unwrap_or(6);
        digits.zeros = digits.zeros.max(desired.saturating_sub(2 + fraction));
    }
    digits.exponent(if uppercase { b'P' } else { b'p' }, exponent)?;
    Ok(digits)
}

fn binary(value: f64) -> Access<Digits> {
    let bits = value.to_bits();
    let mut mantissa = bits & ((1u64 << 52) - 1);
    let mut exponent = ((bits >> 52) & 2047) as i32 - 1023;
    if bits >> 52 != 0 {
        mantissa |= 1u64 << 52;
    } else {
        exponent = -1022;
    }
    exponent -= 52;
    let mut digits = Digits::new();
    digits.radix(mantissa, 10, false)?;
    digits.push(&[b'p', if exponent < 0 { b'-' } else { b'+' }])?;
    digits.radix(exponent.unsigned_abs() as u64, 10, false)?;
    Ok(digits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digit_buffer_overflow_reports_an_internal_error() {
        let mut digits = Digits::new();
        digits.push(&[b'0'; 2048]).unwrap();
        let error = crate::Error::from(digits.push(b"1").unwrap_err());
        assert_eq!(error.kind, crate::ErrorKind::Internal);
        assert_eq!(digits.length, 2048);
        assert!(digits.write_str("1").is_err());
    }

    #[test]
    fn malformed_generated_exponents_return_internal_errors() {
        for bytes in [b"1".as_slice(), b"1e?", b"1e\xff"] {
            let mut raw = Digits::new();
            raw.push(bytes).unwrap();
            let error = crate::Error::from(exponent(&raw).unwrap_err());
            assert_eq!(error.kind, crate::ErrorKind::Internal);
        }
    }
}
