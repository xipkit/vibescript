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

/// Why parsing stopped, in the terms of the reference parser, so script-facing
/// builtins can report its wording. Bytes are the offending input byte and
/// spans are byte ranges of the offending number literal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    End,
    Value(u8),
    AfterElement(u8),
    KeyStart(u8),
    AfterKey(u8),
    AfterValue(u8),
    Literal(u8),
    Escape(u8),
    Unicode(u8),
    Number(usize, usize),
    /// A well-formed number outside the finite float range; the reference
    /// reports it without the invalid-JSON framing.
    Range(usize, usize),
    Trailing,
    Depth,
}

impl Failure {
    /// Renders the reference message for `name` (`JSON.parse`, `JSON.parse_as`).
    pub fn render(
        self,
        name: &str,
        input: &[u8],
        out: &mut impl std::fmt::Write,
    ) -> std::fmt::Result {
        let number = |out: &mut dyn std::fmt::Write, start: usize, end: usize| {
            let mut quoted = Vec::new();
            crate::shapes::quote(&input[start..end], &mut quoted);
            out.write_str(std::str::from_utf8(&quoted).unwrap())
        };
        if let Self::Range(start, end) = self {
            write!(out, "{name} invalid number ")?;
            return number(out, start, end);
        }
        write!(out, "{name} invalid JSON: ")?;
        let (byte, context) = match self {
            Self::End => return out.write_str("unexpected end of JSON input"),
            Self::Trailing => return out.write_str("trailing data"),
            Self::Depth => return out.write_str("exceeded max depth"),
            Self::Number(start, end) => {
                out.write_str("invalid number ")?;
                return number(out, start, end);
            }
            Self::Range(..) => unreachable!(),
            Self::Value(byte) => (byte, "looking for beginning of value"),
            Self::AfterElement(byte) => (byte, "after array element"),
            Self::KeyStart(byte) => (byte, "looking for beginning of object key string"),
            Self::AfterKey(byte) => (byte, "after object key"),
            Self::AfterValue(byte) => (byte, "after object value"),
            Self::Literal(byte) => (byte, "in string literal"),
            Self::Escape(byte) => (byte, "in string escape code"),
            Self::Unicode(byte) => (byte, "in unicode escape"),
        };
        out.write_str("invalid character ")?;
        quote_byte(byte, out)?;
        write!(out, " {context}")
    }
}

/// Quotes one input byte as Go's `%q` renders a byte: a rune literal.
fn quote_byte(byte: u8, out: &mut impl std::fmt::Write) -> std::fmt::Result {
    let rune = char::from(byte);
    out.write_char('\'')?;
    match rune {
        '\'' | '\\' => write!(out, "\\{rune}")?,
        '\u{7}' => out.write_str("\\a")?,
        '\u{8}' => out.write_str("\\b")?,
        '\u{c}' => out.write_str("\\f")?,
        '\n' => out.write_str("\\n")?,
        '\r' => out.write_str("\\r")?,
        '\t' => out.write_str("\\t")?,
        '\u{b}' => out.write_str("\\v")?,
        _ if crate::printable::is_print(rune) => out.write_char(rune)?,
        _ if byte < 0x20 || byte == 0x7f => write!(out, "\\x{byte:02x}")?,
        _ => write!(out, "\\u{byte:04x}")?,
    }
    out.write_char('\'')
}

pub(super) struct Parser<'a> {
    ctx: &'a mut CallContext,
    input: &'a [u8],
    pos: usize,
    /// The reference's reason for the most recent syntax failure.
    pub failure: Option<Failure>,
    /// A leading zero followed by a digit. The reference rejects the number
    /// there; the port rejects the digit, which must be the next failure.
    zero: Option<(usize, usize)>,
}

impl<'a> Parser<'a> {
    pub fn new(ctx: &'a mut CallContext, input: &'a [u8]) -> Self {
        Self {
            ctx,
            input,
            pos: 0,
            failure: None,
            zero: None,
        }
    }

    pub fn finished(&self) -> bool {
        self.pos == self.input.len()
    }

    /// Fails with the host message `msg` and records the reference's reason.
    pub fn err<T>(&mut self, msg: &str, failure: Failure) -> Result<T> {
        self.failure = Some(match self.zero.take() {
            Some((start, end)) => Failure::Number(start, end),
            None => failure,
        });
        Err(Error::new(
            ErrorKind::Json,
            format!("{msg} at byte {}", self.pos),
        ))
    }

    /// The failure for the byte at the current position, or the end of input.
    fn found(&self, failure: fn(u8) -> Failure) -> Failure {
        self.input
            .get(self.pos)
            .copied()
            .map_or(Failure::End, failure)
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
                                return self.err("expected closing bracket", Failure::End);
                            };
                            value = Value::from_array(self.ctx, out)?;
                            continue;
                        }
                        if !self.take(b',') {
                            let failure = self.found(Failure::AfterElement);
                            return self.err("expected comma or closing bracket", failure);
                        }
                        break;
                    }
                    Frame::Hash { out, key } => {
                        out.insert(self.ctx, std::mem::take(key), value)?;
                        self.space()?;
                        if self.take(b'}') {
                            let Some(Frame::Hash { out, .. }) = frames.data.pop() else {
                                return self.err("expected closing brace", Failure::End);
                            };
                            value = Value::from_hash(self.ctx, out)?;
                            continue;
                        }
                        if !self.take(b',') {
                            let failure = self.found(Failure::AfterValue);
                            return self.err("expected comma or closing brace", failure);
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
            _ => {
                let failure = self.found(Failure::Value);
                self.err("expected JSON value", failure)
            }
        }
    }

    /// Rejects a container opener once `MAX_VALUE_DEPTH` containers are open.
    fn enter(&mut self, frames: &Buffer<Frame>) -> Result<()> {
        if frames.data.len() >= MAX_VALUE_DEPTH {
            let error = self
                .ctx
                .guard::<()>(ErrorKind::Recursion, "JSON nesting too deep")
                .unwrap_err();
            if error.kind == ErrorKind::Recursion {
                self.failure = Some(Failure::Depth);
            }
            return Err(error);
        }
        Ok(())
    }

    fn key(&mut self) -> Result<Value> {
        self.space()?;
        if self.input.get(self.pos) != Some(&b'"') {
            let failure = self.found(Failure::KeyStart);
            return self.err("expected JSON object key", failure);
        }
        let key = self.string()?;
        self.space()?;
        if !self.take(b':') {
            let failure = self.found(Failure::AfterKey);
            return self.err("expected colon", failure);
        }
        Ok(key)
    }

    fn literal(&mut self, literal: &[u8]) -> Result<()> {
        if self.input[self.pos..].starts_with(literal) {
            self.pos += literal.len();
            Ok(())
        } else {
            let failure = self.found(Failure::Value);
            self.err("invalid JSON literal", failure)
        }
    }

    fn string(&mut self) -> Result<Value> {
        self.pos += 1;
        let start = self.pos;
        loop {
            if self.pos >= self.input.len() {
                return self.err("unterminated JSON string", Failure::End);
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
                return self.err("unterminated JSON string", Failure::End);
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
                        return self.err("incomplete JSON escape", Failure::End);
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
                        _ => return self.err("invalid JSON escape", Failure::Escape(b)),
                    }
                }
                0..=31 => {
                    return self.err("unescaped control byte in JSON string", Failure::Literal(b));
                }
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
        // The reference rejects a short escape as the end of input before it
        // reads any digit.
        let short = self.input.len() - self.pos < 4;
        let mut n = 0;
        for _ in 0..4 {
            let Some(&b) = self.input.get(self.pos) else {
                return self.err("incomplete Unicode escape", Failure::End);
            };
            let digit = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => {
                    let failure = if short {
                        Failure::End
                    } else {
                        Failure::Unicode(b)
                    };
                    return self.err("invalid Unicode escape", failure);
                }
            };
            n = (n << 4) | digit as u16;
            self.pos += 1;
        }
        Ok(n)
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.pos;
        self.take(b'-');
        if self.take(b'0') {
            if self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.zero = Some((start, self.pos + 1));
            }
        } else {
            let digits = self.pos;
            self.digits()?;
            if digits == self.pos {
                return self.err("invalid JSON number", Failure::Number(start, self.pos));
            }
        }
        let mut float = false;
        if self.take(b'.') {
            float = true;
            let digits = self.pos;
            self.digits()?;
            if digits == self.pos {
                return self.err("invalid JSON fraction", Failure::Number(start, self.pos));
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
                return self.err("invalid JSON exponent", Failure::Number(start, self.pos));
            }
        }
        let text = std::str::from_utf8(&self.input[start..self.pos]).unwrap();
        self.ctx.work_bytes(text.len())?;
        if float {
            let n = super::parse_float(self.ctx, text.as_bytes())?;
            if !n.is_finite() {
                return self.err(
                    "JSON number outside finite f64 range",
                    Failure::Range(start, self.pos),
                );
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
