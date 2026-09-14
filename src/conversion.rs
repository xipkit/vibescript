use crate::{CallContext, Error, ErrorKind, Result, budget::Buffer, json, ops};

fn invalid() -> Error {
    Error::new(
        ErrorKind::Argument,
        "to_float expects a finite numeric string",
    )
}

pub(crate) fn float(ctx: &mut CallContext, input: &[u8]) -> Result<f64> {
    let (start, end) = ops::trim(ctx, input)?;
    let input = &input[start..end];
    let mut underscores = false;
    for chunk in input.chunks(1024) {
        ctx.work_bytes(chunk.len())?;
        for &byte in chunk {
            if !byte.is_ascii() {
                return Err(invalid());
            }
            underscores |= byte == b'_';
        }
    }
    let sign = usize::from(matches!(input.first(), Some(b'-' | b'+')));
    let hex = input
        .get(sign..sign + 2)
        .is_some_and(|prefix| prefix == b"0x" || prefix == b"0X");
    let mut at = sign + if hex { 2 } else { 0 };
    let radix = if hex { 16 } else { 10 };
    let whole = digits(ctx, input, &mut at, radix, hex)?;
    let fraction = if input.get(at) == Some(&b'.') {
        at += 1;
        digits(ctx, input, &mut at, radix, false)?
    } else {
        0
    };
    if whole + fraction == 0 {
        return Err(invalid());
    }
    if input.get(at).is_some_and(|b| {
        if hex {
            matches!(b, b'p' | b'P')
        } else {
            matches!(b, b'e' | b'E')
        }
    }) {
        at += 1;
        if matches!(input.get(at), Some(b'-' | b'+')) {
            at += 1;
        }
        if digits(ctx, input, &mut at, 10, false)? == 0 {
            return Err(invalid());
        }
    } else if hex {
        return Err(invalid());
    }
    if at != input.len() {
        return Err(invalid());
    }
    let mut normalized = Buffer::empty();
    let input = if underscores {
        normalized.ensure(ctx, input.len())?;
        for chunk in input.chunks(1024) {
            ctx.work_bytes(chunk.len())?;
            normalized
                .data
                .extend(chunk.iter().copied().filter(|&b| b != b'_'));
        }
        normalized.data.as_slice()
    } else {
        input
    };
    let value = if hex {
        hex_float(ctx, input)?
    } else {
        let input = input.strip_prefix(b"+").unwrap_or(input);
        json::parse_float(ctx, input).map_err(|error| {
            if error.class() == Some(crate::ErrorClass::Limit)
                || matches!(error.kind, ErrorKind::Cancelled | ErrorKind::Deadline)
            {
                error
            } else {
                invalid()
            }
        })?
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid())
    }
}

fn digit(byte: u8, radix: u32) -> bool {
    (byte as char).is_digit(radix)
}

fn digits(
    ctx: &mut CallContext,
    input: &[u8],
    at: &mut usize,
    radix: u32,
    prefix: bool,
) -> Result<usize> {
    let mut count = 0;
    let mut previous = prefix;
    while let Some(&byte) = input.get(*at) {
        if *at % 1024 == 0 {
            ctx.work_bytes((input.len() - *at).min(1024))?;
        }
        if byte == b'_' {
            if !previous || !input.get(*at + 1).is_some_and(|&next| digit(next, radix)) {
                return Err(invalid());
            }
            previous = false;
        } else if digit(byte, radix) {
            previous = true;
            count += 1;
        } else {
            break;
        }
        *at += 1;
    }
    Ok(count)
}

fn hex_float(ctx: &mut CallContext, input: &[u8]) -> Result<f64> {
    let negative = input[0] == b'-';
    let mut at = usize::from(matches!(input[0], b'-' | b'+')) + 2;
    let mut mantissa = 0u64;
    let mut significant = 0usize;
    let mut fractional = 0usize;
    let mut fraction = false;
    let mut tail = false;
    while !matches!(input[at], b'p' | b'P') {
        if at % 1024 == 0 {
            ctx.work_bytes((input.len() - at).min(1024))?;
        }
        let byte = input[at];
        at += 1;
        if byte == b'.' {
            fraction = true;
            continue;
        }
        fractional += usize::from(fraction);
        let digit = (byte as char).to_digit(16).unwrap() as u64;
        if significant != 0 || digit != 0 {
            if significant < 16 {
                mantissa = mantissa << 4 | digit;
            } else {
                tail |= digit != 0;
            }
            significant += 1;
        }
    }
    at += 1;
    let exponent_negative = input.get(at) == Some(&b'-');
    if matches!(input.get(at), Some(b'-' | b'+')) {
        at += 1;
    }
    let mut exponent = 0i128;
    while at < input.len() {
        if at % 1024 == 0 {
            ctx.work_bytes((input.len() - at).min(1024))?;
        }
        exponent = exponent
            .saturating_mul(10)
            .saturating_add((input[at] - b'0') as i128);
        at += 1;
    }
    if exponent_negative {
        exponent = -exponent;
    }
    exponent = exponent
        .saturating_sub(4 * fractional as i128)
        .saturating_add(4 * significant.saturating_sub(16) as i128);
    let sign = u64::from(negative) << 63;
    if mantissa == 0 {
        return Ok(f64::from_bits(sign));
    }
    let width = 64 - mantissa.leading_zeros() as i128;
    let mut binary_exponent = exponent.saturating_add(width - 1);
    if binary_exponent > 1023 {
        return Ok(f64::from_bits(sign | 0x7ff0_0000_0000_0000));
    }
    if binary_exponent < -1075 {
        return Ok(f64::from_bits(sign));
    }
    let normal = binary_exponent >= -1022;
    let shift = if normal { width - 53 } else { -1074 - exponent };
    let mut rounded = if shift <= 0 {
        mantissa << (-shift) as u32
    } else {
        let shift = shift as u32;
        let whole = mantissa.checked_shr(shift).unwrap_or(0);
        let half = 1u64.checked_shl(shift - 1).unwrap_or(0);
        let remainder = if shift >= 64 {
            mantissa
        } else {
            mantissa & ((1u64 << shift) - 1)
        };
        whole
            + u64::from(
                half != 0 && (remainder > half || remainder == half && (tail || whole & 1 != 0)),
            )
    };
    if !normal {
        return Ok(f64::from_bits(sign | rounded));
    }
    if rounded == 1u64 << 53 {
        rounded >>= 1;
        binary_exponent += 1;
    }
    Ok(f64::from_bits(
        sign | ((binary_exponent + 1023) as u64) << 52 | (rounded & ((1u64 << 52) - 1)),
    ))
}
