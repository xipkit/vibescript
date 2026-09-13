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

fn fraction(input: &mut &[u8], length: usize) -> Result<u32> {
    if input.len() < length || !matches!(input.first(), Some(b'.' | b',')) {
        return Err(invalid());
    }
    let digits = (length - 1).min(9);
    let nanos = signed(&input[1..=digits])?;
    if nanos < 0 {
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
    fraction(input, length)
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

fn numeric_zone(input: &mut &[u8], zone: NumericZone) -> Result<i32> {
    let negative = input.first() == Some(&b'-');
    if !matches!(input.first(), Some(b'+' | b'-')) {
        return Err(invalid());
    }
    *input = &input[1..];
    let hours = number(input, 2, 2)?;
    let mut minutes = 0;
    let mut seconds = 0;
    if !zone.short {
        if zone.colon {
            separator(input, b':')?;
        }
        minutes = number(input, 2, 2)?;
    }
    if zone.seconds {
        if zone.colon {
            separator(input, b':')?;
        }
        seconds = number(input, 2, 2)?;
    }
    if hours > 24 || minutes > 60 || seconds > 60 {
        return Err(invalid());
    }
    let seconds = ((hours * 60 + minutes) * 60 + seconds) as i32;
    Ok(if negative { -seconds } else { seconds })
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

fn layout<'a>(ctx: &mut CallContext, mut layout: &[u8], mut input: &'a [u8]) -> Result<Fields<'a>> {
    let mut fields = Fields {
        parts: [0, -1, -1, 0, 0, 0],
        nanos: 0,
        utc: false,
        offset: -1,
        zone_name: b"",
    };
    let mut yearday = -1;
    let (mut am, mut pm) = (false, false);
    loop {
        ctx.charge(1)?;
        let (prefix, token, end) = format::next(ctx, layout)?;
        skip(ctx, &mut input, &layout[..prefix])?;
        let Some(token) = token else {
            if !input.is_empty() {
                return Err(invalid());
            }
            break;
        };
        layout = &layout[end..];
        match token {
            Token::Number(part, width, pad) => {
                let n = match part {
                    Part::YearShort => {
                        let bytes = input.get(..2).ok_or_else(invalid)?;
                        let year = signed(bytes)?;
                        input = &input[2..];
                        year + if year >= 69 { 1900 } else { 2000 }
                    }
                    Part::Year => number(&mut input, 4, 4)?,
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
                        number(&mut input, min, max)?
                    }
                };
                let index = match part {
                    Part::Year | Part::YearShort => 0,
                    Part::Month if (1..=12).contains(&n) => 1,
                    Part::Day => 2,
                    Part::YearDay => {
                        yearday = n;
                        continue;
                    }
                    Part::Hour if n < 24 => 3,
                    Part::Hour12 if n <= 12 => 3,
                    Part::Minute if n < 60 => 4,
                    Part::Second if n < 60 => {
                        if has_fraction(input)
                            && !matches!(format::next(ctx, layout)?.1, Some(Token::Fraction(..)))
                        {
                            fields.nanos = variable_fraction(ctx, &mut input)?;
                        }
                        5
                    }
                    _ => return Err(invalid()),
                };
                fields.parts[index] = n;
            }
            Token::Name(part, full) => {
                let n = name(&mut input, part, full)?;
                if matches!(part, Part::Month) {
                    fields.parts[1] = n;
                }
            }
            Token::Meridian(upper) => {
                let text = input.get(..2).ok_or_else(invalid)?;
                if text == if upper { b"AM" } else { b"am" } {
                    am = true;
                } else if text == if upper { b"PM" } else { b"pm" } {
                    pm = true;
                } else {
                    return Err(invalid());
                }
                input = &input[2..];
            }
            Token::Zone(zone) => {
                if zone.iso && input.first() == Some(&b'Z') {
                    fields.utc = true;
                    input = &input[1..];
                } else {
                    fields.offset = numeric_zone(&mut input, zone)?;
                }
            }
            Token::ZoneName => {
                if input.starts_with(b"UTC") {
                    fields.utc = true;
                    input = &input[3..];
                } else {
                    let length = zone_name(ctx, input)?;
                    fields.zone_name = &input[..length];
                    input = &input[length..];
                }
            }
            Token::Fraction(_, digits, trim) => {
                if !trim {
                    fields.nanos = fraction(&mut input, digits + 1)?;
                } else if has_fraction(input) {
                    fields.nanos = variable_fraction(ctx, &mut input)?;
                }
            }
        }
    }
    let [year, month, day, hour, _, _] = &mut fields.parts;
    if pm && *hour < 12 {
        *hour += 12;
    } else if am && *hour == 12 {
        *hour = 0;
    }
    if yearday >= 0 {
        if !(1..=365 + i64::from(calendar::leap(*year))).contains(&yearday) {
            return Err(invalid());
        }
        let mut m = 1;
        while yearday > calendar::days_in(*year, m) {
            yearday -= calendar::days_in(*year, m);
            m += 1;
        }
        if (*month >= 0 && *month != m) || (*day >= 0 && *day != yearday) {
            return Err(invalid());
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
        return Err(invalid());
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
        return Err(invalid());
    };
    let input = input.data.as_slice();
    let custom = match args.get(1).map(|v| &v.0) {
        None | Some(Kind::Nil) => None,
        Some(Kind::Bytes(bytes)) => Some(bytes.data.as_slice()),
        _ => return Err(invalid()),
    };
    let (fields, host_zone) = if let Some(custom) = custom {
        let fields = layout(ctx, custom, input)?;
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
            match layout(ctx, candidate, input) {
                Ok(fields) => {
                    parsed = Some((fields, index < 2 || (index == 3 && !overridden)));
                    break;
                }
                Err(error) if error.kind == ErrorKind::Argument => {}
                Err(error) => return Err(error),
            }
        }
        parsed.ok_or_else(invalid)?
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
        let fields = layout(&mut ctx, format.as_bytes(), text.as_bytes()).unwrap();
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
