use super::{Stamp, calendar};
use crate::{CallContext, Error, ErrorKind, Result};

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid RFC3339 time")
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
