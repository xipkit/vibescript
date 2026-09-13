use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    hash::Hash,
    scan::{self, Class},
    value::Kind,
};
use std::fmt::{self, Write};

pub(crate) fn parse(ctx: &mut CallContext, input: &[u8]) -> Result<Value> {
    ctx.checkpoint()?;
    let mut p = Parser { ctx, input, pos: 0 };
    let v = p.value(0)?;
    p.space()?;
    if p.pos != input.len() {
        return p.err("trailing JSON data");
    }
    Ok(v)
}
struct Parser<'a> {
    ctx: &'a mut CallContext,
    input: &'a [u8],
    pos: usize,
}
impl Parser<'_> {
    fn err<T>(&self, msg: &str) -> Result<T> {
        Err(Error::new(
            ErrorKind::Json,
            format!("{msg} at byte {}", self.pos),
        ))
    }
    fn space(&mut self) -> Result<()> {
        while self
            .input
            .get(self.pos)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            let start = self.pos;
            while self.pos < self.input.len()
                && self.pos - start < CHUNK
                && matches!(self.input[self.pos], b' ' | b'\n' | b'\r' | b'\t')
            {
                self.pos += 1;
            }
            self.ctx.work_bytes(self.pos - start)?;
        }
        Ok(())
    }
    fn take(&mut self, b: u8) -> bool {
        if self.input.get(self.pos) == Some(&b) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        self.ctx.charge(1)?;
        if depth > MAX_VALUE_DEPTH {
            return self.ctx.fail(ErrorKind::Recursion, "JSON nesting too deep");
        }
        self.space()?;
        match self.input.get(self.pos).copied() {
            Some(b'"') => self.string(),
            Some(b'[') => {
                self.pos += 1;
                self.space()?;
                let mut out = Buffer::empty();
                if !self.take(b']') {
                    loop {
                        let value = self.value(depth + 1)?;
                        out.push(self.ctx, value)?;
                        self.space()?;
                        if self.take(b']') {
                            break;
                        }
                        if !self.take(b',') {
                            return self.err("expected comma or closing bracket");
                        }
                    }
                }
                Value::from_array(self.ctx, out)
            }
            Some(b'{') => {
                self.pos += 1;
                self.space()?;
                let mut out = Hash::empty();
                if !self.take(b'}') {
                    loop {
                        self.space()?;
                        if self.input.get(self.pos) != Some(&b'"') {
                            return self.err("expected JSON object key");
                        }
                        let key = self.string()?;
                        self.space()?;
                        if !self.take(b':') {
                            return self.err("expected colon");
                        }
                        let value = self.value(depth + 1)?;
                        out.insert(self.ctx, key, value)?;
                        self.space()?;
                        if self.take(b'}') {
                            break;
                        }
                        if !self.take(b',') {
                            return self.err("expected comma or closing brace");
                        }
                    }
                }
                Value::from_hash(self.ctx, out)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(Value::boolean(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(Value::boolean(false))
            }
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(Value::nil())
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err("expected JSON value"),
        }
    }
    fn literal(&mut self, literal: &[u8]) -> Result<()> {
        if self.input[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            self.err("invalid JSON literal")
        }
    }
    fn string(&mut self) -> Result<Value> {
        self.pos += 1;
        let start = self.pos;
        loop {
            if self.pos >= self.input.len() {
                return self.err("unterminated JSON string");
            }
            let end = self.input.len().min(self.pos + CHUNK);
            let span = scan::text_span(&self.input[self.pos..end], Class::JsonParse);
            if span.len > 0 {
                self.ctx.charge(span.steps)?;
                self.pos += span.len;
                continue;
            }
            if self.input[self.pos] == b'"' {
                let value = self.ctx.bytes(&self.input[start..self.pos])?;
                self.pos += 1;
                return Ok(value);
            }
            break;
        }
        let mut out = Buffer::with_capacity(self.ctx, self.pos - start)?;
        out.extend(self.ctx, &self.input[start..self.pos])?;
        loop {
            if self.pos >= self.input.len() {
                return self.err("unterminated JSON string");
            }
            let end = self.input.len().min(self.pos + CHUNK);
            let span = scan::text_span(&self.input[self.pos..end], Class::JsonParse);
            if span.len > 0 {
                if span.runes != span.len {
                    self.ctx.charge(span.steps)?;
                }
                out.extend(self.ctx, &self.input[self.pos..self.pos + span.len])?;
                self.pos += span.len;
                continue;
            }
            self.ctx.charge(1)?;
            let b = self.input[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Value::from_bytes(self.ctx, out),
                b'\\' => {
                    let Some(&b) = self.input.get(self.pos) else {
                        return self.err("incomplete JSON escape");
                    };
                    self.pos += 1;
                    match b {
                        b'"' | b'\\' | b'/' => out.push(self.ctx, b)?,
                        b'b' => out.push(self.ctx, 8)?,
                        b'f' => out.push(self.ctx, 12)?,
                        b'n' => out.push(self.ctx, b'\n')?,
                        b'r' => out.push(self.ctx, b'\r')?,
                        b't' => out.push(self.ctx, b'\t')?,
                        b'u' => {
                            let high = self.hex()?;
                            let cp = if (0xd800..=0xdbff).contains(&high) {
                                if self.input[self.pos..].starts_with(b"\\u") {
                                    let saved = self.pos;
                                    self.pos += 2;
                                    let low = self.hex()?;
                                    if (0xdc00..=0xdfff).contains(&low) {
                                        0x10000
                                            + ((high as u32 - 0xd800) << 10)
                                            + (low as u32 - 0xdc00)
                                    } else {
                                        self.pos = saved;
                                        0xfffd
                                    }
                                } else {
                                    0xfffd
                                }
                            } else if (0xdc00..=0xdfff).contains(&high) {
                                0xfffd
                            } else {
                                high as u32
                            };
                            let ch = char::from_u32(cp).unwrap();
                            let mut buf = [0; 4];
                            out.extend(self.ctx, ch.encode_utf8(&mut buf).as_bytes())?;
                        }
                        _ => return self.err("invalid JSON escape"),
                    }
                }
                0..=31 => return self.err("unescaped control byte in JSON string"),
                _ => {
                    self.pos -= 1;
                    let (ch, n, _) = scan::rune(&self.input[self.pos..]);
                    self.pos += n;
                    let mut buf = [0; 4];
                    out.extend(self.ctx, ch.encode_utf8(&mut buf).as_bytes())?;
                }
            }
        }
    }
    fn hex(&mut self) -> Result<u16> {
        let mut n = 0;
        for _ in 0..4 {
            let Some(&b) = self.input.get(self.pos) else {
                return self.err("incomplete Unicode escape");
            };
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => return self.err("invalid Unicode escape"),
            };
            n = (n << 4) | digit as u16;
            self.pos += 1;
        }
        Ok(n)
    }
    fn number(&mut self) -> Result<Value> {
        let start = self.pos;
        self.take(b'-');
        if !self.take(b'0') {
            let digits = self.pos;
            self.digits()?;
            if digits == self.pos {
                return self.err("invalid JSON number");
            }
        }
        let mut float = false;
        if self.take(b'.') {
            float = true;
            let digits = self.pos;
            self.digits()?;
            if digits == self.pos {
                return self.err("invalid JSON fraction");
            }
        }
        if self.take(b'e') || self.take(b'E') {
            float = true;
            if !self.take(b'+') {
                self.take(b'-');
            }
            let digits = self.pos;
            self.digits()?;
            if digits == self.pos {
                return self.err("invalid JSON exponent");
            }
        }
        let text = std::str::from_utf8(&self.input[start..self.pos]).unwrap();
        self.ctx.work_bytes(text.len())?;
        if float {
            let n = parse_float(self.ctx, text.as_bytes())?;
            if !n.is_finite() {
                return self.err("JSON number outside finite f64 range");
            }
            Ok(Value::float(n))
        } else {
            if let Ok(n) = text.parse::<i64>() {
                Ok(Value::int(n))
            } else {
                crate::integer::parse_digits(self.ctx, text.as_bytes(), 10)
            }
        }
    }
    fn digits(&mut self) -> Result<()> {
        let start = self.pos;
        while self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
            if (self.pos - start) % 64 == 0 {
                self.ctx.charge(1)?;
            }
            self.pos += 1;
        }
        Ok(())
    }
}

pub(crate) fn parse_float(ctx: &mut CallContext, input: &[u8]) -> Result<f64> {
    const DIGITS: usize = 1100;
    if input.len() <= DIGITS {
        return std::str::from_utf8(input)
            .unwrap()
            .parse()
            .map_err(|_| Error::new(ErrorKind::Json, "invalid JSON number"));
    }
    // Binary64 rounding boundaries need at most 768 significant decimal digits.
    // Keep extra digits and a nonzero tail marker, bounding the library conversion.
    let mut text = [0u8; DIGITS + 80];
    let negative = input[0] == b'-';
    let mut used = usize::from(negative);
    text[0] = b'-';
    let mut at = used;
    let mut significant = 0usize;
    let mut kept = 0usize;
    let mut fractional = false;
    let mut fraction_digits = 0usize;
    let mut tail = false;
    while at < input.len() && !matches!(input[at], b'e' | b'E') {
        if (at - usize::from(negative)) % 1024 == 0 {
            ctx.work_bytes((input.len() - at).min(1024))?;
        }
        let byte = input[at];
        at += 1;
        if byte == b'.' {
            fractional = true;
            continue;
        }
        fraction_digits += usize::from(fractional);
        if significant != 0 || byte != b'0' {
            significant += 1;
            if kept < DIGITS {
                text[used] = byte;
                used += 1;
                kept += 1;
            } else {
                tail |= byte != b'0';
            }
        }
    }
    let mut exponent = 0i128;
    if at < input.len() {
        at += 1;
        let exponent_negative = input.get(at) == Some(&b'-');
        if matches!(input.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        while at < input.len() {
            if at % 1024 == 0 {
                ctx.work_bytes((input.len() - at).min(1024))?;
            }
            exponent = exponent
                .saturating_mul(10)
                .saturating_add((input[at] - b'0') as i128);
            at += 1;
        }
        if exponent_negative {
            exponent = -exponent;
        }
    }
    if significant == 0 {
        return Ok(if negative { -0.0 } else { 0.0 });
    }
    exponent = exponent
        .saturating_sub(fraction_digits as i128)
        .saturating_add((significant - kept) as i128);
    if tail {
        text[used] = b'1';
        used += 1;
        exponent = exponent.saturating_sub(1);
    }
    let mut suffix = Number::new();
    write!(suffix, "e{exponent}").unwrap();
    text[used..used + suffix.bytes().len()].copy_from_slice(suffix.bytes());
    used += suffix.bytes().len();
    std::str::from_utf8(&text[..used])
        .unwrap()
        .parse()
        .map_err(|_| Error::new(ErrorKind::Json, "invalid JSON number"))
}

pub(crate) fn bytes_equal(ctx: &mut CallContext, a: &[u8], b: &[u8]) -> Result<bool> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (a, b) in a.chunks(CHUNK).zip(b.chunks(CHUNK)) {
        ctx.work_bytes(a.len())?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn stringify(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut out = Buffer::empty();
    write_value(ctx, value, &mut out, 0)?;
    Value::from_bytes(ctx, out)
}
fn write_value(
    ctx: &mut CallContext,
    value: &Value,
    out: &mut Buffer<u8>,
    depth: usize,
) -> Result<()> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH {
        return ctx.fail(ErrorKind::Recursion, "JSON nesting too deep");
    }
    match &value.0 {
        Kind::Builtin(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a builtin")),
        Kind::Money(_) => return Err(Error::new(ErrorKind::Json, "cannot encode money")),
        Kind::Duration(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a duration")),
        Kind::Range(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a range")),
        Kind::Nil => out.extend(ctx, b"null")?,
        Kind::Bool(v) => out.extend(ctx, if *v { b"true" } else { b"false" })?,
        Kind::Int(n) => {
            let mut text = Number::new();
            write!(text, "{n}").unwrap();
            out.extend(ctx, text.bytes())?;
        }
        Kind::Big(_) => {
            let text = crate::integer::format(ctx, value, 10)?;
            out.extend(ctx, &text.data)?;
        }
        Kind::Float(n) => {
            if !n.is_finite() {
                return Err(Error::new(
                    ErrorKind::Json,
                    "cannot encode a non-finite float",
                ));
            }
            let mut text = Number::new();
            if *n != 0.0 && !(1e-6..1e21).contains(&n.abs()) {
                let mut scientific = Number::new();
                write!(scientific, "{n:e}").unwrap();
                let scientific = std::str::from_utf8(scientific.bytes()).unwrap();
                let (mantissa, exponent) = scientific.split_once('e').unwrap();
                let exponent: i32 = exponent.parse().unwrap();
                write!(text, "{mantissa}e{exponent:+}").unwrap();
            } else {
                write!(text, "{n}").unwrap();
            }
            out.extend(ctx, text.bytes())?;
        }
        Kind::Bytes(h) | Kind::Symbol(h) => write_string(ctx, &h.data, out)?,
        Kind::Array(h) => {
            out.push(ctx, b'[')?;
            for (i, v) in h.buffer.data.iter().enumerate() {
                if i > 0 {
                    out.push(ctx, b',')?;
                }
                write_value(ctx, v, out, depth + 1)?;
            }
            out.push(ctx, b']')?;
        }
        Kind::Hash(h) => {
            out.push(ctx, b'{')?;
            for (i, (k, v)) in h.buffer.data.iter().enumerate() {
                if i > 0 {
                    out.push(ctx, b',')?;
                }
                write_string(ctx, k.require_bytes()?, out)?;
                out.push(ctx, b':')?;
                write_value(ctx, v, out, depth + 1)?;
            }
            out.push(ctx, b'}')?;
        }
    }
    Ok(())
}
fn write_string(ctx: &mut CallContext, input: &[u8], out: &mut Buffer<u8>) -> Result<()> {
    let Some(minimum) = out.data.len().checked_add(input.len()).and_then(|n| {
        n.checked_add(
            2 + if input.len() >= CHUNK {
                MAX_VALUE_DEPTH
            } else {
                0
            },
        )
    }) else {
        return ctx.fail(ErrorKind::Memory, "JSON output size overflow");
    };
    if minimum > out.data.capacity() {
        out.ensure(ctx, minimum.max(out.data.capacity().saturating_mul(2)))?;
    }
    out.push(ctx, b'"')?;
    let mut i = 0;
    while i < input.len() {
        let span = scan::text_span(&input[i..input.len().min(i + CHUNK)], Class::JsonStringify);
        if span.len > 0 {
            if span.runes != span.len {
                ctx.charge(span.steps)?;
            }
            out.extend(ctx, &input[i..i + span.len])?;
            i += span.len;
            continue;
        }
        ctx.charge(1)?;
        let b = input[i];
        i += 1;
        match b {
            b'"' => out.extend(ctx, b"\\\"")?,
            b'\\' => out.extend(ctx, b"\\\\")?,
            b'\n' => out.extend(ctx, b"\\n")?,
            b'\r' => out.extend(ctx, b"\\r")?,
            b'\t' => out.extend(ctx, b"\\t")?,
            8 => out.extend(ctx, b"\\b")?,
            12 => out.extend(ctx, b"\\f")?,
            0..=31 | b'<' | b'>' | b'&' => {
                let hex = b"0123456789abcdef";
                out.extend(
                    ctx,
                    &[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        hex[(b >> 4) as usize],
                        hex[(b & 15) as usize],
                    ],
                )?;
            }
            _ => {
                i -= 1;
                let (ch, n, valid) = scan::rune(&input[i..]);
                i += n;
                if ch == '\u{2028}' {
                    out.extend(ctx, b"\\u2028")?;
                } else if ch == '\u{2029}' {
                    out.extend(ctx, b"\\u2029")?;
                } else if !valid {
                    out.extend(ctx, b"\\ufffd")?;
                } else {
                    out.extend(ctx, &input[i - n..i])?;
                }
            }
        }
    }
    out.push(ctx, b'"')?;
    Ok(())
}

pub(crate) struct Number {
    buf: [u8; 512],
    len: usize,
}
impl Number {
    pub fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}
impl Write for Number {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}
