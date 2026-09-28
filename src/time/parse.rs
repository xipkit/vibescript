// Layout parsing adapts the Go standard library's time package.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::{
    Stamp, absent_zone, calendar,
    format::{self, NumericZone, Part, Token},
    location, value,
    zone::Zone,
};
use crate::{CallContext, Error, ErrorKind, Result, Value, value::Kind};
use std::sync::Arc;

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid time string or layout")
}

/// Refuses a `Time.parse` call without a time string and optional layout.
pub(super) fn shape() -> Error {
    Error::new(
        ErrorKind::Argument,
        "Time.parse expects a time string and optional layout",
    )
}

fn unparsed() -> Error {
    Error::new(ErrorKind::Argument, "Time.parse could not parse time")
}

/// Explains why `input` is not an RFC 3339 time with Go's `time.ParseError` text,
/// by rerunning Go's general layout parser outside the caller's budget.
pub(super) fn rfc3339_rejection(input: &[u8]) -> Option<String> {
    const LAYOUT: &[u8] = b"2006-01-02T15:04:05Z07:00";
    let mut rejection = None;
    let mut ctx = crate::integer::unlimited_context();
    layout(&mut ctx, LAYOUT, input, &mut rejection).err()?;
    rejection.map(|rejection| rejection.describe(LAYOUT, input))
}

fn number(input: &mut &[u8], minimum: usize, maximum: usize) -> Result<i64> {
    let mut result = 0;
    let mut length = 0;
    for &byte in input.iter().take(maximum) {
        if !byte.is_ascii_digit() {
            break;
        }
        result = result * 10 + i64::from(byte - b'0');
        length += 1;
    }
    if length < minimum {
        return Err(invalid());
    }
    *input = &input[length..];
    Ok(result)
}

fn separator(input: &mut &[u8], expected: u8) -> Result<()> {
    if input.first() != Some(&expected) {
        return Err(invalid());
    }
    *input = &input[1..];
    Ok(())
}

pub(super) fn rfc3339(ctx: &mut CallContext, mut input: &[u8]) -> Result<Stamp> {
    ctx.charge(1)?;
    let year = number(&mut input, 4, 4)?;
    separator(&mut input, b'-')?;
    let month = number(&mut input, 2, 2)?;
    separator(&mut input, b'-')?;
    let day = number(&mut input, 2, 2)?;
    separator(&mut input, b'T')?;
    // Go's RFC3339 layout fallback also accepts a one-digit hour and comma fractions.
    let hour = number(&mut input, 1, 2)?;
    separator(&mut input, b':')?;
    let minute = number(&mut input, 2, 2)?;
    separator(&mut input, b':')?;
    let second = number(&mut input, 2, 2)?;
    if !(1..=12).contains(&month)
        || !(1..=calendar::days_in(year, month)).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return Err(invalid());
    }

    let mut nanos = 0;
    if matches!(input.first(), Some(b'.' | b',')) {
        input = &input[1..];
        let mut length = 0;
        for chunk in input.chunks(1024) {
            ctx.charge(1)?;
            ctx.checkpoint()?;
            let n = chunk
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(chunk.len());
            length += n;
            if n < chunk.len() {
                break;
            }
        }
        if length == 0 {
            return Err(invalid());
        }
        for &byte in &input[..length.min(9)] {
            nanos = nanos * 10 + u32::from(byte - b'0');
        }
        nanos *= 10u32.pow(9 - length.min(9) as u32);
        input = &input[length..];
    }

    let offset = if input == b"Z" {
        0
    } else {
        if input.len() != 6 || !matches!(input[0], b'+' | b'-') {
            return Err(invalid());
        }
        let negative = input[0] == b'-';
        input = &input[1..];
        let hours = number(&mut input, 2, 2)?;
        separator(&mut input, b':')?;
        let minutes = number(&mut input, 2, 2)?;
        if hours > 24 || minutes > 60 {
            return Err(invalid());
        }
        let seconds = (hours * 60 + minutes) * 60;
        if negative { -seconds } else { seconds }
    };
    Ok(Stamp::new(
        calendar::normalized([year, month, day, hour, minute, second]) - offset,
        nanos,
    ))
}

const DEFAULT_LAYOUTS: [&[u8]; 11] = [
    b"2006-01-02T15:04:05.999999999Z07:00",
    b"2006-01-02T15:04:05Z07:00",
    b"Mon, 02 Jan 2006 15:04:05 -0700",
    b"Mon, 02 Jan 2006 15:04:05 MST",
    b"2006-01-02T15:04:05",
    b"2006-01-02 15:04:05",
    b"2006/01/02 15:04:05",
    b"2006-01-02",
    b"2006/01/02",
    b"01/02/2006 15:04:05",
    b"01/02/2006",
];

fn run(ctx: &mut CallContext, input: &[u8], predicate: impl Fn(u8) -> bool) -> Result<usize> {
    let mut length = 0;
    for chunk in input.chunks(1024) {
        ctx.charge(1)?;
        ctx.checkpoint()?;
        let n = chunk
            .iter()
            .position(|&b| !predicate(b))
            .unwrap_or(chunk.len());
        length += n;
        if n < chunk.len() {
            break;
        }
    }
    Ok(length)
}

fn skip(ctx: &mut CallContext, input: &mut &[u8], mut prefix: &[u8]) -> Result<()> {
    let mut consumed = 0;
    while let Some(&byte) = prefix.first() {
        if consumed % 1024 == 0 {
            ctx.charge(1)?;
            ctx.checkpoint()?;
        }
        if byte == b' ' {
            if input.first().is_some_and(|&b| b != b' ') {
                return Err(invalid());
            }
            prefix = &prefix[run(ctx, prefix, |b| b == b' ')?..];
            *input = &input[run(ctx, input, |b| b == b' ')?..];
        } else {
            separator(input, byte)?;
            prefix = &prefix[1..];
        }
        consumed += 1;
    }
    Ok(())
}

// Go's small signed fields accept an empty magnitude, including a lone sign.
fn signed(mut bytes: &[u8]) -> Result<i64> {
    let negative = bytes.first() == Some(&b'-');
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        bytes = &bytes[1..];
    }
    let mut result = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return Err(invalid());
        }
        result = result * 10 + i64::from(b - b'0');
    }
    Ok(if negative { -result } else { result })
}

fn fraction(input: &mut &[u8], length: usize, range: &mut Option<&'static str>) -> Result<u32> {
    if input.len() < length || !matches!(input.first(), Some(b'.' | b',')) {
        return Err(invalid());
    }
    let digits = (length - 1).min(9);
    let nanos = signed(&input[1..=digits])?;
    if nanos < 0 {
        *range = Some("fractional second");
        return Err(invalid());
    }
    *input = &input[length..];
    Ok(nanos as u32 * 10u32.pow(9 - digits as u32))
}

fn has_fraction(input: &[u8]) -> bool {
    matches!(input.first(), Some(b'.' | b',')) && input.get(1).is_some_and(u8::is_ascii_digit)
}

fn variable_fraction(ctx: &mut CallContext, input: &mut &[u8]) -> Result<u32> {
    let length = 1 + run(ctx, &input[1..], |b| b.is_ascii_digit())?;
    fraction(input, length, &mut None)
}

fn name(input: &mut &[u8], part: Part, full: bool) -> Result<i64> {
    let names = match part {
        Part::Month => format::MONTHS.as_slice(),
        Part::Weekday => format::WEEKDAYS.as_slice(),
        _ => unreachable!(),
    };
    for (i, name) in names.iter().enumerate() {
        let name = if full { name } else { &name[..3] };
        if input
            .get(..name.len())
            .is_some_and(|v| v.eq_ignore_ascii_case(name))
        {
            *input = &input[name.len()..];
            return Ok(i as i64 + 1);
        }
    }
    Err(invalid())
}

/// Reads a numeric zone offset in the order Go's parser checks it, naming
/// an out-of-range field in `range`.
fn numeric_zone(
    input: &mut &[u8],
    zone: NumericZone,
    range: &mut Option<&'static str>,
) -> Result<i32> {
    let value = *input;
    let (sign, fields, rest): (u8, [&[u8]; 3], &[u8]) = if zone.colon && !zone.seconds {
        if value.len() < 6 || value[3] != b':' {
            return Err(invalid());
        }
        (value[0], [&value[1..3], &value[4..6], b"00"], &value[6..])
    } else if zone.short {
        if value.len() < 3 {
            return Err(invalid());
        }
        (value[0], [&value[1..3], b"00", b"00"], &value[3..])
    } else if zone.colon {
        if value.len() < 9 || value[3] != b':' || value[6] != b':' {
            return Err(invalid());
        }
        (
            value[0],
            [&value[1..3], &value[4..6], &value[7..9]],
            &value[9..],
        )
    } else if zone.seconds {
        if value.len() < 7 {
            return Err(invalid());
        }
        (
            value[0],
            [&value[1..3], &value[3..5], &value[5..7]],
            &value[7..],
        )
    } else {
        if value.len() < 5 {
            return Err(invalid());
        }
        (value[0], [&value[1..3], &value[3..5], b"00"], &value[5..])
    };
    *input = rest;
    let mut parts = [0i64; 3];
    let mut bad = false;
    for (part, field) in parts.iter_mut().zip(fields) {
        if !field.iter().all(u8::is_ascii_digit) {
            bad = true;
            break;
        }
        *part = i64::from(field[0] - b'0') * 10 + i64::from(field[1] - b'0');
    }
    let [hours, minutes, seconds] = parts;
    for (value, name) in [
        (hours, "time zone offset hour"),
        (minutes, "time zone offset minute"),
        (seconds, "time zone offset second"),
    ] {
        if value > if name.ends_with("hour") { 24 } else { 60 } {
            *range = Some(name);
        }
    }
    let offset = ((hours * 60 + minutes) * 60 + seconds) as i32;
    let offset = match sign {
        b'+' => offset,
        b'-' => -offset,
        _ => {
            bad = true;
            offset
        }
    };
    if bad || range.is_some() {
        return Err(invalid());
    }
    Ok(offset)
}

fn signed_zone(ctx: &mut CallContext, input: &[u8]) -> Result<usize> {
    if !matches!(input.first(), Some(b'+' | b'-')) {
        return Ok(0);
    }
    let mut value = 0u64;
    let mut length = 1;
    for &byte in &input[1..] {
        if length % 1024 == 0 {
            ctx.charge(1)?;
            ctx.checkpoint()?;
        }
        if !byte.is_ascii_digit() {
            break;
        }
        let Some(next) = value
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(byte - b'0')))
        else {
            return Ok(0);
        };
        value = next;
        if value > 1 << 63 {
            return Ok(0);
        }
        length += 1;
    }
    Ok(if length == 1 || value > 23 { 0 } else { length })
}

fn zone_name(ctx: &mut CallContext, input: &[u8]) -> Result<usize> {
    if input.len() < 3 {
        return Err(invalid());
    }
    if input.starts_with(b"ChST") || input.starts_with(b"MeST") {
        return Ok(4);
    }
    if input.starts_with(b"GMT") {
        return Ok(3 + signed_zone(ctx, &input[3..])?);
    }
    if matches!(input.first(), Some(b'+' | b'-')) {
        let length = signed_zone(ctx, input)?;
        return if length == 0 {
            Err(invalid())
        } else {
            Ok(length)
        };
    }
    let length = input
        .iter()
        .take(6)
        .take_while(|b| b.is_ascii_uppercase())
        .count();
    if length == 3
        || (matches!(length, 4 | 5) && input[length - 1] == b'T')
        || (length == 4 && input.starts_with(b"WITA"))
    {
        Ok(length)
    } else {
        Err(invalid())
    }
}

struct Fields<'a> {
    parts: [i64; 6],
    nanos: u32,
    utc: bool,
    offset: i32,
    zone_name: &'a [u8],
}

/// Why a layout rejected its input, located the way Go's `time.ParseError` reports it.
#[derive(Clone, Copy)]
pub(super) struct Rejection {
    /// The layout element Go names, as a byte range of the layout.
    element: (usize, usize),
    /// The start of the rejected input suffix.
    value: usize,
    reason: Reason,
}

#[derive(Clone, Copy)]
enum Reason {
    CannotParse,
    OutOfRange(&'static str),
    ExtraText,
    Other(&'static str),
}

impl Rejection {
    /// Renders Go's `ParseError` text for this rejection.
    pub(super) fn describe(self, layout: &[u8], input: &[u8]) -> String {
        let mut out = String::from("parsing time ");
        quote(&mut out, input);
        let value = &input[self.value..];
        match self.reason {
            Reason::CannotParse => {
                out.push_str(" as ");
                quote(&mut out, layout);
                out.push_str(": cannot parse ");
                quote(&mut out, value);
                out.push_str(" as ");
                quote(&mut out, &layout[self.element.0..self.element.1]);
            }
            Reason::OutOfRange(field) => {
                out.push_str(": ");
                out.push_str(field);
                out.push_str(" out of range");
            }
            Reason::ExtraText => {
                out.push_str(": extra text: ");
                quote(&mut out, value);
            }
            Reason::Other(message) => out.push_str(message),
        }
        out
    }
}

/// Quotes bytes as Go's time package does in parse errors: printable ASCII
/// stays literal, and every other byte of a rune is written as `\xNN`.
fn quote(out: &mut String, bytes: &[u8]) {
    out.push('"');
    let mut at = 0;
    while at < bytes.len() {
        let (rune, width, valid) = crate::scan::rune(&bytes[at..]);
        if valid && (' '..'\u{80}').contains(&rune) {
            if matches!(rune, '"' | '\\') {
                out.push('\\');
            }
            out.push(rune);
        } else {
            let width = if valid { width } else { 1 };
            for byte in &bytes[at..at + width] {
                out.push_str(&format!("\\x{byte:02x}"));
            }
            at += width;
            continue;
        }
        at += width;
    }
    out.push('"');
}

fn layout<'a>(
    ctx: &mut CallContext,
    whole: &[u8],
    original: &'a [u8],
    rejection: &mut Option<Rejection>,
) -> Result<Fields<'a>> {
    let mut layout = whole;
    let mut input = original;
    let mut fields = Fields {
        parts: [0, -1, -1, 0, 0, 0],
        nanos: 0,
        utc: false,
        offset: -1,
        zone_name: b"",
    };
    let mut yearday = -1;
    let (mut am, mut pm) = (false, false);
    let offset = |rest: &[u8], whole: &[u8]| whole.len() - rest.len();
    let mut reject = |element: (usize, usize), value: &[u8], reason: Reason| {
        *rejection = Some(Rejection {
            element,
            value: offset(value, original),
            reason,
        });
        invalid()
    };
    loop {
        ctx.charge(1)?;
        let (prefix, token, end) = format::next(ctx, layout)?;
        let start = offset(layout, whole);
        if let Err(error) = skip(ctx, &mut input, &layout[..prefix]) {
            if error.kind != ErrorKind::Argument {
                return Err(error);
            }
            return Err(reject((start, start + prefix), input, Reason::CannotParse));
        }
        let Some(token) = token else {
            if !input.is_empty() {
                return Err(reject((start, start), input, Reason::ExtraText));
            }
            break;
        };
        layout = &layout[end..];
        let element = (start + prefix, start + end);
        let hold = input;
        let mut range = None;
        let parsed: Result<()> = 'token: {
            match token {
                Token::Number(part, width, pad) => {
                    let n = match part {
                        Part::YearShort => {
                            let Some(bytes) = input.get(..2) else {
                                break 'token Err(invalid());
                            };
                            let year = match signed(bytes) {
                                Ok(year) => year,
                                Err(error) => break 'token Err(error),
                            };
                            input = &input[2..];
                            year + if year >= 69 { 1900 } else { 2000 }
                        }
                        Part::Year => match number(&mut input, 4, 4) {
                            Ok(year) => year,
                            Err(error) => break 'token Err(error),
                        },
                        _ => {
                            if pad == b' ' {
                                for _ in 1..width {
                                    if input.first() == Some(&b' ') {
                                        input = &input[1..];
                                    }
                                }
                            }
                            let max = if matches!(part, Part::YearDay) { 3 } else { 2 };
                            let min = if pad == b'0' && width != 0 && !matches!(part, Part::Hour) {
                                width
                            } else {
                                1
                            };
                            match number(&mut input, min, max) {
                                Ok(n) => n,
                                Err(error) => break 'token Err(error),
                            }
                        }
                    };
                    let index = match part {
                        Part::Year | Part::YearShort => 0,
                        Part::Month if (1..=12).contains(&n) => 1,
                        Part::Day => 2,
                        Part::YearDay => {
                            yearday = n;
                            break 'token Ok(());
                        }
                        Part::Hour if n < 24 => 3,
                        Part::Hour12 if n <= 12 => 3,
                        Part::Minute if n < 60 => 4,
                        Part::Second if n < 60 => {
                            let next = match format::next(ctx, layout) {
                                Ok((_, next, _)) => next,
                                Err(error) => break 'token Err(error),
                            };
                            if has_fraction(input) && !matches!(next, Some(Token::Fraction(..))) {
                                fields.nanos = match variable_fraction(ctx, &mut input) {
                                    Ok(nanos) => nanos,
                                    Err(error) => break 'token Err(error),
                                };
                            }
                            5
                        }
                        _ => {
                            range = Some(match part {
                                Part::Month => "month",
                                Part::Minute => "minute",
                                Part::Second => "second",
                                _ => "hour",
                            });
                            break 'token Err(invalid());
                        }
                    };
                    fields.parts[index] = n;
                    Ok(())
                }
                Token::Name(part, full) => name(&mut input, part, full).map(|n| {
                    if matches!(part, Part::Month) {
                        fields.parts[1] = n;
                    }
                }),
                Token::Meridian(upper) => {
                    let Some(text) = input.get(..2) else {
                        break 'token Err(invalid());
                    };
                    if text == if upper { b"AM" } else { b"am" } {
                        am = true;
                    } else if text == if upper { b"PM" } else { b"pm" } {
                        pm = true;
                    } else {
                        break 'token Err(invalid());
                    }
                    input = &input[2..];
                    Ok(())
                }
                Token::Zone(zone) => {
                    if zone.iso && input.first() == Some(&b'Z') {
                        fields.utc = true;
                        input = &input[1..];
                        Ok(())
                    } else {
                        numeric_zone(&mut input, zone, &mut range)
                            .map(|offset| fields.offset = offset)
                    }
                }
                Token::ZoneName => {
                    if input.starts_with(b"UTC") {
                        fields.utc = true;
                        input = &input[3..];
                        Ok(())
                    } else {
                        zone_name(ctx, input).map(|length| {
                            fields.zone_name = &input[..length];
                            input = &input[length..];
                        })
                    }
                }
                Token::Fraction(_, digits, trim) => {
                    if !trim {
                        fraction(&mut input, digits + 1, &mut range).map(|nanos| {
                            fields.nanos = nanos;
                        })
                    } else if has_fraction(input) {
                        variable_fraction(ctx, &mut input).map(|nanos| fields.nanos = nanos)
                    } else {
                        Ok(())
                    }
                }
            }
        };
        if let Err(error) = parsed {
            if error.kind != ErrorKind::Argument {
                return Err(error);
            }
            return Err(match range {
                Some(field) => reject(element, input, Reason::OutOfRange(field)),
                None => reject(element, hold, Reason::CannotParse),
            });
        }
    }
    let end = (whole.len(), whole.len());
    let [year, month, day, hour, _, _] = &mut fields.parts;
    if pm && *hour < 12 {
        *hour += 12;
    } else if am && *hour == 12 {
        *hour = 0;
    }
    if yearday >= 0 {
        if !(1..=365 + i64::from(calendar::leap(*year))).contains(&yearday) {
            return Err(reject(
                end,
                input,
                Reason::Other(": day-of-year out of range"),
            ));
        }
        let mut m = 1;
        while yearday > calendar::days_in(*year, m) {
            yearday -= calendar::days_in(*year, m);
            m += 1;
        }
        if *month >= 0 && *month != m {
            return Err(reject(
                end,
                input,
                Reason::Other(": day-of-year does not match month"),
            ));
        }
        if *day >= 0 && *day != yearday {
            return Err(reject(
                end,
                input,
                Reason::Other(": day-of-year does not match day"),
            ));
        }
        *month = m;
        *day = yearday;
    } else {
        if *month < 0 {
            *month = 1;
        }
        if *day < 0 {
            *day = 1;
        }
    }
    if !(1..=calendar::days_in(*year, *month)).contains(day) {
        return Err(reject(end, input, Reason::Other(": day out of range")));
    }
    Ok(fields)
}

impl Fields<'_> {
    fn finish(
        self,
        ctx: &mut CallContext,
        local: Option<Arc<Zone>>,
        override_zone: Option<&Option<Arc<Zone>>>,
        input: &[u8],
    ) -> Result<Value> {
        let mut seconds = calendar::normalized(self.parts);
        let mut zone = None;
        if !self.utc {
            // The reference uses -1 as the sentinel, even for a parsed -00:00:01.
            if self.offset != -1 {
                let offset = self.offset;
                seconds -= i64::from(offset);
                if override_zone.is_none() {
                    let active = match &local {
                        Some(local) => local.lookup(ctx, seconds)?,
                        None => super::zone::Offset::utc(),
                    };
                    if active.seconds == offset
                        && (self.zone_name.is_empty()
                            || Zone::equal_name(ctx, active.name, self.zone_name)?)
                    {
                        zone = local;
                    } else {
                        zone = Some(Zone::fixed(ctx, self.zone_name, offset)?);
                    }
                }
            } else if !self.zone_name.is_empty() {
                let known = match &local {
                    Some(local) => local.lookup_name(ctx, self.zone_name, seconds)?,
                    None => None,
                };
                if let Some(found) = known {
                    seconds -= i64::from(found);
                    zone = local;
                } else {
                    let mut offset = 0;
                    if self.zone_name.starts_with(b"GMT") && self.zone_name.len() > 3 {
                        // Parsing already validated the magnitude; leading zeroes can be arbitrarily long.
                        for chunk in self.zone_name[4..].chunks(1024) {
                            ctx.charge(1)?;
                            ctx.checkpoint()?;
                            for &digit in chunk {
                                offset = offset * 10 + i32::from(digit - b'0');
                            }
                        }
                        offset *= if self.zone_name[3] == b'-' {
                            -3600
                        } else {
                            3600
                        };
                    }
                    if override_zone.is_none() {
                        zone = Some(Zone::fixed(ctx, self.zone_name, offset)?);
                    }
                }
            } else if let Some(local) = local {
                seconds = local.calendar(ctx, self.parts)?;
                zone = Some(local);
            }
        }
        if let Some(requested) = override_zone {
            zone = requested.clone();
        } else if input.ends_with(b"-00:00") || input.ends_with(b"-0000") {
            let offset = match &zone {
                Some(zone) => zone.lookup(ctx, seconds)?.seconds,
                None => 0,
            };
            if offset == 0 {
                zone = Some(Zone::fixed(ctx, b"-00:00", 0)?);
            }
        }
        value(ctx, Stamp::new(seconds, self.nanos), zone)
    }
}

pub(super) fn call(
    ctx: &mut CallContext,
    args: &[Value],
    zone_input: Option<&Value>,
) -> Result<Value> {
    let requested = location(ctx, zone_input, false)?;
    let overridden = zone_input.is_some_and(|v| !absent_zone(v));
    let Kind::Bytes(input) = &args[0].0 else {
        return Err(shape());
    };
    let input = input.data.as_slice();
    let custom = match args.get(1).map(|v| &v.0) {
        None | Some(Kind::Nil) => None,
        Some(Kind::Bytes(bytes)) => Some(bytes.data.as_slice()),
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                "Time.parse layout must be string",
            ));
        }
    };
    let (fields, host_zone) = if let Some(custom) = custom {
        let mut rejection = None;
        let fields = layout(ctx, custom, input, &mut rejection).map_err(|error| match rejection
            .filter(|_| error.kind == ErrorKind::Argument)
        {
            Some(rejection) => Error::new(
                ErrorKind::Argument,
                format!(
                    "Time.parse could not parse time: {}",
                    rejection.describe(custom, input)
                ),
            ),
            None => error,
        })?;
        let mut host = false;
        if !overridden && !fields.utc {
            for (i, bytes) in custom.windows(3).enumerate() {
                if i % 1024 == 0 {
                    ctx.charge(1)?;
                    ctx.checkpoint()?;
                }
                if bytes == b"MST" {
                    host = true;
                    break;
                }
            }
        }
        (fields, host)
    } else {
        let mut parsed = None;
        for (index, candidate) in DEFAULT_LAYOUTS.iter().enumerate() {
            match layout(ctx, candidate, input, &mut None) {
                Ok(fields) => {
                    parsed = Some((fields, index < 2 || (index == 3 && !overridden)));
                    break;
                }
                Err(error) if error.kind == ErrorKind::Argument => {}
                Err(error) => return Err(error),
            }
        }
        parsed.ok_or_else(unparsed)?
    };
    let local = if host_zone && !fields.utc && !(overridden && fields.offset != -1) {
        Some(Zone::local(ctx)?)
    } else {
        requested.clone()
    };
    fields.finish(ctx, local, overridden.then_some(&requested), input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn parsing_uses_no_heap_scratch_and_never_swallows_exhaustion() {
        let text = Value::bytes(format!("1970-01-01T00:00:00.{}Z", "1".repeat(131072)));
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(0),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let result = call(&mut ctx, std::slice::from_ref(&text), None).unwrap();
        assert_eq!(result.as_time(), Some((0, 111111111)));
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        for custom in [false, true] {
            let mut args = vec![text.clone()];
            if custom {
                args.push(Value::bytes(DEFAULT_LAYOUTS[0]));
            }
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            assert_eq!(
                call(&mut ctx, &args, None).unwrap_err().kind,
                ErrorKind::Steps
            );
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Steps);
            let mut ctx = CallContext::new(CallOptions::default());
            ctx.cancellation().cancel();
            assert_eq!(
                call(&mut ctx, &args, None).unwrap_err().kind,
                ErrorKind::Cancelled
            );
        }
        let args = [Value::bytes("1970 +0530"), Value::bytes("2006 -0700")];
        assert_eq!(
            call(&mut ctx, &args, None).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn unknown_abbreviations_copy_only_the_name_from_large_inputs() {
        let prefix = "x".repeat(131072);
        let text = format!("{prefix}1970 ABC");
        let format = format!("{prefix}2006 MST");
        let mut ctx = CallContext::new(CallOptions::default());
        let fields = layout(&mut ctx, format.as_bytes(), text.as_bytes(), &mut None).unwrap();
        let result = fields
            .finish(&mut ctx, None, None, text.as_bytes())
            .unwrap();
        assert_eq!(result.as_time(), Some((0, 0)));
        assert_eq!(
            super::super::offset(&mut ctx, &result).unwrap().name,
            b"ABC"
        );
        assert!(ctx.stats().retained_memory_bytes < 512);
        assert!(ctx.stats().peak_memory_bytes < 512);
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
