use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    hash::Hash,
    scan::{self, Class},
};

/// A partially parsed container awaiting further elements.
///
/// Hash frames hold the key of the element currently being parsed; a
/// placeholder nil sits there between a completed element and the next key.
enum Frame {
    Array(Buffer<Value>),
    Hash { out: Hash, key: Value },
}

pub(super) struct Parser<'a> {
    ctx: &'a mut CallContext,
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    pub fn new(ctx: &'a mut CallContext, input: &'a [u8]) -> Self {
        Self { ctx, input, pos: 0 }
    }

    pub fn finished(&self) -> bool {
        self.pos == self.input.len()
    }

    pub fn err<T>(&self, msg: &str) -> Result<T> {
        Err(Error::new(
            ErrorKind::Json,
            format!("{msg} at byte {}", self.pos),
        ))
    }

    pub fn space(&mut self) -> Result<()> {
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

    /// Parses one complete value using an explicit, budget-accounted frame stack.
    ///
    /// Each open array or object is one frame, so nesting is bounded by the
    /// frame count rather than the native stack. Any error drops the frames,
    /// releasing every partially built container and completed sibling.
    pub fn value(&mut self) -> Result<Value> {
        let mut frames: Buffer<Frame> = Buffer::empty();
        loop {
            let Some(mut value) = self.start(&mut frames)? else {
                continue;
            };
            // Deliver the finished value to the innermost open container, then
            // keep closing containers while their terminators follow.
            loop {
                let Some(frame) = frames.data.last_mut() else {
                    return Ok(value);
                };
                match frame {
                    Frame::Array(out) => {
                        out.push(self.ctx, value)?;
                        self.space()?;
                        if self.take(b']') {
                            let Some(Frame::Array(out)) = frames.data.pop() else {
                                return self.err("expected closing bracket");
                            };
                            value = Value::from_array(self.ctx, out)?;
                            continue;
                        }
                        if !self.take(b',') {
                            return self.err("expected comma or closing bracket");
                        }
                        break;
                    }
                    Frame::Hash { out, key } => {
                        out.insert(self.ctx, std::mem::take(key), value)?;
                        self.space()?;
                        if self.take(b'}') {
                            let Some(Frame::Hash { out, .. }) = frames.data.pop() else {
                                return self.err("expected closing brace");
                            };
                            value = Value::from_hash(self.ctx, out)?;
                            continue;
                        }
                        if !self.take(b',') {
                            return self.err("expected comma or closing brace");
                        }
                        *key = self.key()?;
                        break;
                    }
                }
            }
        }
    }

    /// Consumes the start of a value. Scalars and empty containers complete
    /// immediately; a non-empty container pushes a frame and returns `None` so
    /// the caller continues with its first element.
    fn start(&mut self, frames: &mut Buffer<Frame>) -> Result<Option<Value>> {
        self.ctx.charge(1)?;
        self.space()?;
        match self.input.get(self.pos).copied() {
            Some(b'"') => self.string().map(Some),
            Some(b'[') => {
                self.enter(frames)?;
                self.pos += 1;
                self.space()?;
                if self.take(b']') {
                    return Value::from_array(self.ctx, Buffer::empty()).map(Some);
                }
                frames.push(self.ctx, Frame::Array(Buffer::empty()))?;
                Ok(None)
            }
            Some(b'{') => {
                self.enter(frames)?;
                self.pos += 1;
                self.space()?;
                if self.take(b'}') {
                    return Value::from_hash(self.ctx, Hash::empty()).map(Some);
                }
                let key = self.key()?;
                frames.push(
                    self.ctx,
                    Frame::Hash {
                        out: Hash::empty(),
                        key,
                    },
                )?;
                Ok(None)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(Some(Value::boolean(true)))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(Some(Value::boolean(false)))
            }
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(Some(Value::nil()))
            }
            Some(b'-' | b'0'..=b'9') => self.number().map(Some),
            _ => self.err("expected JSON value"),
        }
    }

    /// Rejects a container opener once `MAX_VALUE_DEPTH` containers are open.
    fn enter(&mut self, frames: &Buffer<Frame>) -> Result<()> {
        if frames.data.len() >= MAX_VALUE_DEPTH {
            return self
                .ctx
                .guard(ErrorKind::Recursion, "JSON nesting too deep");
        }
        Ok(())
    }

    fn key(&mut self) -> Result<Value> {
        self.space()?;
        if self.input.get(self.pos) != Some(&b'"') {
            return self.err("expected JSON object key");
        }
        let key = self.string()?;
        self.space()?;
        if !self.take(b':') {
            return self.err("expected colon");
        }
        Ok(key)
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
            let n = super::parse_float(self.ctx, text.as_bytes())?;
            if !n.is_finite() {
                return self.err("JSON number outside finite f64 range");
            }
            Ok(Value::float(n))
        } else if let Ok(n) = text.parse::<i64>() {
            Ok(Value::int(n))
        } else {
            crate::integer::parse_digits(self.ctx, text.as_bytes(), 10)
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
