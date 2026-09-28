use super::*;
use crate::{budget::CHUNK, scan};

mod number;

struct Output {
    bytes: Option<Buffer<u8>>,
    length: usize,
}

impl Output {
    fn write(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        if bytes.len() > LIMIT - self.length {
            return limit(ctx, "output");
        }
        self.length += bytes.len();
        if let Some(output) = &mut self.bytes {
            output.extend(ctx, bytes)
        } else {
            ctx.work_bytes(bytes.len())
        }
    }

    fn repeat(&mut self, ctx: &mut CallContext, byte: u8, count: usize) -> Result<()> {
        if count > LIMIT - self.length {
            return limit(ctx, "output");
        }
        if self.bytes.is_none() {
            self.length += count;
            return ctx.work_bytes(count);
        }
        let chunk = [byte; CHUNK];
        let mut remaining = count;
        while remaining > 0 {
            let count = remaining.min(chunk.len());
            self.write(ctx, &chunk[..count])?;
            remaining -= count;
        }
        Ok(())
    }
}

pub(super) fn render(ctx: &mut CallContext, prepared: &Prepared) -> Result<Value> {
    let mut output = Output {
        bytes: None,
        length: 0,
    };
    printf(ctx, prepared, &mut output)?;
    output.bytes = Some(Buffer::with_capacity(ctx, output.length)?);
    output.length = 0;
    printf(ctx, prepared, &mut output)?;
    Value::from_bytes(ctx, output.bytes.unwrap())
}

fn printf(ctx: &mut CallContext, prepared: &Prepared, output: &mut Output) -> Result<()> {
    let pattern = &prepared.pattern.data;
    let args = &prepared.arguments.data;
    let mut offset = 0;
    let mut argument = 0;
    let mut reordered = false;
    while offset < pattern.len() {
        ctx.charge(1)?;
        let start = offset;
        while offset < pattern.len() && pattern[offset] != b'%' {
            offset += 1;
        }
        output.write(ctx, &pattern[start..offset])?;
        if offset == pattern.len() {
            break;
        }
        offset += 1;
        let mut field = Field::default();
        while offset < pattern.len() && field.take_flag(pattern[offset]) {
            offset += 1;
        }
        let mut good = true;
        let mut indexed = arg_index(
            pattern,
            &mut offset,
            &mut argument,
            args.len(),
            &mut reordered,
            &mut good,
        );
        if pattern.get(offset) == Some(&b'*') {
            offset += 1;
            field.width = match star(args, &mut argument) {
                Some(width) => {
                    if width < 0 {
                        field.flags = (field.flags | MINUS) & !ZERO;
                    }
                    Some(width.unsigned_abs() as usize)
                }
                None => {
                    output.write(ctx, b"%!(BADWIDTH)")?;
                    None
                }
            };
            indexed = false;
        } else {
            field.width = count(pattern, &mut offset);
            if indexed && field.width.is_some() {
                good = false;
            }
        }
        if offset + 1 < pattern.len() && pattern[offset] == b'.' {
            offset += 1;
            if indexed {
                good = false;
            }
            indexed = arg_index(
                pattern,
                &mut offset,
                &mut argument,
                args.len(),
                &mut reordered,
                &mut good,
            );
            if pattern.get(offset) == Some(&b'*') {
                offset += 1;
                field.precision = match star(args, &mut argument).filter(|n| *n >= 0) {
                    Some(precision) => Some(precision as usize),
                    None => {
                        output.write(ctx, b"%!(BADPREC)")?;
                        None
                    }
                };
                indexed = false;
            } else {
                field.precision = Some(count(pattern, &mut offset).unwrap_or(0));
            }
        }
        if !indexed {
            arg_index(
                pattern,
                &mut offset,
                &mut argument,
                args.len(),
                &mut reordered,
                &mut good,
            );
        }
        if offset == pattern.len() {
            output.write(ctx, b"%!(NOVERB)")?;
            break;
        }
        let (verb, width, _) = scan::rune(&pattern[offset..]);
        offset += width;
        if verb == '%' {
            output.write(ctx, b"%")?;
        } else if !good || argument >= args.len() {
            output.write(ctx, b"%!")?;
            let mut bytes = [0; 4];
            output.write(ctx, verb.encode_utf8(&mut bytes).as_bytes())?;
            output.write(ctx, if good { b"(MISSING)" } else { b"(BADINDEX)" })?;
        } else {
            item(ctx, &args[argument], verb, field, output)?;
            argument += 1;
        }
    }
    if !reordered && argument < args.len() {
        output.write(ctx, b"%!(EXTRA ")?;
        for (index, argument) in args[argument..].iter().enumerate() {
            if index > 0 {
                output.write(ctx, b", ")?;
            }
            if argument.text.is_none() && matches!(argument.value.0, Kind::Nil) {
                output.write(ctx, b"<nil>")?;
            } else {
                output.write(ctx, type_name(argument).as_bytes())?;
                output.write(ctx, b"=")?;
                item(ctx, argument, 'v', Field::default(), output)?;
            }
        }
        output.write(ctx, b")")?;
    }
    Ok(())
}

fn arg_index(
    pattern: &[u8],
    offset: &mut usize,
    argument: &mut usize,
    length: usize,
    reordered: &mut bool,
    good: &mut bool,
) -> bool {
    if pattern.get(*offset) != Some(&b'[') {
        return false;
    }
    *reordered = true;
    let start = *offset;
    let close = (pattern.len() - start >= 3)
        .then(|| pattern[start + 1..].iter().position(|b| *b == b']'))
        .flatten()
        .map(|n| start + n + 1);
    let Some(close) = close else {
        *offset += 1;
        *good = false;
        return false;
    };
    let mut end = start + 1;
    let number = count(&pattern[..close], &mut end);
    *offset = close + 1;
    if let Some(number) = number.filter(|_| end == close) {
        if number > 0 && number <= length {
            *argument = number - 1;
        } else {
            *good = false;
        }
        return true;
    }
    *good = false;
    false
}

fn count(pattern: &[u8], offset: &mut usize) -> Option<usize> {
    let start = *offset;
    let mut count = 0usize;
    while pattern.get(*offset).is_some_and(u8::is_ascii_digit) {
        if count > 1_000_000 {
            *offset = pattern.len();
            return None;
        }
        count = count
            .saturating_mul(10)
            .saturating_add((pattern[*offset] - b'0') as usize);
        *offset += 1;
    }
    (*offset != start).then_some(count)
}

fn star(args: &[Argument], argument: &mut usize) -> Option<i64> {
    let arg = args.get(*argument)?;
    *argument += 1;
    if arg.text.is_none() {
        if let Kind::Int(value @ -1_000_000..=1_000_000) = arg.value.0 {
            return Some(value);
        }
    }
    None
}

fn type_name(argument: &Argument) -> &'static str {
    if argument.text.is_some() {
        return "string";
    }
    match argument.value.0 {
        Kind::Int(_) => "int64",
        Kind::Big(_) => "*big.Int",
        Kind::Float(_) => "float64",
        Kind::Bool(_) => "bool",
        Kind::Nil => "<nil>",
        _ => "string",
    }
}

fn text(ctx: &mut CallContext, argument: &Argument) -> Result<Value> {
    if matches!(argument.value.0, Kind::Bytes(_) | Kind::Symbol(_)) {
        return Ok(argument.value.clone());
    }
    crate::text::bounded::prefix(ctx, &argument.value, argument.text.unwrap_or(usize::MAX))
}

fn pad(ctx: &mut CallContext, output: &mut Output, bytes: &[u8], field: Field) -> Result<()> {
    let width = field.width.unwrap_or(0);
    let count = if width == 0 {
        0
    } else {
        width.saturating_sub(crate::ops::runes(ctx, bytes)?.0)
    };
    if !field.flag(MINUS) {
        output.repeat(ctx, if field.flag(ZERO) { b'0' } else { b' ' }, count)?;
    }
    output.write(ctx, bytes)?;
    if field.flag(MINUS) {
        output.repeat(ctx, b' ', count)?;
    }
    Ok(())
}

fn item(
    ctx: &mut CallContext,
    argument: &Argument,
    verb: char,
    mut field: Field,
    output: &mut Output,
) -> Result<()> {
    if verb == 'T' {
        let bytes = type_name(argument).as_bytes();
        let length = if !matches!(argument.value.0, Kind::Nil) || argument.text.is_some() {
            field.precision.map_or(bytes.len(), |n| n.min(bytes.len()))
        } else {
            bytes.len()
        };
        return pad(ctx, output, &bytes[..length], field);
    }
    if argument.text.is_some() || matches!(argument.value.0, Kind::Bytes(_) | Kind::Symbol(_)) {
        let value = text(ctx, argument)?;
        let bytes = value.require_bytes()?;
        if matches!(verb, 's' | 'v' | 'q' | 'x' | 'X') {
            let bytes = if let Some(precision) = field.precision {
                let length = if matches!(verb, 'x' | 'X') {
                    bytes.len().min(precision)
                } else {
                    precision_bytes(ctx, bytes, precision)?
                };
                &bytes[..length]
            } else {
                bytes
            };
            return match verb {
                'q' => quote(ctx, output, bytes, field),
                'v' if field.flag(SHARP) => {
                    field.flags &= !(SHARP | PLUS);
                    quote(ctx, output, bytes, field)
                }
                'x' | 'X' => hex(ctx, output, bytes, verb == 'X', field),
                _ => pad(ctx, output, bytes, field),
            };
        }
    } else {
        match &argument.value.0 {
            Kind::Nil if verb == 'v' => return pad(ctx, output, b"<nil>", field),
            Kind::Bool(value) if matches!(verb, 't' | 'v') => {
                return pad(ctx, output, if *value { b"true" } else { b"false" }, field);
            }
            Kind::Int(_) | Kind::Big(_)
                if matches!(verb, 'v' | 'd' | 'b' | 'o' | 'O' | 'x' | 'X' | 'c' | 'U') =>
            {
                return number::integer(ctx, output, &argument.value, verb, field);
            }
            Kind::Float(value)
                if matches!(
                    verb,
                    'v' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'x' | 'X' | 'b'
                ) =>
            {
                return number::float(ctx, output, *value, verb, field);
            }
            Kind::Big(value) if verb == 'p' => {
                field.flags ^= SHARP;
                let address = std::sync::Arc::as_ptr(value) as usize;
                return number::unsigned(ctx, output, address as u64, 'x', field);
            }
            Kind::Big(_) if verb == 'w' => {
                output.write(ctx, b"%!w(*big.Int=")?;
                number::debug_big(ctx, output, &argument.value, field)?;
                return output.write(ctx, b")");
            }
            Kind::Big(_) if verb != 'w' => {
                output.write(ctx, b"%!")?;
                let mut bytes = [0; 4];
                output.write(ctx, verb.encode_utf8(&mut bytes).as_bytes())?;
                output.write(ctx, b"(big.Int=")?;
                number::integer(ctx, output, &argument.value, 'd', Field::default())?;
                return output.write(ctx, b")");
            }
            _ => (),
        }
    }
    output.write(ctx, b"%!")?;
    let mut bytes = [0; 4];
    output.write(ctx, verb.encode_utf8(&mut bytes).as_bytes())?;
    output.write(ctx, b"(")?;
    output.write(ctx, type_name(argument).as_bytes())?;
    if argument.text.is_some() || !matches!(argument.value.0, Kind::Nil) {
        output.write(ctx, b"=")?;
        if verb == 'w'
            && field.flag(SHARP)
            && (argument.text.is_some()
                || matches!(argument.value.0, Kind::Bytes(_) | Kind::Symbol(_)))
        {
            item(ctx, argument, 'v', field, output)?;
            return output.write(ctx, b")");
        }
        if matches!(verb, 'w' | 'v') {
            field.flags &= !(SHARP | PLUS);
        }
        if argument.text.is_some() || matches!(argument.value.0, Kind::Bytes(_) | Kind::Symbol(_)) {
            item(ctx, argument, 's', field, output)?;
        } else {
            match &argument.value.0 {
                Kind::Int(_) => number::integer(ctx, output, &argument.value, 'd', field)?,
                Kind::Float(value) => number::float(ctx, output, *value, 'g', field)?,
                _ => item(ctx, argument, 'v', field, output)?,
            }
        }
    }
    output.write(ctx, b")")
}

fn hex(
    ctx: &mut CallContext,
    output: &mut Output,
    bytes: &[u8],
    uppercase: bool,
    field: Field,
) -> Result<()> {
    let each = if field.flag(SPACE) {
        3 + 2 * usize::from(field.flag(SHARP))
    } else {
        2
    };
    let length = if bytes.is_empty() {
        0
    } else {
        bytes
            .len()
            .saturating_mul(each)
            .saturating_sub(usize::from(field.flag(SPACE)))
            + if field.flag(SHARP) && !field.flag(SPACE) {
                2
            } else {
                0
            }
    };
    let padding = field.width.unwrap_or(0).saturating_sub(length);
    if !field.flag(MINUS) {
        output.repeat(ctx, if field.flag(ZERO) { b'0' } else { b' ' }, padding)?;
    }
    let digits = if uppercase {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && field.flag(SPACE) {
            output.write(ctx, b" ")?;
        }
        if field.flag(SHARP) && (index == 0 || field.flag(SPACE)) {
            output.write(ctx, if uppercase { b"0X" } else { b"0x" })?;
        }
        output.write(
            ctx,
            &[digits[(byte >> 4) as usize], digits[(byte & 15) as usize]],
        )?;
    }
    if field.flag(MINUS) {
        output.repeat(ctx, b' ', padding)?;
    }
    Ok(())
}

fn quote(ctx: &mut CallContext, output: &mut Output, bytes: &[u8], field: Field) -> Result<()> {
    let mut rendered = Buffer::empty();
    let raw = field.flag(SHARP) && can_backquote(ctx, bytes)?;
    if raw {
        rendered.push(ctx, 96)?;
        rendered.extend(ctx, bytes)?;
        rendered.push(ctx, 96)?;
    } else {
        rendered.push(ctx, b'"')?;
        let mut offset = 0;
        while offset < bytes.len() {
            ctx.charge(1)?;
            let (rune, width, valid) = scan::rune(&bytes[offset..]);
            if !valid {
                escape(ctx, &mut rendered, bytes[offset] as u32, b'x', 2)?;
            } else {
                match rune {
                    '"' | '\\' => rendered.extend(ctx, &[b'\\', rune as u8])?,
                    '\u{7}' => rendered.extend(ctx, b"\\a")?,
                    '\u{8}' => rendered.extend(ctx, b"\\b")?,
                    '\u{c}' => rendered.extend(ctx, b"\\f")?,
                    '\n' => rendered.extend(ctx, b"\\n")?,
                    '\r' => rendered.extend(ctx, b"\\r")?,
                    '\t' => rendered.extend(ctx, b"\\t")?,
                    '\u{b}' => rendered.extend(ctx, b"\\v")?,
                    c if c.is_ascii() && (!c.is_ascii_control()) => rendered.push(ctx, c as u8)?,
                    c if !field.flag(PLUS) && crate::printable::is_print(c) => {
                        rendered.extend(ctx, &bytes[offset..offset + width])?
                    }
                    c if c < ' ' || c == '\u{7f}' => escape(ctx, &mut rendered, c as u32, b'x', 2)?,
                    c if c <= '\u{ffff}' => escape(ctx, &mut rendered, c as u32, b'u', 4)?,
                    c => escape(ctx, &mut rendered, c as u32, b'U', 8)?,
                }
            }
            offset += width;
        }
        rendered.push(ctx, b'"')?;
    }
    pad(ctx, output, &rendered.data, field)
}

fn escape(
    ctx: &mut CallContext,
    output: &mut Buffer<u8>,
    value: u32,
    prefix: u8,
    digits: usize,
) -> Result<()> {
    output.extend(ctx, &[b'\\', prefix])?;
    for shift in (0..digits).rev() {
        output.push(
            ctx,
            b"0123456789abcdef"[((value >> (shift * 4)) & 15) as usize],
        )?;
    }
    Ok(())
}

fn can_backquote(ctx: &mut CallContext, bytes: &[u8]) -> Result<bool> {
    let mut offset = 0;
    while offset < bytes.len() {
        ctx.charge(1)?;
        let (rune, width, valid) = scan::rune(&bytes[offset..]);
        if !valid
            || rune == '\u{feff}'
            || rune == '\u{7f}'
            || rune == '\u{60}'
            || (rune < ' ' && rune != '\t')
        {
            return Ok(false);
        }
        offset += width;
    }
    Ok(true)
}
