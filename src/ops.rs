use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    bytecode::Method,
    json,
    scan::{self, Class},
    value::Kind,
};
use std::{cmp::Ordering, fmt::Write};

fn type_error() -> Error {
    Error::new(ErrorKind::Type, "unsupported operand types")
}

pub(crate) fn unary(ctx: &mut CallContext, op: &str, value: Value) -> Result<Value> {
    match (op, &value.0) {
        ("!", _) => Ok(Value::boolean(!value.truthy())),
        ("+", Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) => Ok(value),
        ("-", Kind::Int(n)) if *n != i64::MIN => Ok(Value::int(-n)),
        ("-", Kind::Int(_) | Kind::Big(_)) => crate::integer::negate(ctx, &value, false),
        ("-", Kind::Float(n)) => Ok(Value::float(-n)),
        _ => Err(type_error()),
    }
}

pub(crate) fn binary(ctx: &mut CallContext, op: &str, a: Value, b: Value) -> Result<Value> {
    if op == "<=>" {
        return crate::ordering::spaceship(ctx, &a, &b);
    }
    if op == "===" {
        return Ok(Value::boolean(case_matches(ctx, Some(&b), &a, false)?));
    }
    if matches!(op, "==" | "!=") {
        let same = equal(ctx, &a, &b, 0)?;
        return Ok(Value::boolean(if op == "==" { same } else { !same }));
    }
    if matches!(op, "<" | "<=" | ">" | ">=") {
        let cmp = compare(ctx, &a, &b)?;
        return Ok(Value::boolean(match op {
            "<" => cmp == Some(Ordering::Less),
            "<=" => matches!(cmp, Some(Ordering::Less | Ordering::Equal)),
            ">" => cmp == Some(Ordering::Greater),
            _ => matches!(cmp, Some(Ordering::Greater | Ordering::Equal)),
        }));
    }
    if op == "<<" {
        return a.push(ctx, &[b]);
    }
    if op == "+" && matches!(a.0, Kind::Array(_)) {
        if let Some(values) = b.as_array() {
            return a.push(ctx, values);
        }
    }
    if op == "+" && (matches!(a.0, Kind::Bytes(_)) || matches!(b.0, Kind::Bytes(_))) {
        let scalar = |v: &Value| {
            matches!(
                v.0,
                Kind::Bytes(_)
                    | Kind::Symbol(_)
                    | Kind::Int(_)
                    | Kind::Big(_)
                    | Kind::Float(_)
                    | Kind::Bool(_)
            )
        };
        if !scalar(&a) || !scalar(&b) {
            return Err(type_error());
        }
        let a = to_string(ctx, &a)?;
        let b = to_string(ctx, &b)?;
        let mut out = Buffer::empty();
        out.extend(ctx, a.require_bytes()?)?;
        out.extend(ctx, b.require_bytes()?)?;
        return Value::from_bytes(ctx, out);
    }
    match (&a.0, &b.0) {
        (Kind::Int(a), Kind::Int(b)) => {
            let n = match op {
                "+" => a.checked_add(*b),
                "-" => a.checked_sub(*b),
                "*" => a.checked_mul(*b),
                "/" | "%" => {
                    if *b == 0 {
                        return Err(Error::new(ErrorKind::Arithmetic, "division by zero"));
                    }
                    let Some(q) = a.checked_div(*b) else {
                        return crate::integer::binary(ctx, op, &Value::int(*a), &Value::int(*b));
                    };
                    let r = a % b;
                    let adjust = r != 0 && (r < 0) != (*b < 0);
                    Some(if op == "/" {
                        if adjust { q - 1 } else { q }
                    } else if adjust {
                        r + b
                    } else {
                        r
                    })
                }
                "**" => {
                    if *b < 0 {
                        return float_power(*a as f64, *b as f64);
                    }
                    u32::try_from(*b)
                        .ok()
                        .and_then(|power| a.checked_pow(power))
                }
                _ => return Err(type_error()),
            };
            match n {
                Some(n) => Ok(Value::int(n)),
                None => crate::integer::binary(ctx, op, &Value::int(*a), &Value::int(*b)),
            }
        }
        (Kind::Int(_) | Kind::Big(_), Kind::Int(_) | Kind::Big(_)) => {
            crate::integer::binary(ctx, op, &a, &b)
        }
        (
            Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
            Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
        ) => {
            let a = a.as_float().unwrap();
            let b = b.as_float().unwrap();
            if op == "**" {
                return float_power(a, b);
            }
            Ok(Value::float(match op {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" => a / b,
                _ => return Err(type_error()),
            }))
        }
        (Kind::Bytes(s), Kind::Int(n)) if op == "*" => {
            let n = usize::try_from(*n)
                .map_err(|_| Error::new(ErrorKind::Argument, "negative repeat count"))?;
            let len = s
                .data
                .len()
                .checked_mul(n)
                .ok_or_else(|| Error::new(ErrorKind::Memory, "string size overflow"))?;
            let mut out = Buffer::with_capacity(ctx, len)?;
            if !s.data.is_empty() {
                for _ in 0..n {
                    ctx.charge(1)?;
                    out.extend(ctx, &s.data)?;
                }
            }
            Value::from_bytes(ctx, out)
        }
        _ => Err(type_error()),
    }
}

pub(crate) fn float_power(base: f64, exponent: f64) -> Result<Value> {
    let value = base.powf(exponent);
    if !value.is_finite() {
        return Err(Error::new(
            ErrorKind::Arithmetic,
            "float exponentiation result is not finite",
        ));
    }
    Ok(Value::float(value))
}

pub(crate) fn case_matches(
    ctx: &mut CallContext,
    target: Option<&Value>,
    candidate: &Value,
    splat: bool,
) -> Result<bool> {
    let candidates = if splat {
        candidate
            .as_array()
            .ok_or_else(|| Error::new(ErrorKind::Type, "case when splat value must be an array"))?
    } else {
        std::slice::from_ref(candidate)
    };
    for candidate in candidates {
        ctx.charge(1)?;
        let matched = if let Some(target) = target {
            if let Kind::Range(range) = &candidate.0 {
                range.contains(target)
            } else {
                equal(ctx, candidate, target, 0)?
            }
        } else {
            candidate.truthy()
        };
        if matched {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn compare(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Option<Ordering>> {
    match (&a.0, &b.0) {
        (Kind::Int(a), Kind::Int(b)) => Ok(Some(a.cmp(b))),
        (Kind::Int(_) | Kind::Big(_), Kind::Int(_) | Kind::Big(_)) => {
            crate::integer::compare(ctx, a, b).map(Some)
        }
        (Kind::Big(_), Kind::Float(f)) => crate::integer::compare_float(ctx, a, *f),
        (Kind::Float(f), Kind::Big(_)) => {
            crate::integer::compare_float(ctx, b, *f).map(|order| order.map(Ordering::reverse))
        }
        (Kind::Int(_) | Kind::Float(_), Kind::Int(_) | Kind::Float(_)) => {
            Ok(a.as_float().unwrap().partial_cmp(&b.as_float().unwrap()))
        }
        (Kind::Bytes(a), Kind::Bytes(b)) | (Kind::Symbol(a), Kind::Symbol(b)) => {
            for (a, b) in a.data.chunks(CHUNK).zip(b.data.chunks(CHUNK)) {
                ctx.work_bytes(a.len().min(b.len()))?;
                let cmp = a.cmp(b);
                if cmp != Ordering::Equal {
                    return Ok(Some(cmp));
                }
            }
            Ok(Some(a.data.len().cmp(&b.data.len())))
        }
        _ => Err(type_error()),
    }
}

pub(crate) fn equal(ctx: &mut CallContext, a: &Value, b: &Value, depth: usize) -> Result<bool> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH {
        return ctx.fail(ErrorKind::Recursion, "value nesting too deep");
    }
    match (&a.0, &b.0) {
        (Kind::Nil, Kind::Nil) => Ok(true),
        (Kind::Builtin(a), Kind::Builtin(b)) => Ok(a == b),
        (Kind::Bool(a), Kind::Bool(b)) => Ok(a == b),
        (Kind::Int(a), Kind::Int(b)) => Ok(a == b),
        (Kind::Big(_), Kind::Big(_)) => Ok(crate::integer::compare(ctx, a, b)? == Ordering::Equal),
        (Kind::Big(_), Kind::Float(f)) => {
            Ok(crate::integer::compare_float(ctx, a, *f)? == Some(Ordering::Equal))
        }
        (Kind::Float(f), Kind::Big(_)) => {
            Ok(crate::integer::compare_float(ctx, b, *f)? == Some(Ordering::Equal))
        }
        (Kind::Range(a), Kind::Range(b)) => {
            Ok(a.start == b.start && a.end == b.end && a.exclusive == b.exclusive)
        }
        (Kind::Int(_) | Kind::Float(_), Kind::Int(_) | Kind::Float(_)) => {
            Ok(a.as_float() == b.as_float())
        }
        (Kind::Bytes(a), Kind::Bytes(b)) | (Kind::Symbol(a), Kind::Symbol(b)) => {
            json::bytes_equal(ctx, &a.data, &b.data)
        }
        (Kind::Array(a), Kind::Array(b)) => {
            if a.buffer.data.len() != b.buffer.data.len() {
                return Ok(false);
            }
            for (a, b) in a.buffer.data.iter().zip(&b.buffer.data) {
                if !equal(ctx, a, b, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Kind::Hash(a), Kind::Hash(b)) => {
            if a.object != b.object || a.buffer.data.len() != b.buffer.data.len() {
                return Ok(false);
            }
            for (k, v) in &a.buffer.data {
                let Some(i) = b.find(ctx, k.require_bytes()?)? else {
                    return Ok(false);
                };
                if !equal(ctx, v, &b.buffer.data[i].1, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn index(ctx: &mut CallContext, value: &Value, index: &Value) -> Result<Value> {
    if matches!(index.0, Kind::Range(_)) {
        return crate::sequence::slice(ctx, value, std::slice::from_ref(index), false);
    }
    match &value.0 {
        Kind::Array(h) => {
            let n = normalized(crate::sequence::integer(index)?, h.buffer.data.len());
            Ok(n.and_then(|n| h.buffer.data.get(n))
                .cloned()
                .unwrap_or_default())
        }
        Kind::Hash(h) => {
            let key = index.require_bytes()?;
            Ok(h.find(ctx, key)?
                .map(|i| h.buffer.data[i].1.clone())
                .unwrap_or_default())
        }
        Kind::Bytes(h) => {
            let bytes = &h.data;
            let n = crate::sequence::integer(index)?;
            let n = if n < 0 {
                let (count, _) = runes(ctx, bytes)?;
                normalized(n, count)
            } else {
                usize::try_from(n).ok()
            };
            let Some(n) = n else {
                return Ok(Value::nil());
            };
            let mut pos = 0;
            let mut count = 0;
            while pos < bytes.len() {
                ctx.charge(1)?;
                let (ch, len, valid) = scan::rune(&bytes[pos..]);
                if count == n {
                    if !valid {
                        let mut encoded = [0; 4];
                        return ctx.bytes(ch.encode_utf8(&mut encoded).as_bytes());
                    }
                    return ctx.bytes(&bytes[pos..pos + len]);
                }
                count += 1;
                pos += len;
            }
            Ok(Value::nil())
        }
        _ => Err(type_error()),
    }
}

fn normalized(index: i64, len: usize) -> Option<usize> {
    if index < 0 {
        len.checked_sub(usize::try_from(index.unsigned_abs()).ok()?)
    } else {
        usize::try_from(index).ok()
    }
}

pub(crate) fn set_index(
    ctx: &mut CallContext,
    root: Value,
    key: Value,
    value: Value,
) -> Result<Value> {
    match &root.0 {
        Kind::Array(h) => {
            let n = normalized(crate::sequence::integer(&key)?, h.buffer.data.len())
                .ok_or_else(|| Error::new(ErrorKind::Argument, "array index too small"))?;
            let len = h.buffer.data.len();
            if n >= len {
                return Err(Error::new(ErrorKind::Argument, "array index out of bounds"));
            }
            root.set_array_index(ctx, n, value)
        }
        Kind::Hash(_) => {
            let key = if matches!(key.0, Kind::Symbol(_)) {
                ctx.bytes(key.require_bytes()?)?
            } else {
                key
            };
            key.require_bytes()?;
            root.set_hash_index(ctx, key, value)
        }
        _ => Err(type_error()),
    }
}

pub(crate) fn arity(args: &[Value], n: usize) -> Result<()> {
    if args.len() == n {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Argument,
            format!("expected {n} arguments, got {}", args.len()),
        ))
    }
}

pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    value: Value,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    match method {
        IsNil => {
            arity(args, 0)?;
            return Ok(Value::boolean(matches!(value.0, Kind::Nil)));
        }
        Itself | Dup => {
            arity(args, 0)?;
            return Ok(value);
        }
        ToString => {
            arity(args, 0)?;
            if matches!(value.0, Kind::Hash(_)) {
                return Err(type_error());
            }
            return to_string(ctx, &value);
        }
        _ => (),
    }
    if let Kind::Range(range) = &value.0 {
        return crate::range::method(ctx, method, range, args);
    }
    match method {
        IsNil | Itself | Dup | ToString => unreachable!(),
        Prepend | Pop | Shift | Delete | Insert | Clear | Fill | Store | Replace => {
            crate::mutate::call(ctx, method, value, args).map(|(_, result)| result)
        }
        Empty => {
            arity(args, 0)?;
            Ok(Value::boolean(match &value.0 {
                Kind::Bytes(h) => h.data.is_empty(),
                Kind::Array(h) => h.buffer.data.is_empty(),
                Kind::Hash(h) => h.buffer.data.is_empty(),
                _ => return Err(type_error()),
            }))
        }
        Abs => {
            arity(args, 0)?;
            match value.0 {
                Kind::Int(n) if n != i64::MIN => Ok(Value::int(n.abs())),
                Kind::Int(_) | Kind::Big(_) => crate::integer::negate(ctx, &value, true),
                Kind::Float(n) => Ok(Value::float(n.abs())),
                _ => Err(type_error()),
            }
        }
        Even | Odd => {
            arity(args, 0)?;
            Ok(Value::boolean(if value.is_integer() {
                crate::integer::odd(&value) == matches!(method, Odd)
            } else {
                return Err(type_error());
            }))
        }
        Reverse if matches!(value.0, Kind::Bytes(_)) => {
            crate::text::method(ctx, method, value, args)
        }
        Ord | Chr | Bytes | Chars | Codepoints | StartWith | EndWith => {
            crate::text::method(ctx, method, value, args)
        }
        Reverse | Take | Drop | Compact | Uniq | Flatten | Chunk | Window | Zip | Transpose
        | ToHash | Fetch | Dig | Key | HasValue | Member | RemapKeys | Except => {
            crate::collections::method(ctx, method, value, args)
        }
        Slice if matches!(value.0, Kind::Hash(_)) => {
            crate::collections::method(ctx, method, value, args)
        }
        At | Slice | ByteSlice | GetByte | First | Last | ToArray => {
            crate::sequence::method(ctx, method, value, args)
        }
        Cover | ExcludeEnd => Err(type_error()),
        Length | Size => {
            arity(args, 0)?;
            let n = match &value.0 {
                Kind::Bytes(h) => runes(ctx, &h.data)?.0,
                Kind::Array(h) => h.buffer.data.len(),
                Kind::Hash(h) => h.buffer.data.len(),
                _ => return Err(type_error()),
            };
            Ok(Value::int(n as i64))
        }
        ByteSize => {
            arity(args, 0)?;
            Ok(Value::int(value.require_bytes()?.len() as i64))
        }
        Upcase | Downcase => {
            let bytes = value.require_bytes()?;
            if !args.is_empty() {
                arity(args, 1)?;
                if !matches!(args[0].0, Kind::Symbol(_)) || args[0].require_bytes()? != b"ascii" {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "only the :ascii case option is implemented",
                    ));
                }
            } else {
                for bytes in bytes.chunks(CHUNK) {
                    ctx.work_bytes(bytes.len())?;
                    if scan::prefix(bytes, Class::Ascii) != bytes.len() {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            "Unicode case conversion is not implemented; use :ascii",
                        ));
                    }
                }
            }
            let mut out = Buffer::with_capacity(ctx, bytes.len())?;
            out.extend(ctx, bytes)?;
            for bytes in out.data.chunks_mut(CHUNK) {
                ctx.work_bytes(bytes.len())?;
                scan::ascii_case(bytes, matches!(method, Upcase));
            }
            Value::from_bytes(ctx, out)
        }
        Include | Index | Rindex => {
            arity(args, 1)?;
            if matches!(method, Include) && matches!(value.0, Kind::Hash(_)) {
                return crate::collections::method(ctx, Key, value, args);
            }
            let found = if let Some(array) = value.as_array() {
                let mut found = None;
                for (i, item) in array.iter().enumerate() {
                    if equal(ctx, item, &args[0], 0)? {
                        found = Some(i);
                        if !matches!(method, Rindex) {
                            break;
                        }
                    }
                }
                found
            } else {
                let bytes = value.require_bytes()?;
                let needle = args[0].require_bytes()?;
                let found = find(ctx, bytes, needle, matches!(method, Rindex))?;
                if let Some(pos) = found {
                    Some(runes(ctx, &bytes[..pos])?.0)
                } else {
                    None
                }
            };
            if matches!(method, Include) {
                Ok(Value::boolean(found.is_some()))
            } else {
                Ok(found.map(|n| Value::int(n as i64)).unwrap_or_default())
            }
        }
        Strip => {
            arity(args, 0)?;
            let bytes = value.require_bytes()?;
            let (start, end) = trim_ascii(ctx, bytes)?;
            if start == 0 && end == bytes.len() {
                Ok(value)
            } else {
                ctx.bytes(&bytes[start..end])
            }
        }
        Split => split(ctx, &value, args),
        Join => join(ctx, &value, args),
        Push => value.push(ctx, args),
        Sum => {
            if args.len() > 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "sum accepts at most an initial value",
                ));
            }
            let array = value.as_array().ok_or_else(type_error)?;
            let mut sum = args.first().cloned().unwrap_or_else(|| Value::int(0));
            for item in array {
                ctx.charge(1)?;
                if matches!(sum.0, Kind::Bytes(_)) != matches!(item.0, Kind::Bytes(_)) {
                    return Err(type_error());
                }
                sum = binary(ctx, "+", sum, item.clone())?;
            }
            Ok(sum)
        }
        Keys | Values => {
            arity(args, 0)?;
            let entries = value.as_hash().ok_or_else(type_error)?;
            let mut out = Buffer::with_capacity(ctx, entries.len())?;
            for (k, v) in entries {
                ctx.charge(1)?;
                out.data.push(if matches!(method, Keys) {
                    k.clone()
                } else {
                    v.clone()
                });
            }
            Value::from_array(ctx, out)
        }
        ToFloat => {
            arity(args, 0)?;
            value.as_float().map(Value::float).ok_or_else(type_error)
        }
        ToInt => {
            arity(args, 0)?;
            match &value.0 {
                Kind::Int(_) | Kind::Big(_) => Ok(value),
                Kind::Float(n) => crate::integer::from_float(ctx, *n),
                _ => {
                    let bytes = value.require_bytes()?;
                    let (start, end) = trim(ctx, bytes)?;
                    let text = std::str::from_utf8(&bytes[start..end]).map_err(|_| type_error())?;
                    for chunk in text.as_bytes().chunks(CHUNK) {
                        ctx.work_bytes(chunk.len())?;
                    }
                    crate::integer::parse(ctx, text.as_bytes(), 10)
                }
            }
        }
    }
}

pub(crate) fn runes(ctx: &mut CallContext, bytes: &[u8]) -> Result<(usize, bool)> {
    let mut i = 0;
    let mut count = 0;
    let mut valid = true;
    while i < bytes.len() {
        let end = bytes.len().min(i + CHUNK);
        let chunk = &bytes[i..end];
        let ascii = scan::prefix(chunk, Class::Ascii);
        if ascii > 0 {
            ctx.work_bytes(ascii)?;
            i += ascii;
            count += ascii;
            continue;
        }
        let span = scan::unicode_span(chunk);
        if span.len > 0 {
            ctx.charge(span.steps)?;
            ctx.checkpoint()?;
            i += span.len;
            count += span.runes;
        } else {
            ctx.charge(1)?;
            let (_, n, ok) = scan::rune(&bytes[i..]);
            i += n;
            count += 1;
            valid &= ok;
        }
    }
    Ok((count, valid))
}

fn trim_ascii(ctx: &mut CallContext, bytes: &[u8]) -> Result<(usize, usize)> {
    let mut start = 0;
    while start < bytes.len() && matches!(bytes[start], 0 | 9..=13 | 32) {
        ctx.charge(1)?;
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && matches!(bytes[end - 1], 0 | 9..=13 | 32) {
        ctx.charge(1)?;
        end -= 1;
    }
    Ok((start, end))
}

pub(crate) fn trim(ctx: &mut CallContext, bytes: &[u8]) -> Result<(usize, usize)> {
    let mut start = 0;
    while start < bytes.len() {
        ctx.charge(1)?;
        let (ch, n, _) = scan::rune(&bytes[start..]);
        if !ch.is_whitespace() {
            break;
        }
        start += n;
    }
    let mut end = bytes.len();
    while end > start {
        ctx.charge(1)?;
        let mut pos = end - 1;
        for _ in 0..3 {
            if pos == start || bytes[pos] & 0xc0 != 0x80 {
                break;
            }
            pos -= 1;
        }
        let (ch, n, valid) = scan::rune(&bytes[pos..end]);
        if !valid || pos + n != end || !ch.is_whitespace() {
            break;
        }
        end = pos;
    }
    Ok((start, end))
}

pub(crate) fn find(
    ctx: &mut CallContext,
    bytes: &[u8],
    needle: &[u8],
    last: bool,
) -> Result<Option<usize>> {
    if needle.is_empty() {
        return Ok(Some(if last { bytes.len() } else { 0 }));
    }
    if needle.len() > bytes.len() {
        return Ok(None);
    }
    let mut table = Buffer::with_capacity(ctx, needle.len())?;
    table.data.push(0usize);
    let mut matched = 0;
    for i in 1..needle.len() {
        ctx.charge(1)?;
        while matched > 0 && needle[i] != needle[matched] {
            ctx.charge(1)?;
            matched = table.data[matched - 1];
        }
        if needle[i] == needle[matched] {
            matched += 1;
        }
        table.data.push(matched);
    }
    let mut matched = 0;
    let mut found = None;
    for (i, &b) in bytes.iter().enumerate() {
        ctx.charge(1)?;
        while matched > 0 && b != needle[matched] {
            ctx.charge(1)?;
            matched = table.data[matched - 1];
        }
        if b == needle[matched] {
            matched += 1;
        }
        if matched == needle.len() {
            found = Some(i + 1 - needle.len());
            if !last {
                break;
            }
            matched = table.data[matched - 1];
        }
    }
    Ok(found)
}

fn split(ctx: &mut CallContext, value: &Value, args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(Error::new(
            ErrorKind::Argument,
            "split accepts zero or one argument in this core",
        ));
    }
    let bytes = value.require_bytes()?;
    let mut out = Buffer::empty();
    if args.is_empty() || args[0].as_bytes() == Some(b" ") {
        let mut i = 0;
        let mut start = None;
        while i < bytes.len() {
            ctx.charge(1)?;
            let (ch, n, _) = scan::rune(&bytes[i..]);
            if ch.is_ascii() && matches!(ch as u8, b' ' | 9..=13) {
                if let Some(start) = start.take() {
                    let part = ctx.bytes(&bytes[start..i])?;
                    out.push(ctx, part)?;
                }
            } else if start.is_none() {
                start = Some(i);
            }
            i += n;
        }
        if let Some(start) = start {
            let part = ctx.bytes(&bytes[start..])?;
            out.push(ctx, part)?;
        }
    } else {
        let delim = args[0].require_bytes()?;
        let mut i = 0;
        if delim.is_empty() {
            while i < bytes.len() {
                ctx.charge(1)?;
                let (_, n, _) = scan::rune(&bytes[i..]);
                let part = ctx.bytes(&bytes[i..i + n])?;
                out.push(ctx, part)?;
                i += n;
            }
        } else {
            while i < bytes.len() {
                let Some(n) = find(ctx, &bytes[i..], delim, false)? else {
                    let part = ctx.bytes(&bytes[i..])?;
                    out.push(ctx, part)?;
                    break;
                };
                let part = ctx.bytes(&bytes[i..i + n])?;
                out.push(ctx, part)?;
                i += n + delim.len();
            }
            while out.data.last().is_some_and(|v| v.as_bytes() == Some(b"")) {
                out.data.pop();
            }
        }
    }
    Value::from_array(ctx, out)
}

fn join(ctx: &mut CallContext, value: &Value, args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(Error::new(
            ErrorKind::Argument,
            "join expects at most one separator",
        ));
    }
    let sep = if args.is_empty() {
        b"".as_slice()
    } else {
        args[0].require_bytes()?
    };
    let array = value.as_array().ok_or_else(type_error)?;
    let mut out = Buffer::empty();
    join_into(ctx, array, sep, &mut out, 0)?;
    Value::from_bytes(ctx, out)
}

fn join_into(
    ctx: &mut CallContext,
    array: &[Value],
    sep: &[u8],
    out: &mut Buffer<u8>,
    depth: usize,
) -> Result<()> {
    if depth > MAX_VALUE_DEPTH {
        return ctx.fail(ErrorKind::Recursion, "join nesting too deep");
    }
    for (i, v) in array.iter().enumerate() {
        ctx.charge(1)?;
        if i > 0 {
            out.extend(ctx, sep)?;
        }
        if let Some(nested) = v.as_array() {
            join_into(ctx, nested, sep, out, depth + 1)?;
        } else {
            let v = to_string(ctx, v)?;
            out.extend(ctx, v.require_bytes()?)?;
        }
    }
    Ok(())
}

pub(crate) fn to_string(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut text = json::Number::new();
    match &value.0 {
        Kind::Builtin(builtin) => return Err(builtin.value_error()),
        Kind::Big(_) => {
            let text = crate::integer::format(ctx, value, 10)?;
            return Value::from_bytes(ctx, text);
        }
        Kind::Bytes(_) => return Ok(value.clone()),
        Kind::Symbol(h) => return ctx.bytes(&h.data),
        Kind::Nil => return ctx.bytes(b""),
        Kind::Bool(v) => return ctx.bytes(if *v { b"true" } else { b"false" }),
        Kind::Int(n) => write!(text, "{n}").unwrap(),
        Kind::Float(n) => format_float(&mut text, *n),
        _ => return crate::text::display(ctx, value),
    }
    ctx.bytes(text.bytes())
}

fn format_float(out: &mut json::Number, value: f64) {
    if value.is_nan() {
        out.write_str("NaN").unwrap();
    } else if value.is_infinite() {
        out.write_str(if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        })
        .unwrap();
    } else if value != 0.0 && !(1e-4..1e6).contains(&value.abs()) {
        let mut scientific = json::Number::new();
        write!(scientific, "{value:e}").unwrap();
        let text = std::str::from_utf8(scientific.bytes()).unwrap();
        let (mantissa, exponent) = text.split_once('e').unwrap();
        let exponent: i32 = exponent.parse().unwrap();
        write!(out, "{mantissa}e{exponent:+03}").unwrap();
    } else {
        write!(out, "{value}").unwrap();
    }
}
