use crate::{CallContext, Error, ErrorKind, Result, Value, integer, json, ops, value::Kind};
use std::{cmp::Ordering, fmt::Write};

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
) -> Result<Option<Value>> {
    if !matches!(receiver.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
        return Ok(None);
    }
    let kind = receiver.type_name();
    let nullary = || {
        if args.is_empty() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::Argument,
                format!("{kind}.{name} does not take arguments"),
            ))
        }
    };
    let value = match name {
        "zero?" | "positive?" | "negative?" | "nonzero?" => {
            nullary()?;
            let number = receiver.as_float().unwrap();
            match name {
                "zero?" => Value::boolean(number == 0.0),
                "positive?" => Value::boolean(number > 0.0),
                "negative?" => Value::boolean(number < 0.0),
                _ if number == 0.0 => Value::nil(),
                _ => receiver.clone(),
            }
        }
        "nan?" | "infinite?" | "finite?" if matches!(receiver.0, Kind::Float(_)) => {
            nullary()?;
            let number = receiver.as_float().unwrap();
            match name {
                "nan?" => Value::boolean(number.is_nan()),
                "finite?" => Value::boolean(number.is_finite()),
                _ if number.is_infinite() => Value::int(if number < 0.0 { -1 } else { 1 }),
                _ => Value::nil(),
            }
        }
        "succ" | "pred" if receiver.is_integer() => {
            nullary()?;
            calculate(
                ctx,
                if name == "pred" { "-" } else { "+" },
                receiver,
                &Value::int(1),
            )?
        }
        "round" | "floor" | "ceil" => round(ctx, receiver, args, &format!("{kind}.{name}"))?,
        "div" | "divmod" | "fdiv" | "remainder" => {
            let method = format!("{kind}.{name}");
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("{method} expects one numeric argument"),
                ));
            }
            if args[0].as_float().is_none() {
                return Err(Error::new(
                    ErrorKind::Type,
                    format!("{method} expects a numeric argument"),
                ));
            }
            division(ctx, &method, receiver, &args[0])?
        }
        "clamp" => clamp(ctx, &format!("{kind}.clamp"), receiver, args)?,
        "between?" => {
            crate::arguments::between(&format!("{kind}.between?"), args, false, false)?;
            let lower = ops::compare(ctx, &args[0], receiver)?;
            let inside = matches!(lower, Some(Ordering::Less | Ordering::Equal))
                && matches!(
                    ops::compare(ctx, receiver, &args[1])?,
                    Some(Ordering::Less | Ordering::Equal)
                );
            Value::boolean(inside)
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

fn calculate(ctx: &mut CallContext, op: &'static str, a: &Value, b: &Value) -> Result<Value> {
    ops::binary(ctx, op, a.clone(), b.clone())
}

/// Runs a numeric division member; `method` is its full name, such as `int.div`.
fn division(ctx: &mut CallContext, method: &str, a: &Value, b: &Value) -> Result<Value> {
    let name = method.rsplit('.').next().unwrap_or(method);
    if name == "fdiv" {
        return Ok(Value::float(a.as_float().unwrap() / b.as_float().unwrap()));
    }
    if b.as_float() == Some(0.0) {
        return Err(zero_division(method));
    }
    if a.is_integer() && b.is_integer() {
        return match name {
            "div" => calculate(ctx, "//", a, b),
            "remainder" => match (a.as_int(), b.as_int()) {
                (Some(a), Some(b)) => Ok(Value::int(a.checked_rem(b).unwrap_or(0))),
                _ => integer::binary(ctx, "remainder", a, b),
            },
            "divmod" => {
                let (q, r) = if a.as_int().is_some() && b.as_int().is_some() {
                    (calculate(ctx, "//", a, b)?, calculate(ctx, "%", a, b)?)
                } else {
                    integer::divmod(ctx, a, b)?
                };
                ctx.array(&[q, r])
            }
            _ => unreachable!(),
        };
    }
    let a = a.as_float().unwrap();
    let b = b.as_float().unwrap();
    if name == "div" {
        return integer::from_float(ctx, (a / b).floor(), method);
    }
    let remainder = a % b;
    if name == "remainder" {
        return Ok(Value::float(
            if b.is_infinite()
                && a.is_finite()
                && a != 0.0
                && a.is_sign_negative() != b.is_sign_negative()
            {
                f64::NAN
            } else {
                remainder
            },
        ));
    }
    let modulo = if remainder != 0.0 && (remainder < 0.0) != (b < 0.0) {
        remainder + b
    } else {
        remainder
    };
    let quotient = if b.is_infinite() && a.is_finite() {
        Value::int(if a != 0.0 && (a < 0.0) != (b < 0.0) {
            -1
        } else {
            0
        })
    } else {
        integer::from_float(ctx, ((a - modulo) / b).round(), method)?
    };
    ctx.array(&[quotient, Value::float(modulo)])
}

fn zero_division(method: &str) -> Error {
    Error::new(ErrorKind::Arithmetic, format!("{method} by zero"))
        .with_class(crate::ErrorClass::ZeroDivision)
}

fn exact_order(ctx: &mut CallContext, method: &str, a: &Value, b: &Value) -> Result<Ordering> {
    let order = match (&a.0, &b.0) {
        (Kind::Float(a), Kind::Float(b)) => Some(ops::float_order(*a, *b)),
        (Kind::Int(_) | Kind::Big(_), Kind::Float(b)) => integer::compare_float(ctx, a, *b)?,
        (Kind::Float(a), Kind::Int(_) | Kind::Big(_)) => {
            integer::compare_float(ctx, b, *a)?.map(Ordering::reverse)
        }
        _ => ops::compare(ctx, a, b)?,
    };
    order.ok_or_else(|| {
        Error::new(
            ErrorKind::Argument,
            format!("{method} values must not be NaN"),
        )
    })
}

fn clamp(ctx: &mut CallContext, method: &str, receiver: &Value, args: &[Value]) -> Result<Value> {
    let (lower, upper) = match args {
        [Value(Kind::Range(range))] if !range.exclusive => {
            (range.start.map(Value::int), range.end.map(Value::int))
        }
        [Value(Kind::Range(_))] => {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{method} cannot clamp with exclusive range"),
            ));
        }
        [lower, upper] => {
            let bound = |value: &Value| match &value.0 {
                Kind::Nil => Ok(None),
                Kind::Int(_) | Kind::Big(_) | Kind::Float(_) => Ok(Some(value.clone())),
                _ => Err(Error::new(
                    ErrorKind::Type,
                    format!("{method} bounds must be numeric or nil"),
                )),
            };
            (bound(lower)?, bound(upper)?)
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{method} expects min and max or range"),
            ));
        }
    };
    if let (Some(lower), Some(upper)) = (&lower, &upper) {
        if exact_order(ctx, method, lower, upper)? == Ordering::Greater {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{method} min must be <= max"),
            ));
        }
    }
    if let Some(lower) = lower {
        if exact_order(ctx, method, receiver, &lower)? == Ordering::Less {
            return Ok(lower);
        }
    }
    if let Some(upper) = upper {
        if exact_order(ctx, method, receiver, &upper)? == Ordering::Greater {
            return Ok(upper);
        }
    }
    Ok(receiver.clone())
}

#[derive(Clone, Copy)]
enum Rounding {
    Nearest,
    Floor,
    Ceil,
}

/// Rounds a number for `method`, such as `float.round`, reading its precision as Go does.
fn round(ctx: &mut CallContext, receiver: &Value, args: &[Value], method: &str) -> Result<Value> {
    let range = |direction: &str, value: Option<i64>| {
        Error::new(
            ErrorKind::Argument,
            match value {
                Some(value) => {
                    format!("{method} precision {value} too {direction} to convert to int")
                }
                None => format!("{method} precision too {direction} to convert to int"),
            },
        )
    };
    let digits = match args {
        [] => 0,
        [Value(Kind::Int(n))] => {
            i32::try_from(*n).map_err(|_| range(if *n > 0 { "big" } else { "small" }, Some(*n)))?
        }
        [Value(Kind::Big(n))] => {
            return Err(range(if n.negative { "small" } else { "big" }, None));
        }
        [_] => {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{method} precision must be an Integer"),
            ));
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{method} expects at most one precision argument"),
            ));
        }
    };
    let mode = match method.rsplit('.').next() {
        Some("floor") => Rounding::Floor,
        Some("ceil") => Rounding::Ceil,
        _ => Rounding::Nearest,
    };
    if receiver.is_integer() {
        return integer_round(ctx, receiver, digits, mode);
    }
    let number = receiver.as_float().unwrap();
    if digits > 0 {
        return float_digits(ctx, number, digits, mode).map(Value::float);
    }
    let whole = match mode {
        Rounding::Floor => number.floor(),
        Rounding::Ceil => number.ceil(),
        Rounding::Nearest if digits == 0 => number.round(),
        Rounding::Nearest => number.trunc(),
    };
    let whole = integer::from_float(ctx, whole, method)?;
    integer_round(ctx, &whole, digits, mode)
}

fn pow10(ctx: &mut CallContext, digits: u32) -> Result<Value> {
    integer::binary(ctx, "**", &Value::int(10), &Value::int(digits as i64))
}

fn integer_round(
    ctx: &mut CallContext,
    value: &Value,
    digits: i32,
    mode: Rounding,
) -> Result<Value> {
    if digits >= 0 || value.as_int() == Some(0) {
        return Ok(value.clone());
    }
    let negative = match &value.0 {
        Kind::Int(n) => *n < 0,
        Kind::Big(n) => n.negative,
        _ => unreachable!(),
    };
    let away =
        matches!(mode, Rounding::Floor) && negative || matches!(mode, Rounding::Ceil) && !negative;
    let digits = digits.unsigned_abs();
    // This conservative bit bound proves the bucket exceeds twice the magnitude.
    // Extreme precisions that return zero never need to construct that bucket.
    if digits as usize > integer::bits(value) / 3 + 1 && !away {
        return Ok(Value::int(0));
    }
    let bucket = pow10(ctx, digits)?;
    let remainder = division(ctx, "int.remainder", value, &bucket)?;
    let base = calculate(ctx, "-", value, &remainder)?;
    let round_away = if matches!(mode, Rounding::Nearest) {
        let twice = calculate(
            ctx,
            "*",
            &remainder,
            &Value::int(if negative { -2 } else { 2 }),
        )?;
        integer::compare(ctx, &twice, &bucket)? != Ordering::Less
    } else {
        away && remainder.as_int() != Some(0)
    };
    if round_away {
        calculate(ctx, if negative { "-" } else { "+" }, &base, &bucket)
    } else {
        Ok(base)
    }
}

// Decimal corrections and precision guards follow Go Vibescript's rounding.go.
// The shared MIT license is included in LICENSE.
fn float_digits(ctx: &mut CallContext, number: f64, digits: i32, mode: Rounding) -> Result<f64> {
    if number == 0.0 || !number.is_finite() {
        return Ok(number);
    }
    let bits = number.abs().to_bits();
    let raw = ((bits >> 52) & 0x7ff) as i32;
    let exponent = if raw == 0 {
        64 - bits.leading_zeros() as i32 - 1074
    } else {
        raw - 1022
    };
    let overflow = if exponent > 0 {
        17 - exponent / 4
    } else {
        18 - exponent / 3
    };
    if digits >= overflow {
        return Ok(number);
    }
    let underflow = digits
        < if exponent > 0 {
            -(exponent / 3 + 1)
        } else {
            -(exponent / 4)
        };
    if underflow
        && match mode {
            Rounding::Nearest => true,
            Rounding::Floor => number > 0.0,
            Rounding::Ceil => number < 0.0,
        }
    {
        return Ok(0.0);
    }
    if matches!(mode, Rounding::Nearest) && digits >= 15 {
        return rational_round(ctx, number, digits as u32);
    }
    let scale = pow10(ctx, digits as u32)?.as_float().unwrap();
    Ok(match mode {
        Rounding::Floor => {
            let whole = (number * scale).floor();
            let corrected = (whole + 1.0) / scale;
            if corrected > number {
                whole / scale
            } else {
                corrected
            }
        }
        Rounding::Ceil => (number * scale).ceil() / scale,
        Rounding::Nearest => {
            let mut whole = (number * scale).round();
            if number > 0.0 && (whole + 0.5) / scale <= number {
                whole += 1.0;
            } else if number < 0.0 && (whole - 0.5) / scale >= number {
                whole -= 1.0;
            }
            whole / scale
        }
    })
}

fn rational_round(ctx: &mut CallContext, number: f64, digits: u32) -> Result<f64> {
    let bits = number.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = bits & ((1u64 << 52) - 1) | if exponent == 0 { 0 } else { 1u64 << 52 };
    let shift = exponent.max(1) - 1075;
    let scale = pow10(ctx, digits)?;
    let numerator = calculate(ctx, "*", &Value::int(mantissa as i64), &scale)?;
    let power = integer::binary(
        ctx,
        "**",
        &Value::int(2),
        &Value::int(shift.unsigned_abs() as i64),
    )?;
    let mut rounded = if shift >= 0 {
        calculate(ctx, "*", &numerator, &power)?
    } else {
        let (quotient, remainder) = integer::divmod(ctx, &numerator, &power)?;
        let twice = calculate(ctx, "*", &remainder, &Value::int(2))?;
        if integer::compare(ctx, &twice, &power)? == Ordering::Less {
            quotient
        } else {
            calculate(ctx, "+", &quotient, &Value::int(1))?
        }
    };
    if number < 0.0 {
        rounded = ops::unary(ctx, "-", rounded)?;
    }
    // A decimal token gives the final division one correctly rounded conversion.
    // The precision guards bound this token and all preceding scratch storage.
    let mut text = integer::format(ctx, &rounded, 10)?;
    let mut suffix = json::Number::new();
    write!(suffix, "e-{digits}").unwrap();
    text.extend(ctx, suffix.bytes())?;
    ctx.work_bytes(text.data.len())?;
    std::str::from_utf8(&text.data)
        .unwrap()
        .parse()
        .map_err(|_| Error::new(ErrorKind::Arithmetic, "float rounding failed"))
}
