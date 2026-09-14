// Go layout tokens and rendering adapt the Go standard library's time package.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::{Stamp, calendar, offset, stamp, zone};
use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, json};
use std::fmt::Write;

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
                "time formatting output limit exceeded",
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
                Destination::Fixed(buffer) => buffer[self.length..end].copy_from_slice(chunk),
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
            self.append(ctx, &chunk[..n])?;
            remaining -= n;
        }
        Ok(())
    }
    pub fn finish(self, ctx: &mut CallContext) -> Result<Value> {
        let Destination::Buffer(buffer) = self.destination else {
            unreachable!()
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
        let stamp = stamp(value).unwrap();
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
            Part::YearShort => (self.date.year.unsigned_abs() % 100) as i64,
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
    let token = match input[0] {
        b'J' if input.starts_with(b"January") => Some((Name(Month, true), 7)),
        b'J' if input.starts_with(b"Jan") && !input.get(3).is_some_and(u8::is_ascii_lowercase) => {
            Some((Name(Month, false), 3))
        }
        b'M' if input.starts_with(b"Monday") => Some((Name(Weekday, true), 6)),
        b'M' if input.starts_with(b"Mon") && !input.get(3).is_some_and(u8::is_ascii_lowercase) => {
            Some((Name(Weekday, false), 3))
        }
        b'M' if input.starts_with(b"MST") => Some((ZoneName, 3)),
        b'0' if input.get(1).is_some_and(|b| (b'1'..=b'6').contains(b)) => {
            let part = [Month, Day, Hour12, Minute, Second, YearShort][(input[1] - b'1') as usize];
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
                if input[1..].starts_with(suffix) {
                    found = Some((
                        Zone(NumericZone {
                            iso: input[0] == b'Z',
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
        b'.' | b',' if matches!(input.get(1), Some(b'0' | b'9')) => {
            let mut end = 1;
            while input.get(end) == Some(&input[1]) {
                if end % 1024 == 0 {
                    ctx.charge(1)?;
                    ctx.checkpoint()?;
                }
                end += 1;
            }
            if input.get(end).is_some_and(u8::is_ascii_digit) {
                None
            } else {
                Some((Fraction(input[0], (end - 1) & 0xfff, input[1] == b'9'), end))
            }
        }
        _ => None,
    };
    Ok(token)
}

pub(super) fn next(ctx: &mut CallContext, input: &[u8]) -> Result<(usize, Option<Token>, usize)> {
    for i in 0..input.len() {
        if i % 1024 == 0 {
            ctx.charge(1)?;
            ctx.checkpoint()?;
        }
        if let Some((token, length)) = at(ctx, &input[i..])? {
            return Ok((i, Some(token), i + length));
        }
    }
    Ok((input.len(), None, input.len()))
}

fn integer(out: &mut json::Number, value: i64, width: usize, pad: u8) {
    if pad == b' ' {
        write!(out, "{value:width$}").unwrap();
    } else {
        if value < 0 {
            out.write_char('-').unwrap();
        }
        write!(out, "{:0width$}", value.unsigned_abs()).unwrap();
    }
}

fn numeric_zone(out: &mut json::Number, offset: i32, token: NumericZone) {
    if token.iso && offset == 0 {
        out.write_char('Z').unwrap();
        return;
    }
    let minutes = i64::from(offset) / 60;
    let negative = minutes < 0;
    out.write_char(if negative { '-' } else { '+' }).unwrap();
    integer(out, minutes.abs() / 60, 2, b'0');
    if token.colon {
        out.write_char(':').unwrap();
    }
    if !token.short {
        integer(out, minutes.abs() % 60, 2, b'0');
    }
    if token.seconds {
        if token.colon {
            out.write_char(':').unwrap();
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
        );
    }
}

pub(super) fn render(
    ctx: &mut CallContext,
    view: &View<'_>,
    mut layout: &[u8],
    out: &mut Output<'_>,
) -> Result<()> {
    while !layout.is_empty() {
        let (prefix, token, end) = next(ctx, layout)?;
        out.append(ctx, &layout[..prefix])?;
        layout = &layout[end..];
        let Some(token) = token else {
            break;
        };
        let mut number = json::Number::new();
        match token {
            Token::Number(part, width, pad) => integer(&mut number, view.part(part), width, pad),
            Token::Name(part, full) => {
                let name = match part {
                    Part::Month => MONTHS[view.date.month as usize - 1],
                    Part::Weekday => WEEKDAYS[view.date.weekday as usize],
                    _ => unreachable!(),
                };
                out.append(ctx, if full { name } else { &name[..3] })?;
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
            Token::Zone(token) => numeric_zone(&mut number, view.zone.seconds, token),
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
            ),
            Token::Fraction(separator, digits, trim) => {
                if trim && (digits == 0 || view.stamp.nanos() == 0) {
                    continue;
                }
                write!(number, "{:09}", view.stamp.nanos()).unwrap();
                let mut end = digits.min(9);
                if trim {
                    while end > 0 && number.bytes()[end - 1] == b'0' {
                        end -= 1;
                    }
                }
                if !trim || end > 0 {
                    out.append(ctx, &[separator])?;
                    out.append(ctx, &number.bytes()[..end])?;
                }
                continue;
            }
        }
        out.append(ctx, number.bytes())?;
    }
    Ok(())
}

pub(super) fn format(ctx: &mut CallContext, value: &Value, layout: &[u8]) -> Result<Value> {
    if super::strftime::recognized(ctx, layout)? {
        return Err(Error::new(
            ErrorKind::Argument,
            "time.format expects a Go layout; use strftime for percent directives",
        ));
    }
    let view = View::new(ctx, value)?;
    let mut out = Output::buffer(usize::MAX);
    render(ctx, &view, layout, &mut out)?;
    out.finish(ctx)
}
