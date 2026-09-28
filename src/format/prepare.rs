use super::*;

pub(super) fn prepare(ctx: &mut CallContext, pattern: &[u8], values: &[Value]) -> Result<Prepared> {
    let mut prepared = Prepared {
        pattern: Buffer::empty(),
        arguments: Buffer::empty(),
    };
    let mut offset = 0;
    let mut next_argument = 0;
    let mut used = 0;
    let mut total = 0;
    while offset < pattern.len() {
        ctx.charge(1)?;
        if pattern[offset] != b'%' {
            let start = offset;
            while offset < pattern.len()
                && pattern[offset] != b'%'
                && offset - start <= LIMIT - total
            {
                offset += 1;
            }
            add(ctx, &mut total, offset - start)?;
            prepared.pattern.extend(ctx, &pattern[start..offset])?;
            continue;
        }
        let start = offset;
        offset += 1;
        if offset == pattern.len() {
            add(ctx, &mut total, b"%!(NOVERB)".len())?;
            prepared.pattern.push(ctx, b'%')?;
            break;
        }
        if pattern[offset] == b'%' {
            add(ctx, &mut total, 1)?;
            prepared.pattern.extend(ctx, b"%%")?;
            offset += 1;
            continue;
        }
        let mut explicit = index(pattern, &mut offset, true);
        let after_leading = offset;
        let mut field = Field::default();
        while offset < pattern.len() && field.take_flag(pattern[offset]) {
            offset += 1;
        }
        field.width = count(ctx, pattern, &mut offset, "width")?;
        if pattern.get(offset) == Some(&b'.') {
            offset += 1;
            field.precision = count(ctx, pattern, &mut offset, "precision")?;
        }
        let before_trailing = offset;
        if let Some(index) = index(pattern, &mut offset, false) {
            explicit = Some(index);
        }
        if offset == pattern.len() {
            add(ctx, &mut total, pattern.len())?;
            prepared.pattern.extend(ctx, &pattern[start..])?;
            break;
        }
        let verb = pattern[offset];
        offset += 1;
        let selected = explicit.unwrap_or(next_argument);
        next_argument = selected.saturating_add(1);
        used = used.max(next_argument);
        let Some(value) = values.get(selected) else {
            return Err(error(
                ctx,
                format_args!(
                    "format references missing operand {}",
                    selected.saturating_add(1)
                ),
            )?);
        };
        let argument = argument(ctx, value, verb, field)?;
        let bytes = project::field(ctx, value, verb, field)?;
        add(ctx, &mut total, bytes)?;
        prepared.pattern.push(ctx, b'%')?;
        prepared
            .pattern
            .extend(ctx, &pattern[after_leading..before_trailing])?;
        prepared.pattern.push(ctx, verb)?;
        prepared.arguments.push(ctx, argument)?;
    }
    if used < values.len() {
        return Err(error(
            ctx,
            format_args!("format has {} unused operand(s)", values.len() - used),
        )?);
    }
    Ok(prepared)
}

fn add(ctx: &mut CallContext, total: &mut usize, length: usize) -> Result<()> {
    *total = total.saturating_add(length);
    if *total > LIMIT {
        return limit(ctx, "output");
    }
    Ok(())
}

pub(super) fn index(pattern: &[u8], offset: &mut usize, dollar: bool) -> Option<usize> {
    let start = *offset;
    let bracket = pattern.get(start) == Some(&b'[');
    if !bracket && !dollar {
        return None;
    }
    let mut end = start + usize::from(bracket);
    let digits = end;
    let mut value = 0usize;
    let mut valid = true;
    while pattern.get(end).is_some_and(u8::is_ascii_digit) {
        if let Some(next) = value
            .checked_mul(10)
            .and_then(|n| n.checked_add((pattern[end] - b'0') as usize))
        {
            value = next;
        } else {
            valid = false;
        }
        end += 1;
    }
    if !valid
        || value == 0
        || value > isize::MAX as usize
        || end == digits
        || pattern.get(end) != Some(if bracket { &b']' } else { &b'$' })
    {
        return None;
    }
    *offset = end + 1;
    Some(value - 1)
}

fn count(
    ctx: &mut CallContext,
    pattern: &[u8],
    offset: &mut usize,
    label: &str,
) -> Result<Option<usize>> {
    index(pattern, offset, false);
    if pattern.get(*offset) == Some(&b'*') {
        return Err(error(
            ctx,
            format_args!("format dynamic {label} is not supported"),
        )?);
    }
    let start = *offset;
    let mut value = 0usize;
    while pattern.get(*offset).is_some_and(u8::is_ascii_digit) {
        ctx.charge(1)?;
        value = value
            .saturating_mul(10)
            .saturating_add((pattern[*offset] - b'0') as usize);
        *offset += 1;
    }
    if value > LIMIT {
        return limit(ctx, label);
    }
    Ok((*offset > start).then_some(value))
}

fn argument(ctx: &mut CallContext, value: &Value, verb: u8, field: Field) -> Result<Argument> {
    let mut text = None;
    let message = match verb {
        b's' | b'q' => None,
        b'x' | b'X'
            if !matches!(
                value.0,
                Kind::Bytes(_) | Kind::Symbol(_) | Kind::Int(_) | Kind::Big(_) | Kind::Float(_)
            ) =>
        {
            Some("string or numeric")
        }
        b'd' | b'b' | b'o' | b'O' if matches!(value.0, Kind::Big(_)) => None,
        b'd' | b'b' | b'o' | b'O' | b'U' | b'c' if integer(value).is_none() => Some("integer"),
        b'f' | b'F' | b'e' | b'E' | b'g' | b'G'
            if !matches!(value.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) =>
        {
            Some("numeric")
        }
        b't' if !matches!(value.0, Kind::Bool(_)) => Some("bool"),
        _ => None,
    };
    if let Some(message) = message {
        return Err(error(
            ctx,
            format_args!("format %{} expects {message} operand", verb as char),
        )?);
    }
    let rendered = matches!(verb, b's' | b'q')
        || (!matches!(
            verb,
            b'x' | b'X'
                | b'd'
                | b'b'
                | b'o'
                | b'O'
                | b'U'
                | b'c'
                | b'f'
                | b'F'
                | b'e'
                | b'E'
                | b'g'
                | b'G'
                | b't'
        ) && string_like(value));
    if rendered {
        text = Some(if matches!(value.0, Kind::Bytes(_) | Kind::Symbol(_)) {
            usize::MAX
        } else if matches!(verb, b's' | b'q' | b'v') {
            match field.precision {
                Some(precision) => precision.saturating_mul(4),
                None => project::string_bytes(ctx, value, None)?,
            }
        } else {
            project::string_bytes(ctx, value, None)?
        });
    }
    let value = if matches!(verb, b'd' | b'b' | b'o' | b'O' | b'U' | b'c')
        && !matches!(value.0, Kind::Big(_))
    {
        Value::int(integer(value).unwrap())
    } else if matches!(verb, b'f' | b'F' | b'e' | b'E' | b'g' | b'G') {
        Value::float(float(value))
    } else {
        value.clone()
    };
    Ok(Argument { value, text })
}
