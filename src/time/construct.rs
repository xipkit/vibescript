use super::{NANOS, Stamp, argument, calendar, location, parse, scaled_float, value};
use crate::{CallContext, Error, ErrorKind, Result, Value, json, ops, value::Kind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Constructor {
    Local,
    Utc,
    At,
    Now,
    Parse,
}

impl Constructor {
    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "Time.local",
            Self::Utc => "Time.utc",
            Self::At => "Time.at",
            Self::Now => "Time.now",
            Self::Parse => "Time.parse",
        }
    }
    pub fn call(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
    ) -> Result<Value> {
        if self == Self::Parse && !(1..=2).contains(&args.len()) {
            return Err(parse::shape());
        }
        let at_shape = || {
            argument("Time.at expects seconds since epoch with optional subsecond value and unit")
        };
        let mut zone_input = None;
        for (key, val) in keywords {
            ctx.charge(1)?;
            if key.as_bytes() == Some(b"in") {
                zone_input = Some(val);
            } else if matches!(self, Self::At | Self::Parse) {
                if self == Self::At && !(1..=3).contains(&args.len()) {
                    return Err(at_shape());
                }
                return Err(argument(format!(
                    "{} unknown keyword argument {}",
                    self.name(),
                    String::from_utf8_lossy(key.as_bytes().unwrap_or_default())
                )));
            }
        }
        if self == Self::Parse {
            return parse::call(ctx, args, zone_input);
        }
        if self == Self::Now {
            if !args.is_empty() {
                return Err(argument("Time.now does not take positional arguments"));
            }
            let zone = location(ctx, zone_input, false)?;
            ctx.checkpoint()?;
            return value(ctx, Stamp::now(), zone);
        }
        if self == Self::At {
            if !(1..=3).contains(&args.len()) {
                return Err(at_shape());
            }
            let zone = location(ctx, zone_input, true)?;
            let (seconds, mut nanos) = match args[0].0 {
                Kind::Int(n) => (n, 0),
                Kind::Float(n) if n.is_finite() => {
                    let whole = raw_int(n);
                    (whole, raw_int((n - whole as f64) * NANOS as f64))
                }
                Kind::Float(_) => return Err(argument("Time.at expects a finite numeric epoch")),
                Kind::Big(_) => {
                    return Err(argument("Time.at seconds must fit in a 64-bit integer"));
                }
                _ => return Err(argument("Time.at expects numeric seconds")),
            };
            let out_of_range = || {
                Error::new(
                    ErrorKind::Arithmetic,
                    "Time.at subsecond value out of range",
                )
            };
            if args.len() >= 2 {
                let factor = if let Some(unit) = args.get(2) {
                    let Kind::Symbol(name) = &unit.0 else {
                        return Err(unexpected_unit(ctx, unit)?);
                    };
                    match name.data().as_slice() {
                        b"microsecond" => 1000,
                        b"millisecond" => 1_000_000,
                        b"nanosecond" => 1,
                        _ => return Err(unexpected_unit(ctx, unit)?),
                    }
                } else {
                    1000
                };
                let subsecond = scaled(&args[1], factor).map_err(|error| match error {
                    Subsecond::NotNumeric => argument("Time.at subsecond value must be numeric"),
                    Subsecond::NotFinite => Error::new(
                        ErrorKind::Arithmetic,
                        "Time.at expects a finite subsecond value",
                    ),
                    Subsecond::OutOfRange => out_of_range(),
                })?;
                nanos = nanos.checked_add(subsecond).ok_or_else(out_of_range)?;
            }
            let seconds = seconds
                .checked_add(nanos.div_euclid(NANOS))
                .ok_or_else(out_of_range)?;
            return value(
                ctx,
                Stamp::new(seconds, nanos.rem_euclid(NANOS) as u32),
                zone,
            );
        }
        if args.is_empty() {
            return Err(argument("Time constructor expects at least a year"));
        }
        for arg in args {
            ctx.charge(1)?;
            if matches!(arg.0, Kind::Big(_)) {
                return Err(argument(
                    "Time constructor parts must fit in a 64-bit integer",
                ));
            }
        }
        if args.len() > 7 {
            return Err(argument(
                "Time constructor expects at most year, month, day, hour, minute, second, microsecond",
            ));
        }
        let year = required_year(&args[0])?;
        let mut parts = [year, 1, 1, 0, 0, 0];
        for (i, part) in parts.iter_mut().enumerate().skip(1) {
            if let Some(arg) = args.get(i) {
                *part = match arg.0 {
                    Kind::Nil => *part,
                    Kind::Int(n) => n,
                    Kind::Float(n) => raw_int(n),
                    _ => 0,
                };
            }
        }
        let mut nanos = 0;
        if let Some(micros) = args.get(6) {
            if !matches!(micros.0, Kind::Nil) {
                let out_of_range = || {
                    argument(
                        "Time constructor microsecond argument out of range (must be within one second)",
                    )
                };
                if micros.as_float().is_some_and(|n| n < 0.0) {
                    return Err(out_of_range());
                }
                nanos = scaled(micros, 1000).map_err(|error| match error {
                    Subsecond::NotNumeric => {
                        argument("Time constructor microsecond argument must be numeric")
                    }
                    Subsecond::NotFinite | Subsecond::OutOfRange => {
                        let mut error = out_of_range();
                        error.kind = ErrorKind::Arithmetic;
                        error
                    }
                })?;
                if !(0..NANOS).contains(&nanos) {
                    return Err(out_of_range());
                }
            }
        }
        let selected = if self == Self::Local {
            location(ctx, zone_input, true)?
        } else {
            None
        };
        let seconds = if let Some(zone) = &selected {
            zone.calendar(ctx, parts)?
        } else {
            calendar::normalized(parts)
        };
        value(ctx, Stamp::new(seconds, nanos as u32), selected)
    }
}

fn raw_int(value: f64) -> i64 {
    // Go's conversion of an out-of-range float depends on the CPU; every
    // platform uses the reference's arm64 result, which saturates.
    value as i64
}

/// Scales a subsecond number; the error names a non-numeric value, a
/// non-finite float or a result beyond 64-bit nanoseconds.
fn scaled(value: &Value, factor: u32) -> std::result::Result<i64, Subsecond> {
    match value.0 {
        Kind::Int(n) => n
            .checked_mul(i64::from(factor))
            .ok_or(Subsecond::OutOfRange),
        Kind::Float(n) if !n.is_finite() => Err(Subsecond::NotFinite),
        Kind::Float(n) => scaled_float(n, factor).map_err(|_| Subsecond::OutOfRange),
        Kind::Big(_) => Err(Subsecond::OutOfRange),
        _ => Err(Subsecond::NotNumeric),
    }
}

enum Subsecond {
    NotNumeric,
    NotFinite,
    OutOfRange,
}

/// Names a `Time.at` unit Go does not accept, rendering at most 64 bytes of it.
fn unexpected_unit(ctx: &mut CallContext, unit: &Value) -> Result<Error> {
    if crate::text::bounded::measure(ctx, unit, 65, true)? > 64 {
        return Ok(argument(format!(
            "unexpected unit of type {}",
            unit.type_name()
        )));
    }
    let rendered = crate::text::bounded::prefix(ctx, unit, 64)?;
    Ok(argument(format!(
        "unexpected unit: {}",
        String::from_utf8_lossy(rendered.as_bytes().unwrap_or_default())
    )))
}

/// Reads the required year of a Time constructor, as Go's `requiredYear` does.
fn required_year(year: &Value) -> Result<i64> {
    match year.0 {
        Kind::Int(n) => Ok(n),
        Kind::Float(n) if n.is_finite() && n > i64::MIN as f64 && n < i64::MAX as f64 => {
            Ok(n as i64)
        }
        Kind::Float(n) if n.is_nan() => {
            Err(argument("Time constructor year must be finite, got NaN"))
        }
        Kind::Float(n) if n.is_infinite() => Err(argument(format!(
            "Time constructor year must be finite, got {}Inf",
            if n > 0.0 { '+' } else { '-' }
        ))),
        Kind::Float(n) => {
            let mut text = json::Number::new();
            ops::format_float(&mut text, n);
            Err(argument(format!(
                "Time constructor year {} is out of range",
                String::from_utf8_lossy(text.bytes())
            )))
        }
        _ => Err(argument(format!(
            "Time constructor year must be numeric, got {}",
            year.type_name()
        ))),
    }
}
