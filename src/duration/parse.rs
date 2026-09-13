// Go-style duration parsing adapts time/format.go from the Go standard library.
// Copyright 2010 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.
use crate::{CallContext, Result};

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
    fn digits(&mut self, limit: u64) -> Result<Option<u64>> {
        let start = self.at;
        let mut value = 0u64;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            let digit = u64::from(self.advance()? - b'0');
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(digit))
                .filter(|&value| value <= limit)
                .ok_or_else(super::invalid)?;
        }
        Ok((self.at != start).then_some(value))
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
        return Err(super::invalid());
    }
    let mut scan = Scan::new(input, ctx);
    let mut nanos = 0u64;
    while scan.peek().is_some() {
        let whole = scan.digits(1 << 63)?;
        let (fraction, scale, post) = if scan.peek() == Some(b'.') {
            scan.advance()?;
            scan.fraction()?
        } else {
            (0, 1.0, false)
        };
        if whole.is_none() && !post {
            return Err(super::invalid());
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
            _ => return Err(super::invalid()),
        };
        let value = whole.unwrap_or(0);
        if value > (1u64 << 63) / unit {
            return Err(super::invalid());
        }
        let value = value * unit + (fraction as f64 * (unit as f64 / scale)) as u64;
        if value > 1 << 63 {
            return Err(super::invalid());
        }
        nanos = nanos.checked_add(value).ok_or_else(super::invalid)?;
        if nanos > 1 << 63 {
            return Err(super::invalid());
        }
    }
    if (!negative && nanos > i64::MAX as u64) || nanos % 1_000_000_000 != 0 {
        return Err(super::invalid());
    }
    let seconds = (nanos / 1_000_000_000) as i64;
    Ok(if negative { -seconds } else { seconds })
}

fn iso(ctx: &mut CallContext, input: &[u8], negative: bool) -> Result<i64> {
    if input.is_empty() || input == b"T" {
        return Err(super::invalid());
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
        if time_at.is_some() || mixed || input.last() != Some(&b'W') {
            return Err(super::invalid());
        }
        let text = &input[..input.len() - 1];
        let negative = text.first() == Some(&b'-');
        let sign = usize::from(matches!(text.first(), Some(b'-' | b'+')));
        let mut scan = Scan::new(&text[sign..], ctx);
        let magnitude = scan
            .digits(i64::MAX as u64 + u64::from(negative))?
            .ok_or_else(super::invalid)?;
        if scan.peek().is_some() {
            return Err(super::invalid());
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
        let value = scan.digits(i64::MAX as u64)?.ok_or_else(super::invalid)? as i64;
        let suffix = scan.peek().ok_or_else(super::invalid)?;
        let index = units[first_unit..]
            .iter()
            .position(|&(unit, _)| unit == suffix)
            .map(|index| first_unit + index)
            .ok_or_else(super::invalid)?;
        scan.advance()?;
        total = total.wrapping_add(value.wrapping_mul(units[index].1));
        first_unit = index + 1;
    }
    Ok(total)
}
