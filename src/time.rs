use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    bytecode::CallSite,
    ops,
    value::Kind,
};
use std::{
    cmp::Ordering,
    mem::size_of,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

mod calendar;
mod construct;
mod format;
mod parse;
mod strftime;
mod zone;

pub(crate) use construct::Constructor;
pub(crate) use format::text;

const NANOS: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Stamp([u8; 12]);

impl Stamp {
    pub fn new(seconds: i64, nanos: u32) -> Self {
        debug_assert!(nanos < NANOS as u32);
        let mut bytes = [0; 12];
        bytes[..8].copy_from_slice(&seconds.to_le_bytes());
        bytes[8..].copy_from_slice(&nanos.to_le_bytes());
        Self(bytes)
    }
    pub fn seconds(self) -> i64 {
        i64::from_le_bytes(self.0[..8].try_into().unwrap())
    }
    pub fn nanos(self) -> u32 {
        u32::from_le_bytes(self.0[8..].try_into().unwrap())
    }
    fn internal(self) -> i64 {
        self.seconds().wrapping_add(calendar::UNIX_TO_INTERNAL)
    }
    pub fn order(self, other: Self) -> Ordering {
        self.internal()
            .cmp(&other.internal())
            .then_with(|| self.nanos().cmp(&other.nanos()))
    }
    fn add(self, delta: i64) -> Self {
        let nanos = i64::from(self.nanos()) + delta % NANOS;
        let seconds = delta / NANOS + nanos.div_euclid(NANOS);
        // Go saturates the internal epoch's seconds, retaining the normalized fraction.
        let internal = self
            .internal()
            .checked_add(seconds)
            .unwrap_or(if seconds > 0 { i64::MAX } else { -i64::MAX });
        Self::new(
            internal.wrapping_sub(calendar::UNIX_TO_INTERNAL),
            nanos.rem_euclid(NANOS) as u32,
        )
    }
    fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => Self::new(duration.as_secs() as i64, duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                let fraction = duration.subsec_nanos();
                Self::new(
                    -(duration.as_secs() as i64) - i64::from(fraction != 0),
                    if fraction == 0 {
                        0
                    } else {
                        NANOS as u32 - fraction
                    },
                )
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Zoned {
    stamp: Stamp,
    zone: Arc<zone::Zone>,
    header: Option<Charge>,
}

impl Zoned {
    fn new(ctx: &mut CallContext, stamp: Stamp, zone: Arc<zone::Zone>) -> Result<Arc<Self>> {
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            stamp,
            zone,
            header,
        }))
    }
    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) {
            return Ok(value.clone());
        }
        let zone = zone::Zone::import(ctx, &value.zone)?;
        Self::new(ctx, value.stamp, zone)
    }
}

pub(crate) fn stamp(value: &Value) -> Option<Stamp> {
    match &value.0 {
        Kind::Time(stamp) => Some(*stamp),
        Kind::Zoned(value) => Some(value.stamp),
        _ => None,
    }
}

fn value(ctx: &mut CallContext, stamp: Stamp, zone: Option<Arc<zone::Zone>>) -> Result<Value> {
    Ok(Value(if let Some(zone) = zone {
        Kind::Zoned(Zoned::new(ctx, stamp, zone)?)
    } else {
        Kind::Time(stamp)
    }))
}

fn zone(value: &Value) -> Option<Arc<zone::Zone>> {
    if let Kind::Zoned(value) = &value.0 {
        Some(value.zone.clone())
    } else {
        None
    }
}

fn offset<'a>(ctx: &mut CallContext, value: &'a Value) -> Result<zone::Offset<'a>> {
    if let Kind::Zoned(value) = &value.0 {
        value.zone.lookup(ctx, value.stamp.seconds())
    } else {
        Ok(zone::Offset::utc())
    }
}

fn overflow() -> Error {
    Error::new(ErrorKind::Arithmetic, "time offset out of 64-bit range")
}

fn absent_zone(value: &Value) -> bool {
    matches!(&value.0, Kind::Nil) || matches!(&value.0, Kind::Bytes(b) if b.data().is_empty())
}

fn location(
    ctx: &mut CallContext,
    input: Option<&Value>,
    local: bool,
) -> Result<Option<Arc<zone::Zone>>> {
    if let Some(input) = input.filter(|v| !absent_zone(v)) {
        return zone::Zone::parse(ctx, input);
    }
    if local {
        zone::Zone::local(ctx).map(Some)
    } else {
        Ok(None)
    }
}

fn scaled_float(value: f64, factor: u32) -> Result<i64> {
    if !value.is_finite() {
        return Err(overflow());
    }
    let bits = value.to_bits();
    let encoded = ((bits >> 52) & 0x7ff) as i32;
    let mantissa = u128::from(bits & ((1u64 << 52) - 1)) | if encoded == 0 { 0 } else { 1 << 52 };
    let product = mantissa * u128::from(factor);
    if product == 0 {
        return Ok(0);
    }
    let exponent = encoded.max(1) - 1023 - 52;
    let (whole, remainder) = if exponent >= 0 {
        if exponent as u32 >= 128 || exponent as u32 > product.leading_zeros() {
            return Err(overflow());
        }
        (product << exponent, false)
    } else if exponent <= -128 {
        (0, true)
    } else {
        (
            product >> -exponent,
            product & ((1u128 << -exponent) - 1) != 0,
        )
    };
    let negative = value.is_sign_negative();
    let magnitude = whole + u128::from(negative && remainder);
    if magnitude > i64::MAX as u128 + u128::from(negative) {
        return Err(overflow());
    }
    Ok(if negative {
        -(magnitude as i128)
    } else {
        magnitude as i128
    } as i64)
}

fn argument(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Argument, message)
}

/// Reads the optional fractional-digit precision of `method`, as Go's
/// `timeISO8601Precision` and `timeRoundingUnit` do.
fn precision(method: &str, args: &[Value], keywords: bool) -> Result<i64> {
    if keywords {
        return Err(argument(format!(
            "{method} does not accept keyword arguments"
        )));
    }
    let Some(arg) = args.first() else {
        return Ok(0);
    };
    if args.len() > 1 {
        return Err(argument(format!(
            "{method} expects at most one precision argument"
        )));
    }
    let precision = match arg.0 {
        Kind::Int(n) => n,
        Kind::Big(_) => {
            return Err(Error::new(
                ErrorKind::Type,
                format!("{method} precision must fit in a 64-bit integer"),
            ));
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Type,
                format!("{method} precision must be an Integer"),
            ));
        }
    };
    if precision < 0 {
        return Err(argument(format!("{method} precision must be non-negative")));
    }
    Ok(precision)
}

/// The time `seconds` after `start`, or before it when `before` is set;
/// without a start, counted from now.
pub(crate) fn anchor(
    ctx: &mut CallContext,
    seconds: i64,
    start: Option<&Value>,
    before: bool,
) -> Result<Value> {
    ctx.checkpoint()?;
    let name = if before { "before" } else { "after" };
    let start =
        match start {
            None => Stamp::now(),
            Some(input) => match &input.0 {
                Kind::Bytes(bytes) => parse::rfc3339(ctx, bytes.data()).map_err(|error| {
                    match parse::rfc3339_rejection(bytes.data())
                        .filter(|_| error.kind == ErrorKind::Argument)
                    {
                        Some(text) => {
                            Error::new(ErrorKind::Argument, format!("invalid time: {text}"))
                        }
                        None => error,
                    }
                })?,
                _ => stamp(input).ok_or_else(|| {
                    Error::new(
                        ErrorKind::Type,
                        format!("{name} expects a Time or RFC3339 string"),
                    )
                })?,
            },
        };
    // Duration anchors retain Go's wrapping nanosecond conversion, unlike Time arithmetic.
    let delta = seconds.wrapping_mul(NANOS);
    Ok(Value(Kind::Time(start.add(if before {
        delta.wrapping_neg()
    } else {
        delta
    }))))
}

/// Reports an offset outside the 64-bit nanosecond domain, naming the operation as Go does.
fn range_error(method: &str) -> Error {
    Error::new(
        ErrorKind::Arithmetic,
        format!("{method} result out of int64 range"),
    )
}

pub(crate) fn binary(
    ctx: &mut CallContext,
    op: &str,
    left: &Value,
    right: &Value,
) -> Result<Value> {
    if let (Some(left), Some(right), "-") = (stamp(left), stamp(right), op) {
        let seconds = left
            .seconds()
            .checked_sub(right.seconds())
            .ok_or_else(|| range_error("time subtraction"))?;
        return Ok(Value::float(
            seconds as f64
                + (i64::from(left.nanos()) - i64::from(right.nanos())) as f64 / NANOS as f64,
        ));
    }
    let (time, number, negate) = if stamp(left).is_some() && matches!(op, "+" | "-") {
        (left, right, op == "-")
    } else if stamp(right).is_some() && op == "+" {
        (right, left, false)
    } else {
        return Err(ops::unsupported(op));
    };
    let overflow = || {
        range_error(if negate {
            "time subtraction"
        } else {
            "time addition"
        })
    };
    let delta = match number.0 {
        Kind::Int(n) | Kind::Duration(n) => {
            let n = if negate {
                n.checked_neg().ok_or_else(overflow)?
            } else {
                n
            };
            n.checked_mul(NANOS).ok_or_else(overflow)?
        }
        Kind::Float(n) => {
            scaled_float(if negate { -n } else { n }, NANOS as u32).map_err(|_| overflow())?
        }
        Kind::Big(_) => return Err(overflow()),
        _ => return Err(ops::unsupported(op)),
    };
    value(ctx, stamp(time).unwrap().add(delta), zone(time))
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
    let Some(time) = stamp(receiver) else {
        return Ok(None);
    };
    if site.scope {
        return Ok(None);
    }
    let result = match name {
        "dup" => {
            crate::members::dup_shape(args.len(), keywords, block)?;
            return Ok(Some(receiver.clone()));
        }
        "between?" => {
            crate::arguments::between("time.between?", args, keywords, block)?;
            Value::boolean(
                matches!(
                    ops::compare(ctx, receiver, &args[0])?,
                    Some(Ordering::Equal | Ordering::Greater)
                ) && matches!(
                    ops::compare(ctx, receiver, &args[1])?,
                    Some(Ordering::Equal | Ordering::Less)
                ),
            )
        }
        "to_s" | "inspect" => {
            crate::arguments::nullary(&format!("time.{name}"), args, keywords, block)?;
            text(ctx, receiver, None)?
        }
        "format" | "strftime" => {
            if site.auto && !block {
                return Err(Error::new(
                    ErrorKind::Type,
                    format!(
                        "{name} is a method and cannot be used as a value; call it with {name}(...)"
                    ),
                ));
            }
            if keywords {
                return Err(argument(format!(
                    "time.{name} does not accept keyword arguments"
                )));
            }
            let layout = match args {
                [Value(Kind::Bytes(layout))] => layout,
                _ if name == "format" => {
                    return Err(argument("format expects a Go layout string"));
                }
                _ => return Err(argument("time.strftime expects a format string")),
            };
            if name == "format" {
                format::format(ctx, receiver, layout.data())?
            } else {
                strftime::format(ctx, receiver, layout.data())?
            }
        }
        "iso8601" => {
            let precision = precision(&format!("time.{name}"), args, keywords)?;
            if precision > 100 {
                return ctx.guard(
                    ErrorKind::OutputLimit,
                    &format!("time.{name} precision exceeds maximum 100 digits"),
                );
            }
            text(ctx, receiver, Some(precision as usize))?
        }
        "httpdate" | "rfc2822" => {
            if keywords {
                return Err(argument(format!(
                    "time.{name} does not accept keyword arguments"
                )));
            }
            if !args.is_empty() {
                return Err(argument(format!("time.{name} does not accept arguments")));
            }
            format::mail_date(ctx, receiver, name == "httpdate")?
        }
        "localtime" => {
            if keywords {
                return Err(argument(format!(
                    "{name} does not take keyword arguments; pass the offset positionally"
                )));
            }
            if args.len() > 1 {
                return Err(argument(format!(
                    "{name} expects at most one timezone offset argument"
                )));
            }
            let zone = location(ctx, args.first(), true)?;
            value(ctx, time, zone)?
        }
        "round" | "ceil" | "floor" => {
            let precision = if name == "round" {
                precision("time.round", args, keywords)?
            } else if keywords {
                return Err(argument(format!(
                    "time.{name} does not accept keyword arguments"
                )));
            } else if !args.is_empty() {
                return Err(argument(format!("{name} does not accept precision")));
            } else {
                0
            };
            let unit = 10i64.pow(9 - precision.min(9) as u32);
            let remainder = i64::from(time.nanos()) % unit;
            let delta = if name == "floor" || (name == "round" && remainder * 2 < unit) {
                -remainder
            } else if remainder == 0 {
                0
            } else {
                unit - remainder
            };
            if delta == 0 {
                receiver.clone()
            } else {
                value(ctx, time.add(delta), zone(receiver))?
            }
        }
        _ => {
            if !site.auto || block {
                return Err(Error::new(
                    ErrorKind::Type,
                    "attempted to call non-callable value",
                ));
            }
            match name {
                "utc" => Value(Kind::Time(time)),
                "nsec" => Value::int(i64::from(time.nanos())),
                "usec" => Value::int(i64::from(time.nanos() / 1000)),
                "subsec" => Value::float(f64::from(time.nanos()) / NANOS as f64),
                "to_i" => Value::int(time.seconds()),
                "to_f" => {
                    Value::float(time.seconds() as f64 + f64::from(time.nanos()) / NANOS as f64)
                }
                _ => {
                    let offset = offset(ctx, receiver)?;
                    let date = calendar::civil(time.seconds(), offset.seconds);
                    match name {
                        "year" => Value::int(date.year),
                        "month" => Value::int(date.month),
                        "day" => Value::int(date.day),
                        "hour" => Value::int(date.hour),
                        "min" => Value::int(date.minute),
                        "sec" => Value::int(date.second),
                        "wday" => Value::int(date.weekday),
                        "yday" => Value::int(date.yearday),
                        "utc_offset" => Value::int(i64::from(offset.seconds)),
                        "zone" => ctx.bytes(offset.name)?,
                        "utc?" => Value::boolean(offset.seconds / 60 == 0),
                        "dst?" => Value::boolean(offset.dst),
                        "sunday?" | "monday?" | "tuesday?" | "wednesday?" | "thursday?"
                        | "friday?" | "saturday?" => Value::boolean(
                            [
                                "sunday?",
                                "monday?",
                                "tuesday?",
                                "wednesday?",
                                "thursday?",
                                "friday?",
                                "saturday?",
                            ][date.weekday as usize]
                                == name,
                        ),
                        "to_a" => {
                            let name = ctx.bytes(offset.name)?;
                            let mut items = Buffer::with_capacity(ctx, 10)?;
                            items.extend(
                                ctx,
                                &[
                                    Value::int(date.second),
                                    Value::int(date.minute),
                                    Value::int(date.hour),
                                    Value::int(date.day),
                                    Value::int(date.month),
                                    Value::int(date.year),
                                    Value::int(date.weekday),
                                    Value::int(date.yearday),
                                    Value::boolean(offset.dst),
                                    name,
                                ],
                            )?;
                            Value::from_array(ctx, items)?
                        }
                        _ => return Ok(None),
                    }
                }
            }
        }
    };
    Ok(Some(result))
}
