use crate::{
    CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, hash::Hash, json, ops,
    value::Kind,
};
use std::{cmp::Ordering, fmt::Write};

mod parse;

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid duration literal")
}
fn overflow() -> Error {
    Error::new(ErrorKind::Arithmetic, "duration result out of 64-bit range")
}
fn numeric(value: &Value) -> Result<i64> {
    crate::sequence::integer(value).map_err(|_| {
        Error::new(
            ErrorKind::Type,
            "duration expects finite seconds within the signed 64-bit range",
        )
    })
}

pub(crate) fn build(
    ctx: &mut CallContext,
    args: &[Value],
    keywords: &[(Value, Value)],
) -> Result<Value> {
    if keywords.is_empty() {
        ops::arity(args, 1)?;
        return numeric(&args[0]).map(Value::duration);
    }
    ops::arity(args, 0)?;
    let mut parts = [0; 5];
    for (key, value) in keywords {
        ctx.charge(1)?;
        let index = match key.as_bytes() {
            Some(b"weeks") => 0,
            Some(b"days") => 1,
            Some(b"hours") => 2,
            Some(b"minutes") => 3,
            Some(b"seconds") => 4,
            _ => return Err(Error::new(ErrorKind::Argument, "unknown duration part")),
        };
        parts[index] = numeric(value)?;
    }
    let total = parts
        .into_iter()
        .zip([604800i64, 86400, 3600, 60, 1])
        .fold(0i64, |total, (part, factor)| {
            total.wrapping_add(part.wrapping_mul(factor))
        });
    Ok(Value::duration(total))
}

pub(crate) fn parse(ctx: &mut CallContext, args: &[Value]) -> Result<Value> {
    ops::arity(args, 1)?;
    let Kind::Bytes(bytes) = &args[0].0 else {
        return Err(Error::new(
            ErrorKind::Type,
            "Duration.parse expects a string",
        ));
    };
    parse::parse(ctx, &bytes.data).map(Value::duration)
}

pub(crate) fn text(ctx: &mut CallContext, seconds: i64) -> Result<Value> {
    let mut out = json::Number::new();
    write!(out, "{seconds}s").unwrap();
    ctx.bytes(out.bytes())
}

fn components(seconds: i64) -> [i64; 4] {
    let magnitude = if seconds < 0 {
        seconds.wrapping_neg()
    } else {
        seconds
    };
    [
        magnitude / 86400,
        magnitude % 86400 / 3600,
        magnitude % 3600 / 60,
        magnitude % 60,
    ]
}

fn iso8601(ctx: &mut CallContext, seconds: i64) -> Result<Value> {
    if seconds == 0 {
        return ctx.bytes(b"PT0S");
    }
    let mut out = json::Number::new();
    if seconds < 0 {
        out.write_char('-').unwrap();
    }
    out.write_char('P').unwrap();
    let [days, hours, minutes, seconds] = components(seconds);
    if days > 0 {
        write!(out, "{days}D").unwrap();
    }
    if hours > 0 || minutes > 0 || seconds > 0 {
        out.write_char('T').unwrap();
        for (part, suffix) in [(hours, 'H'), (minutes, 'M'), (seconds, 'S')] {
            if part > 0 {
                write!(out, "{part}{suffix}").unwrap();
            }
        }
    }
    ctx.bytes(out.bytes())
}

fn parts(ctx: &mut CallContext, seconds: i64) -> Result<Value> {
    let sign = if seconds < 0 { -1 } else { 1 };
    let mut hash = Hash::empty();
    for (name, part) in ["days", "hours", "minutes", "seconds"]
        .into_iter()
        .zip(components(seconds))
    {
        let key = ctx.bytes(name.as_bytes())?;
        hash.insert(ctx, key, Value::int(part * sign))?;
    }
    Ok(Value(Kind::Hash(hash.into_arc(ctx)?)))
}

pub(crate) fn order(left: i64, right: i64) -> Ordering {
    // Preserve the reference's signed subtraction at the duration boundary.
    left.wrapping_sub(right).cmp(&0)
}

fn shifted(value: u128, bits: u32) -> Option<u128> {
    if value == 0 {
        Some(0)
    } else if bits >= 128 || bits > value.leading_zeros() {
        None
    } else {
        Some(value << bits)
    }
}

fn rounded_ratio(numerator: u128, denominator: u128) -> u128 {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    quotient + u128::from(remainder >= denominator - remainder)
}

fn scale(seconds: i64, factor: f64, divide: bool) -> Result<i64> {
    if !factor.is_finite() {
        return Err(Error::new(
            ErrorKind::Type,
            "duration factor must be finite",
        ));
    }
    if divide && factor == 0.0 {
        return Err(Error::new(ErrorKind::Arithmetic, "division by zero")
            .with_class(crate::ErrorClass::ZeroDivision));
    }
    if seconds == 0 || factor == 0.0 {
        return Ok(0);
    }
    let negative = (seconds < 0) != factor.is_sign_negative();
    let bits = factor.to_bits();
    let encoded_exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa =
        u128::from(bits & ((1u64 << 52) - 1)) | if encoded_exponent == 0 { 0 } else { 1 << 52 };
    let exponent = encoded_exponent.max(1) - 1023 - 52;
    let magnitude = u128::from(seconds.unsigned_abs());
    // An i64 and a binary64 mantissa need at most 117 bits. Work with that
    // exact ratio so float scaling never first rounds large seconds to f64.
    let rounded = if divide {
        if exponent >= 0 {
            let Some(denominator) = shifted(mantissa, exponent as u32) else {
                return Ok(0);
            };
            rounded_ratio(magnitude, denominator)
        } else {
            let numerator = shifted(magnitude, (-exponent) as u32).ok_or_else(overflow)?;
            rounded_ratio(numerator, mantissa)
        }
    } else {
        let product = magnitude * mantissa;
        if exponent >= 0 {
            shifted(product, exponent as u32).ok_or_else(overflow)?
        } else if exponent <= -128 {
            0
        } else {
            rounded_ratio(product, 1u128 << -exponent)
        }
    };
    if rounded > i64::MAX as u128 + u128::from(negative) {
        return Err(overflow());
    }
    Ok(if negative {
        -(rounded as i128)
    } else {
        rounded as i128
    } as i64)
}

pub(crate) fn binary(op: &str, left: &Value, right: &Value) -> Result<Value> {
    let seconds = match (&left.0, &right.0, op) {
        (Kind::Duration(a), Kind::Duration(b), "+") => a.checked_add(*b).ok_or_else(overflow)?,
        (Kind::Duration(a), Kind::Duration(b), "-") => a.checked_sub(*b).ok_or_else(overflow)?,
        (Kind::Duration(a), Kind::Duration(b), "/" | "%") => {
            if *b == 0 {
                return Err(Error::new(ErrorKind::Arithmetic, "division by zero")
                    .with_class(crate::ErrorClass::ZeroDivision));
            }
            if op == "/" {
                return Ok(Value::float(*a as f64 / *b as f64));
            }
            a.checked_rem(*b).unwrap_or(0)
        }
        (Kind::Duration(seconds), Kind::Int(_) | Kind::Big(_) | Kind::Float(_), "+") => {
            seconds.checked_add(numeric(right)?).ok_or_else(overflow)?
        }
        (Kind::Int(_) | Kind::Big(_) | Kind::Float(_), Kind::Duration(seconds), "+") => {
            seconds.checked_add(numeric(left)?).ok_or_else(overflow)?
        }
        (Kind::Duration(seconds), Kind::Int(_) | Kind::Big(_) | Kind::Float(_), "-") => {
            seconds.checked_sub(numeric(right)?).ok_or_else(overflow)?
        }
        (Kind::Duration(seconds), Kind::Float(factor), "*" | "/") => {
            scale(*seconds, *factor, op == "/")?
        }
        (Kind::Float(factor), Kind::Duration(seconds), "*") => scale(*seconds, *factor, false)?,
        (Kind::Duration(seconds), Kind::Int(_) | Kind::Big(_), "*") => {
            seconds.checked_mul(numeric(right)?).ok_or_else(overflow)?
        }
        (Kind::Int(_) | Kind::Big(_), Kind::Duration(seconds), "*") => {
            seconds.checked_mul(numeric(left)?).ok_or_else(overflow)?
        }
        (Kind::Duration(seconds), Kind::Int(_) | Kind::Big(_), "/") => {
            let divisor = numeric(right)?;
            if divisor == 0 {
                return Err(Error::new(ErrorKind::Arithmetic, "division by zero")
                    .with_class(crate::ErrorClass::ZeroDivision));
            }
            seconds.checked_div(divisor).ok_or_else(overflow)?
        }
        _ => return Err(Error::new(ErrorKind::Type, "unsupported duration operands")),
    };
    Ok(Value::duration(seconds))
}

fn unit(name: &str) -> Option<i64> {
    match name {
        "second" | "seconds" => Some(1),
        "minute" | "minutes" => Some(60),
        "hour" | "hours" => Some(3600),
        "day" | "days" => Some(86400),
        "week" | "weeks" => Some(604800),
        _ => None,
    }
}

fn property(site: CallSite, keywords: bool, block: bool) -> Result<()> {
    if site.auto && !keywords && !block {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Type,
            "attempted to call non-callable value",
        ))
    }
}

pub(crate) fn member(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if site.scope {
        return Ok(None);
    }
    if matches!(receiver.0, Kind::Int(_) | Kind::Big(_)) {
        if let Some(factor) = unit(name) {
            property(site, keywords, block)?;
            return Ok(Some(Value::duration(
                numeric(receiver)?.wrapping_mul(factor),
            )));
        }
    }
    let Kind::Duration(seconds) = receiver.0 else {
        return Ok(None);
    };
    let value = match name {
        "after" | "since" | "from_now" | "ago" | "before" | "until" => {
            if site.auto && !block {
                return Err(Error::new(
                    ErrorKind::Type,
                    "duration anchor is a method and requires a call",
                ));
            }
            if keywords {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "duration anchor requires a call without keyword arguments",
                ));
            }
            crate::time::anchor(
                ctx,
                seconds,
                args,
                matches!(name, "ago" | "before" | "until"),
            )?
        }
        "nil?" | "itself" | "dup" | "equal?" | "eql?" => {
            if keywords || (block && name != "eql?") {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "unsupported duration arguments",
                ));
            }
            let equality = matches!(name, "equal?" | "eql?");
            ops::arity(args, usize::from(equality))?;
            if equality {
                Value::boolean(args[0].as_duration() == Some(seconds))
            } else if name == "nil?" {
                Value::boolean(false)
            } else {
                receiver.clone()
            }
        }
        "to_s" | "string" | "inspect" => {
            if keywords || block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "unsupported duration arguments",
                ));
            }
            ops::arity(args, 0)?;
            text(ctx, seconds)?
        }
        "between?" => {
            if keywords || block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "unsupported duration arguments",
                ));
            }
            ops::arity(args, 2)?;
            let lower = ops::compare(ctx, receiver, &args[0])?;
            Value::boolean(
                matches!(lower, Some(Ordering::Equal | Ordering::Greater))
                    && matches!(
                        ops::compare(ctx, receiver, &args[1])?,
                        Some(Ordering::Equal | Ordering::Less)
                    ),
            )
        }
        _ => {
            property(site, keywords, block)?;
            if let Some(factor) = unit(name) {
                Value::int(seconds / factor)
            } else {
                match name {
                    "in_seconds" => Value::float(seconds as f64),
                    "in_minutes" => Value::float(seconds as f64 / 60.0),
                    "in_hours" => Value::float(seconds as f64 / 3600.0),
                    "in_days" => Value::float(seconds as f64 / 86400.0),
                    "in_weeks" => Value::float(seconds as f64 / 604800.0),
                    "in_months" => Value::float(seconds as f64 / 2592000.0),
                    "in_years" => Value::float(seconds as f64 / 31536000.0),
                    "to_i" => Value::int(seconds),
                    "iso8601" => iso8601(ctx, seconds)?,
                    "parts" => parts(ctx, seconds)?,
                    "format" => text(ctx, seconds)?,
                    _ => return Ok(None),
                }
            }
        }
    };
    Ok(Some(value))
}
