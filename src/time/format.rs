use crate::checked::convert;
// Go layout tokens and rendering adapt the Go standard library's time package.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::{Stamp, calendar, offset, stamp, zone};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::Buffer,
    bytes,
    checked::{Access, Checked, violation},
    numeric::to_usize,
    value::Kind,
};

pub(super) const OUTPUT_LIMIT: usize = 1 << 20;
pub(super) const MONTHS: [&[u8]; 12] = [
    b"January",
    b"February",
    b"March",
    b"April",
    b"May",
    b"June",
    b"July",
    b"August",
    b"September",
    b"October",
    b"November",
    b"December",
];
pub(super) const WEEKDAYS: [&[u8]; 7] = [
    b"Sunday",
    b"Monday",
    b"Tuesday",
    b"Wednesday",
    b"Thursday",
    b"Friday",
    b"Saturday",
];

enum Destination<'a> {
    Buffer(Buffer<u8>),
    Compare(&'a [u8], bool),
    Fixed(&'a mut [u8]),
}

pub(super) struct Output<'a> {
    destination: Destination<'a>,
    length: usize,
    limit: usize,
}

impl<'a> Output<'a> {
    pub fn buffer(limit: usize) -> Self {
        Self {
            destination: Destination::Buffer(Buffer::empty()),
            length: 0,
            limit,
        }
    }
    pub fn compare(input: &'a [u8], limit: usize) -> Self {
        Self {
            destination: Destination::Compare(input, true),
            length: 0,
            limit,
        }
    }
    pub fn fixed(buffer: &'a mut [u8]) -> Self {
        let limit = buffer.len();
        Self {
            destination: Destination::Fixed(buffer),
            length: 0,
            limit,
        }
    }
    pub fn len(&self) -> usize {
        self.length
    }
    pub fn matches(&self) -> bool {
        matches!(&self.destination, Destination::Compare(input, true) if input.len() == self.length)
    }
    pub fn reserve(&mut self, ctx: &mut CallContext, additional: usize) -> Result<()> {
        let Some(end) = self.length.checked_add(additional) else {
            return ctx.fail(ErrorKind::Memory, "formatted output size overflow");
        };
        if end > self.limit {
            return ctx.guard(
                ErrorKind::OutputLimit,
                if matches!(self.destination, Destination::Compare(..)) {
                    "output limit exceeded: time.strftime layout diagnostic exceeds limit"
                } else {
                    "output limit exceeded: time.strftime output exceeds limit 1048576 bytes"
                },
            );
        }
        if let Destination::Buffer(buffer) = &mut self.destination {
            if end > buffer.data.capacity() {
                buffer.ensure(ctx, end.max(buffer.data.capacity().saturating_mul(2)))?;
            }
        }
        Ok(())
    }
    pub fn append(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        self.reserve(ctx, bytes.len())?;
        for chunk in bytes.chunks(4096) {
            ctx.work_bytes(chunk.len())?;
            let end = self.length + chunk.len();
            match &mut self.destination {
                Destination::Buffer(buffer) => buffer.data.extend_from_slice(chunk),
                Destination::Compare(input, equal) => {
                    *equal &= input.get(self.length..end) == Some(chunk);
                }
                Destination::Fixed(buffer) => {
                    buffer.span_mut(self.length, end)?.copy_from_slice(chunk)
                }
            }
            self.length = end;
        }
        Ok(())
    }
    pub fn repeat(&mut self, ctx: &mut CallContext, byte: u8, count: usize) -> Result<()> {
        self.reserve(ctx, count)?;
        let chunk = [byte; 4096];
        let mut remaining = count;
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            self.append(ctx, chunk.span(0, n)?)?;
            remaining -= n;
        }
        Ok(())
    }
    pub fn finish(self, ctx: &mut CallContext) -> Result<Value> {
        let Destination::Buffer(buffer) = self.destination else {
            return Err(crate::checked::internal("time output buffer expected"));
        };
        Value::from_bytes(ctx, buffer)
    }
}

pub(super) struct View<'a> {
    pub stamp: Stamp,
    pub date: calendar::Civil,
    pub zone: zone::Offset<'a>,
}

impl<'a> View<'a> {
    pub fn new(ctx: &mut CallContext, value: &'a Value) -> Result<Self> {
        let stamp = stamp(value).ok_or(violation("time formatter requires a time value"))?;
        let zone = offset(ctx, value)?;
        Ok(Self {
            stamp,
            date: calendar::civil(stamp.seconds(), zone.seconds),
            zone,
        })
    }
    fn part(&self, part: Part) -> i64 {
        match part {
            Part::Year => self.date.year,
            Part::YearShort => (self.date.year % 100).abs(),
            Part::Month => self.date.month,
            Part::Day => self.date.day,
            Part::YearDay => self.date.yearday,
            Part::Hour => self.date.hour,
            Part::Hour12 => hour12(self.date.hour),
            Part::Minute => self.date.minute,
            Part::Second => self.date.second,
            Part::Weekday => self.date.weekday,
        }
    }
}

pub(super) fn hour12(hour: i64) -> i64 {
    let hour = hour % 12;
    if hour == 0 { 12 } else { hour }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Part {
    Year,
    YearShort,
    Month,
    Day,
    YearDay,
    Hour,
    Hour12,
    Minute,
    Second,
    Weekday,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct NumericZone {
    pub iso: bool,
    pub colon: bool,
    pub seconds: bool,
    pub short: bool,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Token {
    Number(Part, usize, u8),
    Name(Part, bool),
    Meridian(bool),
    Zone(NumericZone),
    ZoneName,
    Fraction(u8, usize, bool),
}

fn at(ctx: &mut CallContext, input: &[u8]) -> Result<Option<(Token, usize)>> {
    use Part::*;
    use Token::*;
    let number = |part, width, pad, length| Some((Number(part, width, pad), length));
    let Some((&first, rest)) = input.split_first() else {
        return Ok(None);
    };
    let token = match first {
        b'J' if input.starts_with(b"January") => Some((Name(Month, true), 7)),
        b'J' if input.starts_with(b"Jan") && !input.get(3).is_some_and(u8::is_ascii_lowercase) => {
            Some((Name(Month, false), 3))
        }
        b'M' if input.starts_with(b"Monday") => Some((Name(Weekday, true), 6)),
        b'M' if input.starts_with(b"Mon") && !input.get(3).is_some_and(u8::is_ascii_lowercase) => {
            Some((Name(Weekday, false), 3))
        }
        b'M' if input.starts_with(b"MST") => Some((ZoneName, 3)),
        b'0' if matches!(rest.first(), Some(b'1'..=b'6')) => {
            let part = match rest.first() {
                Some(b'1') => Month,
                Some(b'2') => Day,
                Some(b'3') => Hour12,
                Some(b'4') => Minute,
                Some(b'5') => Second,
                _ => YearShort,
            };
            number(part, 2, b'0', 2)
        }
        b'0' if input.starts_with(b"002") => number(YearDay, 3, b'0', 3),
        b'1' if input.starts_with(b"15") => number(Hour, 2, b'0', 2),
        b'1' => number(Month, 0, b'0', 1),
        b'2' if input.starts_with(b"2006") => number(Year, 4, b'0', 4),
        b'2' => number(Day, 0, b'0', 1),
        b'_' if input.starts_with(b"_2006") => None,
        b'_' if input.starts_with(b"_2") => number(Day, 2, b' ', 2),
        b'_' if input.starts_with(b"__2") => number(YearDay, 3, b' ', 3),
        b'3' => number(Hour12, 0, b'0', 1),
        b'4' => number(Minute, 0, b'0', 1),
        b'5' => number(Second, 0, b'0', 1),
        b'P' if input.starts_with(b"PM") => Some((Meridian(true), 2)),
        b'p' if input.starts_with(b"pm") => Some((Meridian(false), 2)),
        b'-' | b'Z' => {
            let mut found = None;
            for (suffix, colon, seconds, short) in [
                (b"070000".as_slice(), false, true, false),
                (b"07:00:00", true, true, false),
                (b"0700", false, false, false),
                (b"07:00", true, false, false),
                (b"07", false, false, true),
            ] {
                if rest.starts_with(suffix) {
                    found = Some((
                        Zone(NumericZone {
                            iso: first == b'Z',
                            colon,
                            seconds,
                            short,
                        }),
                        suffix.len() + 1,
                    ));
                    break;
                }
            }
            found
        }
        b'.' | b',' if matches!(rest.first(), Some(b'0' | b'9')) => {
            let mut end = 1;
            while input.get(end) == rest.first() {
                if end % 1024 == 0 {
                    ctx.charge(1)?;
                    ctx.checkpoint()?;
                }
                end += 1;
            }
            if input.get(end).is_some_and(u8::is_ascii_digit) {
                None
            } else {
                Some((
                    Fraction(first, (end - 1) & 0xfff, rest.first() == Some(&b'9')),
                    end,
                ))
            }
        }
        _ => None,
    };
    Ok(token)
}

pub(super) fn next(ctx: &mut CallContext, input: &[u8]) -> Result<(usize, Option<Token>, usize)> {
    let mut suffix = input;
    let mut i = 0;
    while let Some((_, rest)) = suffix.split_first() {
        if i % 1024 == 0 {
            ctx.charge(1)?;
            ctx.checkpoint()?;
        }
        if let Some((token, length)) = at(ctx, suffix)? {
            return Ok((i, Some(token), i + length));
        }
        suffix = rest;
        i += 1;
    }
    Ok((input.len(), None, input.len()))
}

fn integer(out: &mut bytes::Number, value: i64, width: usize, pad: u8) -> Access<()> {
    if pad == b' ' {
        out.format(format_args!("{value:width$}"))?;
    } else {
        if value < 0 {
            out.append("-")?;
        }
        out.format(format_args!("{:0width$}", value.unsigned_abs()))?;
    }
    Ok(())
}

fn numeric_zone(out: &mut bytes::Number, offset: i32, token: NumericZone) -> Access<()> {
    if token.iso && offset == 0 {
        out.append("Z")?;
        return Ok(());
    }
    let minutes = i64::from(offset) / 60;
    let negative = minutes < 0;
    out.append(if negative { "-" } else { "+" })?;
    integer(out, minutes.abs() / 60, 2, b'0')?;
    if token.colon {
        out.append(":")?;
    }
    if !token.short {
        integer(out, minutes.abs() % 60, 2, b'0')?;
    }
    if token.seconds {
        if token.colon {
            out.append(":")?;
        }
        integer(
            out,
            if negative {
                -i64::from(offset) % 60
            } else {
                i64::from(offset) % 60
            },
            2,
            b'0',
        )?;
    }
    Ok(())
}

pub(super) fn render(
    ctx: &mut CallContext,
    view: &View<'_>,
    mut layout: &[u8],
    out: &mut Output<'_>,
) -> Result<()> {
    while !layout.is_empty() {
        let (prefix, token, end) = next(ctx, layout)?;
        out.append(ctx, layout.span(0, prefix)?)?;
        layout = layout.tail(end)?;
        let Some(token) = token else {
            break;
        };
        let mut number = bytes::Number::new();
        match token {
            Token::Number(part, width, pad) => integer(&mut number, view.part(part), width, pad)?,
            Token::Name(part, full) => {
                let name = match part {
                    Part::Month => *MONTHS.at(to_usize(view.date.month) - 1)?,
                    Part::Weekday => *WEEKDAYS.at(to_usize(view.date.weekday))?,
                    _ => {
                        return Err(crate::checked::internal("month or weekday name expected"));
                    }
                };
                out.append(ctx, if full { name } else { name.span(0, 3)? })?;
            }
            Token::Meridian(upper) => {
                let text: &[u8] = match (view.date.hour < 12, upper) {
                    (true, true) => b"AM",
                    (false, true) => b"PM",
                    (true, false) => b"am",
                    (false, false) => b"pm",
                };
                out.append(ctx, text)?;
            }
            Token::Zone(token) => numeric_zone(&mut number, view.zone.seconds, token)?,
            Token::ZoneName if !view.zone.name.is_empty() => out.append(ctx, view.zone.name)?,
            Token::ZoneName => numeric_zone(
                &mut number,
                view.zone.seconds,
                NumericZone {
                    iso: false,
                    colon: false,
                    seconds: false,
                    short: false,
                },
            )?,
            Token::Fraction(separator, digits, trim) => {
                if trim && (digits == 0 || view.stamp.nanos() == 0) {
                    continue;
                }
                number.format(format_args!("{:09}", view.stamp.nanos()))?;
                let mut fraction = number.bytes()?.span(0, digits.min(9))?;
                if trim {
                    while let Some(rest) = fraction.strip_suffix(b"0") {
                        fraction = rest;
                    }
                }
                if !trim || !fraction.is_empty() {
                    out.append(ctx, &[separator])?;
                    out.append(ctx, fraction)?;
                }
                continue;
            }
        }
        out.append(ctx, number.bytes()?)?;
    }
    Ok(())
}

pub(super) fn format(ctx: &mut CallContext, value: &Value, layout: &[u8]) -> Result<Value> {
    if super::strftime::recognized(ctx, layout)? {
        // The directive scan has already charged for reading the whole layout.
        let mut message = b"time.format expects a Go layout such as \"2006-01-02\"; ".to_vec();
        crate::shapes::quote(layout, &mut message);
        message.extend_from_slice(b" is a strftime format, use strftime for that");
        return Err(Error::new(
            ErrorKind::Argument,
            String::from_utf8_lossy(&message).into_owned(),
        ));
    }
    let view = View::new(ctx, value)?;
    let mut out = Output::buffer(usize::MAX);
    render(ctx, &view, layout, &mut out)?;
    out.finish(ctx)
}

fn year(out: &mut bytes::Number, year: i64) -> Access<()> {
    if year < 0 {
        out.append("-")?;
    }
    out.format(format_args!("{:04}", year.unsigned_abs()))
}

fn numeric_offset(out: &mut bytes::Number, seconds: i32, colon: bool) -> Access<()> {
    let minutes = seconds / 60;
    let sign = if minutes < 0 { '-' } else { '+' };
    let magnitude = minutes.unsigned_abs();
    out.format(format_args!(
        "{sign}{:02}{}{:02}",
        magnitude / 60,
        if colon { ":" } else { "" },
        magnitude % 60
    ))
}

pub(crate) fn text(
    ctx: &mut CallContext,
    value: &Value,
    precision: Option<usize>,
) -> Result<Value> {
    let stamp = stamp(value).ok_or(violation("time formatter requires a time value"))?;
    let offset = offset(ctx, value)?;
    let date = calendar::civil(stamp.seconds(), offset.seconds);
    let mut out = bytes::Number::new();
    year(&mut out, date.year)?;
    out.format(format_args!(
        "-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.month, date.day, date.hour, date.minute, date.second
    ))?;
    let mut digits = precision.unwrap_or(9);
    let mut fraction = stamp.nanos();
    if precision.is_none() {
        while digits > 0 && fraction % 10 == 0 {
            digits -= 1;
            fraction /= 10;
        }
    } else if digits < 9 {
        fraction /= 10u32.pow(convert::<u32, _>(9 - digits)?);
    }
    if digits > 0 {
        out.format(format_args!(".{:0width$}", fraction, width = digits.min(9)))?;
        for _ in 9..digits {
            out.append("0")?;
        }
    }
    if offset.seconds == 0 {
        out.append("Z")?;
    } else {
        numeric_offset(&mut out, offset.seconds, true)?;
    }
    ctx.bytes(out.bytes()?)
}

pub(super) fn mail_date(ctx: &mut CallContext, receiver: &Value, http: bool) -> Result<Value> {
    let stamp = stamp(receiver).ok_or(violation("mail date formatter requires a time value"))?;
    let offset = if http {
        zone::Offset::utc()
    } else {
        offset(ctx, receiver)?
    };
    let date = calendar::civil(stamp.seconds(), offset.seconds);
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"].at(to_usize(date.weekday))?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .at(to_usize(date.month) - 1)?;
    let mut out = bytes::Number::new();
    out.format(format_args!("{weekday}, {:02} {month} ", date.day))?;
    year(&mut out, date.year)?;
    out.format(format_args!(
        " {:02}:{:02}:{:02} ",
        date.hour, date.minute, date.second
    ))?;
    if http {
        out.append("GMT")?;
    } else if matches!(receiver.0, Kind::Time(_))
        || (offset.seconds == 0 && offset.name.starts_with(b"-"))
    {
        out.append("-0000")?;
    } else {
        numeric_offset(&mut out, offset.seconds, false)?;
    }
    ctx.bytes(out.bytes()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn invalid_time_values_return_internal_errors() {
        let mut ctx = CallContext::new(CallOptions::default());
        let value = Value::nil();
        assert_eq!(
            View::new(&mut ctx, &value).err().unwrap().kind,
            ErrorKind::Internal
        );
        assert_eq!(
            text(&mut ctx, &value, None).unwrap_err().kind,
            ErrorKind::Internal
        );
        for http in [false, true] {
            assert_eq!(
                mail_date(&mut ctx, &value, http).unwrap_err().kind,
                ErrorKind::Internal
            );
        }
    }

    #[test]
    fn fixed_numeric_buffer_exhaustion_returns_internal_errors() {
        let mut out = bytes::Number::new();
        out.append(&"x".repeat(512)).unwrap();
        assert_eq!(
            Error::from(out.append("Z").unwrap_err()).kind,
            ErrorKind::Internal
        );
        let mut out = bytes::Number::new();
        let error = Error::from(integer(&mut out, i64::MIN, 513, b' ').unwrap_err());
        assert_eq!(error.kind, ErrorKind::Internal);
        let mut ctx = CallContext::new(CallOptions::default());
        let value = Value(Kind::Time(Stamp::new(0, 0)));
        let error = text(&mut ctx, &value, Some(513)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
    }
}
