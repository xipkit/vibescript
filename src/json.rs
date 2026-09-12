use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
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
                let mut out = Buffer::empty();
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
                        insert(self.ctx, &mut out, key, value)?;
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
        let mut out = Buffer::empty();
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
            let n: f64 = text
                .parse()
                .map_err(|_| Error::new(ErrorKind::Json, "invalid JSON number"))?;
            if !n.is_finite() {
                return self.err("JSON number outside finite f64 range");
            }
            Ok(Value::float(n))
        } else {
            Ok(Value::int(text.parse().map_err(|_| {
                Error::new(
                    ErrorKind::Json,
                    "JSON integer exceeds this core's i64 range",
                )
            })?))
        }
    }
    fn digits(&mut self) -> Result<()> {
        let start = self.pos;
        while self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
            if self.pos - start > 1024 {
                return self.err("JSON number exceeds 1024 digits");
            }
        }
        Ok(())
    }
}

pub(crate) fn insert(
    ctx: &mut CallContext,
    entries: &mut Buffer<(Value, Value)>,
    key: Value,
    value: Value,
) -> Result<()> {
    for (k, v) in &mut entries.data {
        ctx.charge(1)?;
        if bytes_equal(ctx, k.require_bytes()?, key.require_bytes()?)? {
            *v = value;
            return Ok(());
        }
    }
    entries.push(ctx, (key, value))
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
        Kind::Nil => out.extend(ctx, b"null")?,
        Kind::Bool(v) => out.extend(ctx, if *v { b"true" } else { b"false" })?,
        Kind::Int(n) => {
            let mut text = Number::new();
            write!(text, "{n}").unwrap();
            out.extend(ctx, text.bytes())?;
        }
        Kind::Float(n) => {
            if !n.is_finite() {
                return Err(Error::new(
                    ErrorKind::Json,
                    "cannot encode a non-finite float",
                ));
            }
            let mut text = Number::new();
            write!(text, "{n}").unwrap();
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
