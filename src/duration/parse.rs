// Go-style duration parsing adapts time/format.go from the Go standard library.
// Copyright 2010 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.
use crate::{CallContext, Error, ErrorKind, Result};

/// A parse failure worded as Go's `ParseDurationString` reports it.
fn failure(message: &'static str) -> Error {
    Error::new(ErrorKind::Argument, message)
}

fn format() -> Error {
    failure("invalid duration format")
}

fn week() -> Error {
    failure("invalid week duration")
}

fn number() -> Error {
    failure("invalid duration number")
}

enum Digits {
    None,
    Value(u64),
    Overflow,
}

impl Digits {
    /// Returns the value of a run that fit, or the error for a missing or oversized one.
    fn value(self, missing: fn() -> Error, overflow: fn() -> Error) -> Result<u64> {
        match self {
            Self::Value(value) => Ok(value),
            Self::None => Err(missing()),
            Self::Overflow => Err(overflow()),
        }
    }
}

struct Scan<'a, 'c> {
    input: &'a [u8],
    ctx: &'c mut CallContext,
    at: usize,
    checkpoint: usize,
}

impl<'a, 'c> Scan<'a, 'c> {
    fn new(input: &'a [u8], ctx: &'c mut CallContext) -> Self {
        Self {
            input,
            ctx,
            at: 0,
            checkpoint: 0,
        }
    }
    fn peek(&self) -> Option<u8> {
        self.input.get(self.at).copied()
    }
    fn advance(&mut self) -> Result<u8> {
        if self.at >= self.checkpoint {
            self.ctx
                .work_bytes((self.input.len() - self.at).min(1024))?;
            self.checkpoint = self.at + 1024;
        }
        let byte = self.input[self.at];
        self.at += 1;
        Ok(byte)
    }
    /// Reads a decimal run, stopping at the first digit that exceeds `limit`.
    fn digits(&mut self, limit: u64) -> Result<Digits> {
        let start = self.at;
        let mut value = 0u64;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            let digit = u64::from(self.advance()? - b'0');
            match value
                .checked_mul(10)
                .and_then(|value| value.checked_add(digit))
                .filter(|&value| value <= limit)
            {
                Some(next) => value = next,
                None => return Ok(Digits::Overflow),
            }
        }
        Ok(if self.at == start {
            Digits::None
        } else {
            Digits::Value(value)
        })
    }
    /// Skips the rest of a digit run already charged to this scan, reporting
    /// whether it ended there.
    fn skip_charged_digits(&mut self) -> bool {
        while self.at < self.checkpoint.min(self.input.len()) {
            if !self.input[self.at].is_ascii_digit() {
                return true;
            }
            self.at += 1;
        }
        self.at == self.input.len() || !self.input[self.at].is_ascii_digit()
    }
    fn fraction(&mut self) -> Result<(u64, f64, bool)> {
        let start = self.at;
        let mut value = 0u64;
        let mut scale = 1.0;
        let mut overflow = false;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            let digit = u64::from(self.advance()? - b'0');
            if overflow {
                continue;
            }
            if value > i64::MAX as u64 / 10 || value * 10 + digit > 1 << 63 {
                overflow = true;
                continue;
            }
            value = value * 10 + digit;
            scale *= 10.0;
        }
        Ok((value, scale, self.at != start))
    }
}

pub(super) fn parse(ctx: &mut CallContext, input: &[u8]) -> Result<i64> {
    if input.is_empty() {
        return Err(failure("empty duration string"));
    }
    let negative = input.first() == Some(&b'-');
    let sign = usize::from(matches!(input.first(), Some(b'-' | b'+')));
    let input = &input[sign..];
    if input.first() == Some(&b'P') {
        return iso(ctx, &input[1..], negative);
    }
    if input == b"0" {
        return Ok(0);
    }
    if input.is_empty() {
        return Err(format());
    }
    let mut scan = Scan::new(input, ctx);
    let mut nanos = 0u64;
    while scan.peek().is_some() {
        let whole = match scan.digits(1 << 63)? {
            Digits::None => None,
            Digits::Value(value) => Some(value),
            Digits::Overflow => return Err(format()),
        };
        let (fraction, scale, post) = if scan.peek() == Some(b'.') {
            scan.advance()?;
            scan.fraction()?
        } else {
            (0, 1.0, false)
        };
        if whole.is_none() && !post {
            return Err(format());
        }
        let start = scan.at;
        while scan
            .peek()
            .is_some_and(|byte| byte != b'.' && !byte.is_ascii_digit())
        {
            scan.advance()?;
        }
        let unit = match &input[start..scan.at] {
            b"ns" => 1u64,
            b"us" | b"\xc2\xb5s" | b"\xce\xbcs" => 1000,
            b"ms" => 1_000_000,
            b"s" => 1_000_000_000,
            b"m" => 60_000_000_000,
            b"h" => 3_600_000_000_000,
            _ => return Err(format()),
        };
        let value = whole.unwrap_or(0);
        if value > (1u64 << 63) / unit {
            return Err(format());
        }
        let value = value * unit + (fraction as f64 * (unit as f64 / scale)) as u64;
        if value > 1 << 63 {
            return Err(format());
        }
        nanos = nanos.checked_add(value).ok_or_else(format)?;
        if nanos > 1 << 63 {
            return Err(format());
        }
    }
    if !negative && nanos > i64::MAX as u64 {
        return Err(format());
    }
    if nanos % 1_000_000_000 != 0 {
        return Err(failure("duration must be whole seconds"));
    }
    let seconds = (nanos / 1_000_000_000) as i64;
    Ok(if negative { -seconds } else { seconds })
}

fn iso(ctx: &mut CallContext, input: &[u8], negative: bool) -> Result<i64> {
    if input.is_empty() || input == b"T" {
        return Err(format());
    }
    let mut scan = Scan::new(input, ctx);
    let mut weeks = false;
    let mut time_at = None;
    let mut mixed = false;
    while let Some(byte) = scan.peek() {
        if byte == b'W' {
            weeks = true;
        } else if byte == b'T' {
            time_at.get_or_insert(scan.at);
        } else if matches!(byte, b'D' | b'H' | b'M' | b'S') {
            mixed = true;
        }
        scan.advance()?;
    }
    let seconds = if weeks {
        if time_at.is_some() || mixed {
            return Err(failure("invalid mixed week duration"));
        }
        if input.last() != Some(&b'W') || input.len() == 1 {
            return Err(failure("invalid week duration format"));
        }
        let text = &input[..input.len() - 1];
        let negative = text.first() == Some(&b'-');
        let sign = usize::from(matches!(text.first(), Some(b'-' | b'+')));
        let mut scan = Scan::new(&text[sign..], ctx);
        let magnitude = scan
            .digits(i64::MAX as u64 + u64::from(negative))?
            .value(week, week)?;
        if scan.peek().is_some() {
            return Err(week());
        }
        let weeks = if negative {
            -(magnitude as i128)
        } else {
            magnitude as i128
        } as i64;
        weeks.wrapping_mul(604800)
    } else {
        let (date, time) = time_at.map_or((input, b"".as_slice()), |at| {
            (&input[..at], &input[at + 1..])
        });
        segment(ctx, date, &[(b'D', 86400)])?.wrapping_add(segment(
            ctx,
            time,
            &[(b'H', 3600), (b'M', 60), (b'S', 1)],
        )?)
    };
    Ok(if negative {
        seconds.wrapping_neg()
    } else {
        seconds
    })
}

fn segment(ctx: &mut CallContext, input: &[u8], units: &[(u8, i64)]) -> Result<i64> {
    let mut scan = Scan::new(input, ctx);
    let mut first_unit = 0;
    let mut total = 0i64;
    while scan.peek().is_some() {
        let value = match scan.digits(i64::MAX as u64)? {
            Digits::Overflow => {
                // Go reads the whole run and names the number only before a
                // unit it still accepts; a run past the charged window is
                // reported as a number.
                let known = !scan.skip_charged_digits()
                    || scan.peek().is_some_and(|suffix| {
                        units[first_unit..].iter().any(|&(unit, _)| unit == suffix)
                    });
                return Err(if known { number() } else { format() });
            }
            digits => digits.value(format, number)? as i64,
        };
        let suffix = scan.peek().ok_or_else(format)?;
        let index = units[first_unit..]
            .iter()
            .position(|&(unit, _)| unit == suffix)
            .map(|index| first_unit + index)
            .ok_or_else(format)?;
        scan.advance()?;
        total = total.wrapping_add(value.wrapping_mul(units[index].1));
        first_unit = index + 1;
    }
    Ok(total)
}
