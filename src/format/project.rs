use super::*;

pub(super) fn string_bytes(
    ctx: &mut CallContext,
    value: &Value,
    precision: Option<usize>,
) -> Result<usize> {
    if let Kind::Bytes(bytes) | Kind::Symbol(bytes) = &value.0 {
        return match precision {
            Some(count) => precision_bytes(ctx, &bytes.data, count),
            None => {
                ctx.work_bytes(bytes.data.len())?;
                Ok(bytes.data.len())
            }
        };
    }
    let limit = precision.map_or(usize::MAX, |n| n.saturating_mul(4));
    crate::text::bounded::measure(ctx, value, limit, precision.is_some())
}

fn string_runes(ctx: &mut CallContext, value: &Value, precision: Option<usize>) -> Result<usize> {
    let count = if let Kind::Bytes(bytes) | Kind::Symbol(bytes) = &value.0 {
        crate::ops::runes(ctx, &bytes.data)?.0
    } else {
        crate::text::bounded::measure_runes(ctx, value)?
    };
    Ok(precision.map_or(count, |limit| count.min(limit)))
}

pub(super) fn field(ctx: &mut CallContext, value: &Value, verb: u8, field: Field) -> Result<usize> {
    let precision = field.precision;
    let quoted = |n: usize| n.saturating_mul(4).saturating_add(2);
    let mut bytes = match verb {
        b's' => string_bytes(ctx, value, precision)?,
        b'q' => quoted(string_bytes(ctx, value, precision)?),
        b'x' | b'X' => match &value.0 {
            Kind::Bytes(bytes) | Kind::Symbol(bytes) => {
                ctx.work_bytes(bytes.data.len())?;
                let count = bytes.data.len();
                let mut length = count.saturating_mul(if field.flag(SPACE) { 3 } else { 2 });
                if field.flag(SHARP) {
                    length = length.saturating_add(2);
                    if field.flag(SPACE) {
                        length = length.saturating_add(count.saturating_mul(2));
                    }
                }
                length
            }
            Kind::Int(_) | Kind::Big(_) => integer_bytes(ctx, value, verb, field)?,
            Kind::Float(_) => precision.map_or(64, |n| n.saturating_add(32)),
            _ => unreachable!(),
        },
        b'd' | b'b' | b'o' | b'O' | b'U' => integer_bytes(ctx, value, verb, field)?,
        b'c' => 64,
        b'f' | b'F' => fixed_float(value, field),
        b'e' | b'E' | b'g' | b'G' => precision.map_or(64, |n| n.saturating_add(16)),
        b't' => 5,
        b'v' if field.flag(SHARP) => quoted(string_bytes(ctx, value, None)?),
        b'v' if string_like(value) => string_bytes(ctx, value, precision)?,
        _ => string_bytes(ctx, value, None)?.saturating_add(32),
    };
    if matches!(verb, b'f' | b'F' | b'e' | b'E' | b'g' | b'G') {
        if let Some(precision) = precision {
            bytes = bytes.max(precision.saturating_add(16));
        }
    }
    if let Some(width) = field.width {
        if verb == b's' || (verb == b'v' && !field.flag(SHARP) && string_like(value)) {
            let runes = string_runes(ctx, value, precision)?;
            bytes = bytes.saturating_add(width.saturating_sub(runes));
        } else {
            bytes = bytes.max(width);
        }
    }
    Ok(bytes)
}

fn integer_bytes(ctx: &mut CallContext, value: &Value, verb: u8, field: Field) -> Result<usize> {
    let base: u64 = match verb {
        b'b' => 2,
        b'o' | b'O' => 8,
        b'x' | b'X' | b'U' => 16,
        _ => 10,
    };
    let mut prefix = match verb {
        b'b' | b'x' | b'X' if field.flag(SHARP) => 2,
        b'o' if field.flag(SHARP) => 1,
        b'O' => 2 + usize::from(field.flag(SHARP)),
        _ => 0,
    };
    let (mut digits, negative) = if matches!(value.0, Kind::Big(_)) {
        let bits = crate::integer::bits(value);
        let digits = match base {
            2 => bits,
            8 => bits / 3 + 1,
            16 => bits / 4 + 1,
            _ => {
                ((bits as u128 * 30103) / 100000 + 1 + u128::from(float(value).is_sign_negative()))
                    .min(usize::MAX as u128) as usize
            }
        };
        ctx.charge(1 + (digits / 16) as u64)?;
        (digits, float(value).is_sign_negative())
    } else {
        let n = integer(value).unwrap();
        let magnitude = if verb == b'U' {
            n as u64
        } else {
            n.unsigned_abs()
        };
        (digits(magnitude, base), n < 0)
    };
    if verb == b'U' {
        digits = digits.max(4);
        prefix = 2 + if field.flag(SHARP) { 16 } else { 0 };
    } else {
        prefix += usize::from(negative || field.flag(PLUS) || field.flag(SPACE));
    }
    if let Some(precision) = field.precision {
        digits = digits.max(precision);
    }
    Ok(digits.saturating_add(prefix))
}

fn digits(mut n: u64, base: u64) -> usize {
    let mut digits = 1;
    while n >= base {
        n /= base;
        digits += 1;
    }
    digits
}

fn fixed_float(value: &Value, field: Field) -> usize {
    let n = float(value);
    let sign = usize::from(n.is_sign_negative() || field.flag(PLUS) || field.flag(SPACE));
    if matches!(value.0, Kind::Float(_)) && !n.is_finite() {
        return sign + 3;
    }
    let digits = match &value.0 {
        Kind::Big(big) => {
            ((crate::integer::bits(value) as u128 * 30103) / 100000 + 1 + u128::from(big.negative))
                .min(usize::MAX as u128) as usize
        }
        Kind::Int(n) => digits(n.unsigned_abs(), 10),
        _ if n.abs() < 1.0 => 1,
        _ => {
            use std::fmt::Write;
            let mut number = crate::json::Number::new();
            write!(number, "{:e}", n.abs()).unwrap();
            let bytes = number.bytes();
            let exponent = bytes.iter().rposition(|b| *b == b'e').unwrap();
            let exponent: i32 = std::str::from_utf8(&bytes[exponent + 1..])
                .unwrap()
                .parse()
                .unwrap();
            (exponent + 1).max(1) as usize
        }
    };
    let fraction = field.precision.unwrap_or(6);
    sign.saturating_add(digits)
        .saturating_add(fraction)
        .saturating_add(usize::from(fraction > 0 || field.flag(SHARP)))
}
