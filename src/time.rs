use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    bytecode::CallSite,
    json, ops,
    value::Kind,
};
use std::{
    cmp::Ordering,
    fmt::Write,
    mem::size_of,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

mod calendar;
mod zone;

const NANOS: i64 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        if ctx.owns(&value.header) || ctx.options.limits.memory_bytes.is_none() {
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

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid time arguments")
}
fn overflow() -> Error {
    Error::new(ErrorKind::Arithmetic, "time offset out of 64-bit range")
}

fn absent_zone(value: &Value) -> bool {
    matches!(&value.0, Kind::Nil) || matches!(&value.0, Kind::Bytes(b) if b.data.is_empty())
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

fn raw_int(value: f64) -> i64 {
    // Match the reference toolchain's conversion of optional calendar fields.
    #[cfg(target_arch = "x86_64")]
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value) {
        return i64::MIN;
    }
    value as i64
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

fn scaled(value: &Value, factor: u32) -> Result<i64> {
    match value.0 {
        Kind::Int(n) => n.checked_mul(i64::from(factor)).ok_or_else(overflow),
        Kind::Float(n) => scaled_float(n, factor),
        Kind::Big(_) => Err(overflow()),
        _ => Err(invalid()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Constructor {
    New,
    Local,
    Mktime,
    Utc,
    Gm,
    At,
    Now,
}

impl Constructor {
    pub fn name(self) -> &'static str {
        match self {
            Self::New => "Time.new",
            Self::Local => "Time.local",
            Self::Mktime => "Time.mktime",
            Self::Utc => "Time.utc",
            Self::Gm => "Time.gm",
            Self::At => "Time.at",
            Self::Now => "Time.now",
        }
    }
    pub fn auto(self) -> bool {
        matches!(self, Self::Mktime | Self::Gm | Self::Now)
    }
    pub fn call(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
    ) -> Result<Value> {
        let mut zone_input = None;
        for (key, val) in keywords {
            ctx.charge(1)?;
            if key.as_bytes() == Some(b"in") {
                zone_input = Some(val);
            } else if self == Self::At {
                return Err(invalid());
            }
        }
        if self == Self::Now {
            ops::arity(args, 0)?;
            let zone = location(ctx, zone_input, false)?;
            ctx.checkpoint()?;
            return value(ctx, Stamp::now(), zone);
        }
        if self == Self::At {
            if !(1..=3).contains(&args.len()) {
                return Err(invalid());
            }
            let zone = location(ctx, zone_input, true)?;
            let (seconds, mut nanos) = match args[0].0 {
                Kind::Int(n) => (n, 0),
                Kind::Float(n) if n.is_finite() => {
                    let whole = raw_int(n);
                    (whole, raw_int((n - whole as f64) * NANOS as f64))
                }
                _ => return Err(invalid()),
            };
            if args.len() >= 2 {
                let factor = if let Some(unit) = args.get(2) {
                    let Kind::Symbol(unit) = &unit.0 else {
                        return Err(invalid());
                    };
                    match unit.data.as_slice() {
                        b"microsecond" | b"usec" => 1000,
                        b"millisecond" => 1_000_000,
                        b"nanosecond" | b"nsec" => 1,
                        _ => return Err(invalid()),
                    }
                } else {
                    1000
                };
                nanos = nanos
                    .checked_add(scaled(&args[1], factor)?)
                    .ok_or_else(overflow)?;
            }
            let seconds = seconds
                .checked_add(nanos.div_euclid(NANOS))
                .ok_or_else(overflow)?;
            return value(
                ctx,
                Stamp::new(seconds, nanos.rem_euclid(NANOS) as u32),
                zone,
            );
        }
        let mut selected = if self == Self::New {
            // The keyword is validated even when a positional zone replaces it.
            location(ctx, zone_input, false)?
        } else {
            None
        };
        if args.is_empty() {
            return Err(invalid());
        }
        for arg in args {
            ctx.charge(1)?;
            if matches!(arg.0, Kind::Big(_)) {
                return Err(invalid());
            }
        }
        if self != Self::New && args.len() > 7 {
            return Err(invalid());
        }
        let year = match args[0].0 {
            Kind::Int(n) => n,
            Kind::Float(n) if n.is_finite() && n > i64::MIN as f64 && n < i64::MAX as f64 => {
                n as i64
            }
            _ => return Err(invalid()),
        };
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
        if self == Self::New {
            let positional = args.get(6).filter(|v| !absent_zone(v));
            if let Some(positional) = positional {
                selected = zone::Zone::parse(ctx, positional)?;
            } else if zone_input.is_none_or(absent_zone) {
                selected = zone::Zone::local(ctx).map(Some)?;
            }
        } else {
            if let Some(micros) = args.get(6) {
                if !matches!(micros.0, Kind::Nil) {
                    if micros.as_float().is_some_and(|n| n < 0.0) {
                        return Err(invalid());
                    }
                    nanos = scaled(micros, 1000)?;
                    if !(0..NANOS).contains(&nanos) {
                        return Err(invalid());
                    }
                }
            }
            if matches!(self, Self::Local | Self::Mktime) {
                selected = Some(zone::Zone::local(ctx)?);
            }
        }
        let seconds = if let Some(zone) = &selected {
            zone.calendar(ctx, parts)?
        } else {
            calendar::normalized(parts)
        };
        value(ctx, Stamp::new(seconds, nanos as u32), selected)
    }
}

pub(crate) fn now(ctx: &mut CallContext, args: &[Value]) -> Result<Value> {
    ops::arity(args, 0)?;
    ctx.checkpoint()?;
    text(ctx, &Value(Kind::Time(Stamp::now())), Some(0))
}

fn year(out: &mut json::Number, year: i64) {
    if year < 0 {
        out.write_char('-').unwrap();
    }
    write!(out, "{:04}", year.unsigned_abs()).unwrap();
}

fn numeric_zone(out: &mut json::Number, seconds: i32, colon: bool) {
    let minutes = seconds / 60;
    let sign = if minutes < 0 { '-' } else { '+' };
    let magnitude = minutes.unsigned_abs();
    write!(
        out,
        "{sign}{:02}{}{:02}",
        magnitude / 60,
        if colon { ":" } else { "" },
        magnitude % 60
    )
    .unwrap();
}

pub(crate) fn text(
    ctx: &mut CallContext,
    value: &Value,
    precision: Option<usize>,
) -> Result<Value> {
    let stamp = stamp(value).unwrap();
    let offset = offset(ctx, value)?;
    let date = calendar::civil(stamp.seconds(), offset.seconds);
    let mut out = json::Number::new();
    year(&mut out, date.year);
    write!(
        out,
        "-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.month, date.day, date.hour, date.minute, date.second
    )
    .unwrap();
    let mut digits = precision.unwrap_or(9);
    let mut fraction = stamp.nanos();
    if precision.is_none() {
        while digits > 0 && fraction % 10 == 0 {
            digits -= 1;
            fraction /= 10;
        }
    } else if digits < 9 {
        fraction /= 10u32.pow((9 - digits) as u32);
    }
    if digits > 0 {
        write!(out, ".{:0width$}", fraction, width = digits.min(9)).unwrap();
        for _ in 9..digits {
            out.write_char('0').unwrap();
        }
    }
    if offset.seconds == 0 {
        out.write_char('Z').unwrap();
    } else {
        numeric_zone(&mut out, offset.seconds, true);
    }
    ctx.bytes(out.bytes())
}

fn mail_date(ctx: &mut CallContext, receiver: &Value, http: bool) -> Result<Value> {
    let stamp = stamp(receiver).unwrap();
    let offset = if http {
        zone::Offset::utc()
    } else {
        offset(ctx, receiver)?
    };
    let date = calendar::civil(stamp.seconds(), offset.seconds);
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][date.weekday as usize];
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][date.month as usize - 1];
    let mut out = json::Number::new();
    write!(out, "{weekday}, {:02} {month} ", date.day).unwrap();
    year(&mut out, date.year);
    write!(
        out,
        " {:02}:{:02}:{:02} ",
        date.hour, date.minute, date.second
    )
    .unwrap();
    if http {
        out.write_str("GMT").unwrap();
    } else if matches!(receiver.0, Kind::Time(_))
        || (offset.seconds == 0 && offset.name.starts_with(b"-"))
    {
        out.write_str("-0000").unwrap();
    } else {
        numeric_zone(&mut out, offset.seconds, false);
    }
    ctx.bytes(out.bytes())
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
            .ok_or_else(overflow)?;
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
        return Err(Error::new(ErrorKind::Type, "unsupported time operands"));
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
        Kind::Float(n) => scaled_float(if negate { -n } else { n }, NANOS as u32)?,
        Kind::Big(_) => return Err(overflow()),
        _ => return Err(Error::new(ErrorKind::Type, "unsupported time operands")),
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
    if keywords {
        return Err(invalid());
    }
    let result = match name {
        "nil?" | "itself" | "dup" | "equal?" | "eql?" | "<=>" => {
            if block && !matches!(name, "eql?" | "<=>") {
                return Err(invalid());
            }
            let comparison = matches!(name, "equal?" | "eql?" | "<=>");
            ops::arity(args, usize::from(comparison))?;
            if name == "<=>" {
                stamp(&args[0]).map_or(Value::nil(), |other| Value::int(time.order(other) as i64))
            } else if comparison {
                Value::boolean(stamp(&args[0]) == Some(time))
            } else if name == "nil?" {
                Value::boolean(false)
            } else {
                receiver.clone()
            }
        }
        "between?" => {
            if block {
                return Err(invalid());
            }
            ops::arity(args, 2)?;
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
        "to_s" | "string" | "inspect" => {
            if block {
                return Err(invalid());
            }
            ops::arity(args, 0)?;
            text(ctx, receiver, None)?
        }
        "iso8601" | "xmlschema" | "rfc3339" => {
            if args.len() > 1 {
                return Err(invalid());
            }
            let precision = if let Some(arg) = args.first() {
                arg.require_int()?
            } else {
                0
            };
            if !(0..=100).contains(&precision) {
                return Err(invalid());
            }
            text(ctx, receiver, Some(precision as usize))?
        }
        "httpdate" | "rfc2822" | "rfc822" => {
            ops::arity(args, 0)?;
            mail_date(ctx, receiver, name == "httpdate")?
        }
        "getlocal" | "localtime" => {
            if args.len() > 1 {
                return Err(invalid());
            }
            let zone = location(ctx, args.first(), true)?;
            value(ctx, time, zone)?
        }
        "round" | "ceil" | "floor" => {
            if args.len() > usize::from(name == "round") {
                return Err(invalid());
            }
            let precision = if let Some(arg) = args.first() {
                arg.require_int()?
            } else {
                0
            };
            if precision < 0 {
                return Err(invalid());
            }
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
                "getutc" | "getgm" | "utc" | "gmtime" => Value(Kind::Time(time)),
                "nsec" | "tv_nsec" => Value::int(i64::from(time.nanos())),
                "usec" | "tv_usec" => Value::int(i64::from(time.nanos() / 1000)),
                "subsec" => Value::float(f64::from(time.nanos()) / NANOS as f64),
                "hash" => Value::int(
                    time.seconds()
                        .wrapping_mul(NANOS)
                        .wrapping_add(i64::from(time.nanos())),
                ),
                "to_i" | "tv_sec" => Value::int(time.seconds()),
                "to_f" | "to_r" => {
                    Value::float(time.seconds() as f64 + f64::from(time.nanos()) / NANOS as f64)
                }
                _ => {
                    let offset = offset(ctx, receiver)?;
                    let date = calendar::civil(time.seconds(), offset.seconds);
                    match name {
                        "year" => Value::int(date.year),
                        "month" | "mon" => Value::int(date.month),
                        "day" | "mday" => Value::int(date.day),
                        "hour" => Value::int(date.hour),
                        "min" => Value::int(date.minute),
                        "sec" => Value::int(date.second),
                        "wday" => Value::int(date.weekday),
                        "yday" => Value::int(date.yearday),
                        "utc_offset" | "gmt_offset" | "gmtoff" => {
                            Value::int(i64::from(offset.seconds))
                        }
                        "zone" => ctx.bytes(offset.name)?,
                        "utc?" | "gmt?" => Value::boolean(offset.seconds / 60 == 0),
                        "dst?" | "isdst" => Value::boolean(offset.dst),
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
