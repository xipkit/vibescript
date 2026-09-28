#[cfg(test)]
use crate::CallContext;
use crate::scan::Class;
use crate::{
    Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    hash::Hash,
    scan,
};

/// Small documents use ordinary hashes instead of setting up schema tables.
pub(super) const RECORD_MIN_BYTES: usize = CHUNK / 2;

const RESERVATION_MIN_BYTES: usize = 2 * CHUNK;

/// Selects structural indexing using the document's original prefix scan.
pub(super) fn indexed(input: &[u8]) -> bool {
    input.len() >= 512
        && match input
            .iter()
            .find(|&&b| !matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            Some(b'[') => true,
            Some(b'{') => input[..512].contains(&b'['),
            _ => false,
        }
}

/// Flat and small documents cannot amortize reservations between allocations.
pub(super) fn batched(input: &[u8], indexed: bool) -> bool {
    input.len() >= RESERVATION_MIN_BYTES && indexed
}

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

// Only the low five bits select a cache set. Multiplication by 33 leaves
// those bits unchanged, so the original polynomial reduces to a byte XOR.
#[inline(always)]
fn cache_slot(bytes: &[u8]) -> usize {
    let mut words = bytes.chunks_exact(4);
    let mut hash = words.by_ref().fold(0u32, |hash, word| {
        hash ^ u32::from_ne_bytes(word.try_into().unwrap())
    });
    hash ^= hash >> 16;
    hash ^= hash >> 8;
    words
        .remainder()
        .iter()
        .fold(hash as usize, |hash, &byte| hash ^ usize::from(byte))
        & 31
}

/// Reads an integer literal, `-?[0-9]+`, as `str::parse::<i64>` does, or
/// returns `None` when it does not fit.
fn integer(text: &[u8]) -> Option<i64> {
    let (negative, digits) = match text {
        [b'-', digits @ ..] => (true, digits),
        _ => (false, text),
    };
    if digits.len() <= 18 {
        let mut n = 0i64;
        for &digit in digits {
            n = n * 10 + i64::from(digit - b'0');
        }
        return Some(if negative { -n } else { n });
    }
    digits.iter().try_fold(0i64, |n, &digit| {
        let digit = i64::from(digit - b'0');
        let n = n.checked_mul(10)?;
        if negative {
            n.checked_sub(digit)
        } else {
            n.checked_add(digit)
        }
    })
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

pub(super) struct Parser<'a, 'r, C = super::accounting::Steps<'a>> {
    pub(super) ctx: C,
    input: &'a [u8],
    pos: usize,
    /// The reference's reason for the most recent syntax failure.
    pub failure: Option<Failure>,
    /// A leading zero followed by a digit. The reference rejects the number
    /// there; the port rejects the digit, which must be the next failure.
    zero: Option<(usize, usize)>,
    scanner: super::scan::Scanner,
    // Arrays amortize indexing and key sharing across repeated records.
    indexed: bool,
    keys: Option<[[Value; 2]; 32]>,
    strings: Option<[[Value; 2]; 32]>,
    pub(super) records: Option<&'r mut super::records::Records<'a>>,
    record_depth: usize,
    decoded_start: usize,
    #[cfg(test)]
    pub unbatched: bool,
    pub typed: super::typed::Stream<'a>,
}

#[cfg(test)]
impl<'a, 'r> Parser<'a, 'r> {
    pub fn new(ctx: &'a mut CallContext, input: &'a [u8]) -> Self {
        Self::with_context(super::accounting::Steps::new(ctx, input.len()), input)
    }
}

impl<'a, 'r, C: super::accounting::Context> Parser<'a, 'r, C> {
    #[cfg(test)]
    pub fn portable(&mut self) {
        self.scanner.portable = true;
    }

    #[cfg(test)]
    pub fn with_context(ctx: C, input: &'a [u8]) -> Self {
        Self::with_index(ctx, input, indexed(input))
    }

    /// Constructs the parser with its original prefix decision already made.
    pub fn with_index(ctx: C, input: &'a [u8], indexed: bool) -> Self {
        Self {
            ctx,
            input,
            pos: 0,
            failure: None,
            zero: None,
            scanner: super::scan::Scanner::default(),
            indexed,
            keys: None,
            strings: None,
            typed: super::typed::Stream::default(),
            records: None,
            record_depth: 0,
            decoded_start: usize::MAX,
            #[cfg(test)]
            unbatched: false,
        }
    }

    pub fn finished(&self) -> bool {
        self.pos == self.input.len()
    }

    /// Releases scalar cache entries after replacement or a failed document.
    pub fn clear_strings(&mut self) {
        self.strings = None;
    }

    /// Fails with the host message `msg` and records the reference's reason.
    #[cold]
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
        self.space_with::<true>()
    }

    fn space_with<const INDEX: bool>(&mut self) -> Result<()> {
        while self
            .input
            .get(self.pos)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            let start = self.pos;
            let end = self.input.len().min(start + CHUNK);
            self.pos += if INDEX && self.indexed {
                self.scanner.space(self.input, self.pos, end)
            } else {
                self.input[self.pos..end]
                    .iter()
                    .take_while(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
                    .count()
            };
            self.ctx.work_bytes(self.pos - start)?;
        }
        Ok(())
    }

    fn take<const INDEX: bool>(&mut self, b: u8) -> bool {
        if self.input.get(self.pos) == Some(&b) {
            if INDEX
                && self.indexed
                && matches!(b, b']' | b'}' | b',' | b':')
                && !self.scanner.punctuation(self.input, self.pos)
            {
                return false;
            }
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
        if self.indexed {
            self.value_indexed::<true>()
        } else {
            self.value_indexed::<false>()
        }
    }

    /// Parses with the prefix decision known by the caller.
    pub fn value_indexed<const INDEX: bool>(&mut self) -> Result<Value> {
        debug_assert_eq!(INDEX, self.indexed);
        #[cfg(test)]
        {
            self.ctx.set_unbatched(self.unbatched);
        }
        // Small messages keep their streaming proof and ordinary hash growth.
        match (
            self.typed.ty.is_some(),
            self.input.len() >= RECORD_MIN_BYTES,
        ) {
            (false, _) => self.value_with::<INDEX, false, false>(),
            (true, false) => self.value_with::<INDEX, true, false>(),
            (true, true) => self.value_with::<INDEX, true, true>(),
        }
    }

    fn value_with<const INDEX: bool, const TYPED: bool, const RECORDS: bool>(
        &mut self,
    ) -> Result<Value> {
        debug_assert!(!RECORDS || self.records.is_some());
        let mut frames: Buffer<Frame> = Buffer::empty();
        loop {
            let Some(mut value) = self.start::<INDEX, TYPED, RECORDS>(&mut frames)? else {
                continue;
            };
            // Deliver the finished value to the innermost open container, then
            // keep closing containers while their terminators follow.
            loop {
                if TYPED {
                    self.typed.complete(frames.data.len(), &value);
                }
                let open = frames.data.len();
                let Some(frame) = frames.data.last_mut() else {
                    return Ok(value);
                };
                match frame {
                    Frame::Array(out) => {
                        out.push(&mut self.ctx, value)?;
                        self.space_with::<INDEX>()?;
                        if self.take::<INDEX>(b']') {
                            let Some(Frame::Array(out)) = frames.data.pop() else {
                                return self.err("expected closing bracket", Failure::End);
                            };
                            value = Value::from_array(&mut self.ctx, out)?;
                            continue;
                        }
                        if !self.take::<INDEX>(b',') {
                            let failure = self.found(Failure::AfterElement);
                            return self.err("expected comma or closing bracket", failure);
                        }
                        break;
                    }
                    Frame::Hash { out, key } => {
                        let capacity = if RECORDS && out.buffer.data.is_empty() {
                            self.records.as_ref().unwrap().capacity(open - 1)
                        } else {
                            None
                        };
                        if let Some(capacity) = capacity.filter(|&n| n > 0) {
                            // The first insertion has no comparisons or index. Reserve
                            // at its usual allocation point, after its logical step.
                            self.ctx.charge(1)?;
                            let depth = value.depth() + 1;
                            if depth > MAX_VALUE_DEPTH {
                                return self
                                    .ctx
                                    .guard(ErrorKind::Recursion, "value nesting too deep");
                            }
                            out.buffer.ensure(&mut self.ctx, capacity)?;
                            out.buffer.data.push((std::mem::take(key), value));
                            out.depth = depth;
                        } else {
                            let previous = out.buffer.data.len();
                            out.insert(&mut self.ctx, std::mem::take(key), value)?;
                            if (INDEX || RECORDS) && out.buffer.data.len() == previous {
                                // A duplicate can release a whole subtree. Drop
                                // cached scalar references before the next charge
                                // so sharing cannot retain discarded values.
                                self.clear_strings();
                                if RECORDS {
                                    self.records
                                        .as_mut()
                                        .unwrap()
                                        .discard_unused(self.keys.as_ref());
                                }
                            }
                        }
                        self.space_with::<INDEX>()?;
                        if self.take::<INDEX>(b'}') {
                            let Some(Frame::Hash { out, .. }) = frames.data.pop() else {
                                return self.err("expected closing brace", Failure::End);
                            };
                            value = Value::from_hash(&mut self.ctx, out)?;
                            continue;
                        }
                        if !self.take::<INDEX>(b',') {
                            let failure = self.found(Failure::AfterValue);
                            return self.err("expected comma or closing brace", failure);
                        }
                        if RECORDS {
                            self.record_depth = open - 1;
                        }
                        *key = self.key::<INDEX, RECORDS>()?;
                        break;
                    }
                }
            }
        }
    }

    /// Consumes the start of a value. Scalars and empty containers complete
    /// immediately; a non-empty container pushes a frame and returns `None` so
    /// the caller continues with its first element.
    fn start<const INDEX: bool, const TYPED: bool, const RECORDS: bool>(
        &mut self,
        frames: &mut Buffer<Frame>,
    ) -> Result<Option<Value>> {
        self.ctx.charge(1)?;
        self.space_with::<INDEX>()?;
        if TYPED {
            let key = match frames.data.last() {
                Some(Frame::Hash { key, .. }) => key.as_bytes(),
                _ => None,
            };
            if RECORDS && matches!(self.input.get(self.pos), Some(b'[' | b'{')) {
                let index = match frames.data.last() {
                    Some(Frame::Array(out)) => out.data.len(),
                    _ => 0,
                };
                self.records
                    .as_mut()
                    .unwrap()
                    .start(self.typed.ty, frames.data.len(), key, index);
            }
            self.typed.start(
                frames.data.len(),
                key,
                self.input.get(self.pos) == Some(&b'['),
            );
        }
        match self.input.get(self.pos).copied() {
            Some(b'"') => self.string::<INDEX>().map(Some),
            Some(b'[') => {
                if INDEX && !self.indexed {
                    self.indexed = true;
                    self.scanner.end_string(self.pos);
                }
                self.enter(frames)?;
                self.pos += 1;
                self.space_with::<INDEX>()?;
                if self.take::<INDEX>(b']') {
                    return Value::from_array(&mut self.ctx, Buffer::empty()).map(Some);
                }
                frames.push(&mut self.ctx, Frame::Array(Buffer::empty()))?;
                Ok(None)
            }
            Some(b'{') => {
                self.enter(frames)?;
                self.pos += 1;
                self.space_with::<INDEX>()?;
                if self.take::<INDEX>(b'}') {
                    return Value::from_hash(&mut self.ctx, Hash::empty()).map(Some);
                }
                if RECORDS {
                    self.record_depth = frames.data.len();
                }
                let key = self.key::<INDEX, RECORDS>()?;
                frames.push(
                    &mut self.ctx,
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
            Some(b'-' | b'0'..=b'9') => self.number::<INDEX>().map(Some),
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

    #[inline(always)]
    fn key<const INDEX: bool, const RECORDS: bool>(&mut self) -> Result<Value> {
        self.space_with::<INDEX>()?;
        if self.input.get(self.pos) != Some(&b'"') {
            let failure = self.found(Failure::KeyStart);
            return self.err("expected JSON object key", failure);
        }
        let start = self.pos + 1;
        let key = self.read_string::<INDEX, true, RECORDS>()?;
        let key = if RECORDS && self.decoded_start == start {
            self.share_decoded_key(key)?
        } else {
            key
        };
        self.space_with::<INDEX>()?;
        if !self.take::<INDEX>(b':') {
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

    fn string<const INDEX: bool>(&mut self) -> Result<Value> {
        self.read_string::<INDEX, false, false>()
    }

    fn read_string<const INDEX: bool, const KEY: bool, const RECORDS: bool>(
        &mut self,
    ) -> Result<Value> {
        self.pos += 1;
        let start = self.pos;
        if !INDEX {
            // Most flat-object keys finish before a span scanner pays off.
            for (len, &byte) in self.input[start..].iter().take(8).enumerate() {
                if byte == b'"' {
                    if len != 0 {
                        self.ctx.charge(1)?;
                    }
                    self.pos += len;
                    let value = self.copy_string_with::<INDEX, KEY, RECORDS>(start)?;
                    self.pos += 1;
                    return Ok(value);
                }
                if !scan::ordinary(byte, Class::JsonParse) {
                    break;
                }
            }
        }
        loop {
            if self.pos >= self.input.len() {
                return self.err("unterminated JSON string", Failure::End);
            }
            let end = self.input.len().min(self.pos + CHUNK);
            let span = if matches!(self.input[self.pos], 0..=31 | b'"' | b'\\') {
                scan::TextSpan::default()
            } else if INDEX && self.indexed {
                self.span(end)
            } else {
                scan::text_span(&self.input[self.pos..end], Class::JsonParse)
            };
            if span.len > 0 {
                self.ctx.charge(span.steps)?;
                self.pos += span.len;
                continue;
            }
            if self.input[self.pos] == b'"' {
                let value = self.copy_string_with::<INDEX, KEY, RECORDS>(start)?;
                self.pos += 1;
                if INDEX && self.indexed {
                    self.scanner.end_string(self.pos);
                }
                return Ok(value);
            }
            break;
        }
        self.decode_string::<INDEX>(start)
    }

    #[inline(never)]
    fn decode_string<const INDEX: bool>(&mut self, start: usize) -> Result<Value> {
        self.decoded_start = start;
        let mut out = Buffer::with_capacity(&mut self.ctx, self.pos - start)?;
        out.extend(self.ctx.settled(), &self.input[start..self.pos])?;
        // Steps are charged as when each span was appended with `extend` and
        // each escape charged on its own, but settled in batches: before the
        // buffer grows or anything fails, and after each chunk of input.
        let mut pending = 0;
        let mut settled = self.pos;
        loop {
            if self.pos - settled >= CHUNK {
                self.ctx.charge_pending(&mut pending)?;
                self.ctx.checkpoint()?;
                settled = self.pos;
            }
            if self.pos >= self.input.len() {
                self.ctx.charge_pending(&mut pending)?;
                return self.err("unterminated JSON string", Failure::End);
            }
            let end = self.input.len().min(self.pos + CHUNK);
            let span = if matches!(self.input[self.pos], 0..=31 | b'"' | b'\\') {
                scan::TextSpan::default()
            } else if self.input[self.pos] < 128
                && self
                    .input
                    .get(self.pos + 1)
                    .is_some_and(|next| matches!(next, 0..=31 | b'"' | b'\\'))
            {
                scan::TextSpan {
                    len: 1,
                    runes: 1,
                    steps: 1,
                }
            } else {
                scan::text_span(&self.input[self.pos..end], Class::JsonParse)
            };
            if span.len > 0 {
                if span.runes != span.len {
                    pending += span.steps;
                }
                let bytes = &self.input[self.pos..self.pos + span.len];
                if bytes.len() == 1 && out.data.len() < out.data.capacity() {
                    out.data.push(bytes[0]);
                    pending += 1;
                } else {
                    out.extend_deferred(self.ctx.settled(), bytes, &mut pending)?;
                }
                self.pos += span.len;
                continue;
            }
            pending += 1;
            let b = self.input[self.pos];
            self.pos += 1;
            let escape = match b {
                b'\\' => self.input.get(self.pos).copied(),
                _ => None,
            };
            let byte = match escape {
                Some(b'"' | b'\\' | b'/') => escape,
                Some(b'b') => Some(8),
                Some(b'f') => Some(12),
                Some(b'n') => Some(b'\n'),
                Some(b'r') => Some(b'\r'),
                Some(b't') => Some(b'\t'),
                _ => None,
            };
            if let Some(byte) = byte {
                self.pos += 1;
                if out.data.len() < out.data.capacity() {
                    out.data.push(byte);
                } else {
                    out.push_deferred(self.ctx.settled(), byte, &mut pending)?;
                }
                continue;
            }
            if b >= 128 {
                // A rune the span stopped at: invalid or cut by the chunk end.
                self.pos -= 1;
                let (ch, n, _) = scan::rune(&self.input[self.pos..]);
                self.pos += n;
                let mut buf = [0; 4];
                out.extend_deferred(
                    self.ctx.settled(),
                    ch.encode_utf8(&mut buf).as_bytes(),
                    &mut pending,
                )?;
                continue;
            }
            self.ctx.charge_pending(&mut pending)?;
            match b {
                b'"' => {
                    if INDEX && self.indexed {
                        self.scanner.end_string(self.pos);
                    }
                    return Value::from_bytes(self.ctx.settled(), out);
                }
                b'\\' => {
                    let Some(&b) = self.input.get(self.pos) else {
                        return self.err("incomplete JSON escape", Failure::End);
                    };
                    self.pos += 1;
                    match b {
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
                            out.extend(self.ctx.settled(), ch.encode_utf8(&mut buf).as_bytes())?;
                        }
                        _ => return self.err("invalid JSON escape", Failure::Escape(b)),
                    }
                }
                _ => {
                    return self.err("unescaped control byte in JSON string", Failure::Literal(b));
                }
            }
        }
    }

    // Keep decoded-key adoption outside the escape loop, so scalar strings do
    // not carry key-sharing state through every decoded byte. The source offset
    // distinguishes decoded keys without rescanning their input or resetting a
    // flag for every unescaped key.
    #[inline(never)]
    fn share_decoded_key(&mut self, value: Value) -> Result<Value> {
        let Some(records) = self.records.as_mut() else {
            return Ok(value);
        };
        let name = value.as_bytes().unwrap();
        let Some(slot) = records.slot(self.ctx.settled(), self.record_depth, name)? else {
            return Ok(value);
        };
        let cached = self.keys.as_ref().and_then(|keys| {
            if name.len() > 64 {
                return None;
            }
            let index = cache_slot(name);
            keys[index].iter().find(|key| key.as_bytes() == Some(name))
        });
        if let Some(shared) = records
            .get(slot)
            .or_else(|| records.shared(name, Some(slot.0)))
            .or(cached)
        {
            let shared = shared.clone();
            records.remember(self.record_depth, slot, &shared);
            return Ok(shared);
        }
        records.remember(self.record_depth, slot, &value);
        Ok(value)
    }

    #[inline(always)]
    fn copy_string_with<const INDEX: bool, const KEY: bool, const RECORDS: bool>(
        &mut self,
        start: usize,
    ) -> Result<Value> {
        let bytes = &self.input[start..self.pos];
        if (!INDEX || !self.indexed) && !RECORDS {
            return self.ctx.bytes(bytes);
        }
        // Numbered identifiers rarely repeat and would only churn the value cache.
        let cache = if self.indexed
            && bytes.len() <= 64
            && (KEY
                || (!bytes.last().is_some_and(u8::is_ascii_digit)
                    && !bytes.iter().any(u8::is_ascii_digit)))
        {
            let slot = cache_slot(bytes);
            let cache = if KEY {
                &mut self.keys
            } else {
                &mut self.strings
            };
            let keys =
                &cache.get_or_insert_with(|| [const { [Value::nil(), Value::nil()] }; 32])[slot];
            let hit = if keys[0].as_bytes() == Some(bytes) {
                Some(&keys[0])
            } else if keys[1].as_bytes() == Some(bytes) {
                Some(&keys[1])
            } else {
                None
            };
            if let Some(hit) = hit {
                if !KEY {
                    self.ctx.checkpoint()?;
                }
                self.ctx.work_bytes(bytes.len())?;
                if !KEY {
                    self.ctx.checkpoint()?;
                }
                if let Some(records) = self
                    .records
                    .as_mut()
                    .filter(|records| RECORDS && !records.ready(self.record_depth))
                {
                    if let Some(slot) = records.slot(&mut self.ctx, self.record_depth, bytes)? {
                        records.remember(self.record_depth, slot, hit);
                    }
                }
                return Ok(hit.clone());
            }
            Some(slot)
        } else {
            None
        };
        let slot = if let Some(records) = self.records.as_mut().filter(|_| RECORDS) {
            records.slot(&mut self.ctx, self.record_depth, bytes)?
        } else {
            None
        };
        let shared = self
            .records
            .as_ref()
            .filter(|_| RECORDS)
            .and_then(|records| {
                slot.and_then(|slot| records.get(slot))
                    .or_else(|| records.shared(bytes, slot.map(|slot| slot.0)))
            });
        let value = if let Some(shared) = shared {
            self.ctx.checkpoint()?;
            for chunk in bytes.chunks(CHUNK) {
                self.ctx.work_bytes(chunk.len())?;
            }
            self.ctx.checkpoint()?;
            shared.clone()
        } else {
            self.copy_bytes(bytes)?
        };
        if let Some(slot) = slot {
            self.records
                .as_mut()
                .unwrap()
                .remember(self.record_depth, slot, &value);
        }
        if let Some(slot) = cache {
            let cache = if KEY {
                &mut self.keys
            } else {
                &mut self.strings
            };
            let keys = &mut cache.as_mut().unwrap()[slot];
            keys[1] = std::mem::replace(&mut keys[0], value.clone());
        }
        Ok(value)
    }

    fn copy_bytes(&mut self, bytes: &[u8]) -> Result<Value> {
        if !C::BATCHED || !self.indexed {
            return self.ctx.bytes(bytes);
        }
        #[cfg(test)]
        if self.unbatched {
            return self.ctx.bytes(bytes);
        }
        super::accounting::copy_bytes(&mut self.ctx, bytes)
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

    fn number<const INDEX: bool>(&mut self) -> Result<Value> {
        let start = self.pos;
        self.take::<INDEX>(b'-');
        if self.take::<INDEX>(b'0') {
            if self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.zero = Some((start, self.pos + 1));
            }
        } else {
            let digits = self.pos;
            self.digits::<INDEX>()?;
            if digits == self.pos {
                return self.err("invalid JSON number", Failure::Number(start, self.pos));
            }
        }
        let mut float = false;
        if self.take::<INDEX>(b'.') {
            float = true;
            let digits = self.pos;
            self.digits::<INDEX>()?;
            if digits == self.pos {
                return self.err("invalid JSON fraction", Failure::Number(start, self.pos));
            }
        }
        if self.take::<INDEX>(b'e') || self.take::<INDEX>(b'E') {
            float = true;
            if !self.take::<INDEX>(b'+') {
                self.take::<INDEX>(b'-');
            }
            let digits = self.pos;
            self.digits::<INDEX>()?;
            if digits == self.pos {
                return self.err("invalid JSON exponent", Failure::Number(start, self.pos));
            }
        }
        let text = &self.input[start..self.pos];
        self.ctx.work_bytes(text.len())?;
        if float {
            let n = super::parse_float(&mut self.ctx, text)?;
            if !n.is_finite() {
                return self.err(
                    "JSON number outside finite f64 range",
                    Failure::Range(start, self.pos),
                );
            }
            Ok(Value::float(n))
        } else if let Some(n) = integer(text) {
            Ok(Value::int(n))
        } else {
            crate::integer::parse_digits(&mut self.ctx, text, 10)
        }
    }

    fn digits<const INDEX: bool>(&mut self) -> Result<()> {
        while self.input.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.ctx.charge(1)?;
            let end = self.input.len().min(self.pos + 64);
            self.pos += if !INDEX || !self.indexed {
                self.input[self.pos..end]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count()
            } else {
                self.scanner.digits(self.input, self.pos, end)
            };
        }
        Ok(())
    }

    fn span(&mut self, end: usize) -> scan::TextSpan {
        if !self.indexed {
            return scan::text_span(&self.input[self.pos..end], Class::JsonParse);
        }
        if self.input[self.pos] < 128 && !scan::ordinary(self.input[self.pos], Class::JsonParse) {
            return scan::TextSpan::default();
        }
        let mut span = scan::TextSpan::default();
        while self.pos + span.len < end {
            let at = self.pos + span.len;
            if self.input[at] < 128 {
                let n = self.scanner.string(self.input, at, end);
                span.len += n;
                span.runes += n;
                span.steps += (n as u64).div_ceil(64);
                if n == 0 || self.input.get(self.pos + span.len).is_none_or(|&b| b < 128) {
                    break;
                }
            } else {
                let text = scan::text_span(&self.input[at..end], Class::JsonParse);
                if text.len == 0 {
                    break;
                }
                span.len += text.len;
                span.runes += text.runes;
                span.steps += text.steps;
                self.scanner.skip_string(self.pos + span.len);
            }
        }
        span
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn word_cache_hash_preserves_the_original_sets() {
        let mut state = 1u64;
        for len in 0..=64 {
            for _ in 0..256 {
                let bytes: Vec<u8> = (0..len)
                    .map(|_| {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        (state >> 32) as u8
                    })
                    .collect();
                let original = bytes.iter().fold(0usize, |hash, &byte| {
                    hash.wrapping_mul(33) ^ usize::from(byte)
                }) & 31;
                assert_eq!(cache_slot(&bytes), original);
            }
        }
    }

    #[test]
    fn small_documents_skip_record_tables_at_the_size_boundary() {
        use crate::types::{Field, Type, TypeKind};

        let ty = Type {
            name: "array".into(),
            nullable: false,
            kind: TypeKind::Array(Some(Box::new(Type {
                name: String::new(),
                nullable: false,
                kind: TypeKind::Shape(
                    [b"a", b"b"]
                        .into_iter()
                        .map(|name| Field {
                            name: name.to_vec(),
                            ty: Type::named("int".into()),
                            optional: false,
                        })
                        .collect(),
                    false,
                ),
            }))),
        };
        for length in [RECORD_MIN_BYTES - 1, RECORD_MIN_BYTES] {
            let mut input = br#"[{"b":1,"a":2},{"a":3,"b":4}]"#.to_vec();
            input.resize(length, b' ');
            let mut ctx = CallContext::new(CallOptions::default());
            let (value, _) =
                super::super::parse_typed(&mut ctx, &input, "JSON.parse_as", Some(&ty)).unwrap();
            for row in value.as_array().unwrap() {
                let crate::value::Kind::Hash(hash) = &row.0 else {
                    panic!("ordinary hash")
                };
                assert_eq!(
                    hash.buffer.data.capacity(),
                    if length >= RECORD_MIN_BYTES { 2 } else { 8 }
                );
            }
            let mut ordinary = CallContext::new(CallOptions::default());
            let expected = crate::json::parse(&mut ordinary, &input).unwrap();
            assert_eq!(ctx.stats().steps, ordinary.stats().steps);
            assert_eq!(
                crate::json::stringify(&mut ctx, &value).unwrap().as_bytes(),
                crate::json::stringify(&mut ordinary, &expected)
                    .unwrap()
                    .as_bytes()
            );
        }
    }

    #[test]
    fn string_cache_releases_overwritten_subtrees_before_further_allocation() {
        let input = format!(
            r#"[{{"a":{{"k":"discarded"}},"a":{{"k":"replacement"}},"tail":"{}"}}]"#,
            "x".repeat(600)
        );
        let run = |indexed| {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut parser = Parser::new(&mut ctx, input.as_bytes());
            let value = if indexed {
                parser.value_with::<true, false, false>()
            } else {
                parser.value_with::<false, false, false>()
            }
            .unwrap();
            drop(parser);
            let stats = ctx.stats();
            let mut encoder = CallContext::new(CallOptions::default());
            let encoded = crate::json::stringify(&mut encoder, &value)
                .unwrap()
                .as_bytes()
                .unwrap()
                .to_vec();
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            (
                encoded,
                stats.steps,
                stats.peak_memory_bytes,
                stats.retained_memory_bytes,
            )
        };
        let baseline = run(false);
        let shared = run(true);
        assert_eq!((&shared.0, shared.1), (&baseline.0, baseline.1));
        assert!(shared.2 <= baseline.2);
        assert!(shared.3 <= baseline.3);
    }

    #[test]
    fn every_document_quota_boundary_matches_unbatched_accounting() {
        fn read<C: super::super::accounting::Context>(
            ctx: C,
            input: &[u8],
            ty: Option<&crate::types::Type>,
        ) -> (Result<Value>, usize, Option<Failure>) {
            let mut records;
            let mut parser = Parser::with_context(ctx, input);
            if ty.is_some() && input.len() >= RECORD_MIN_BYTES {
                records = super::super::records::Records::default();
                parser.records = Some(&mut records);
            }
            parser.typed.ty = ty;
            let result = super::super::document(&mut parser);
            (result, parser.pos, parser.failure)
        }

        for input in [
            br#"["a",{"b":"cd","b":3},false]"#.as_slice(),
            br#"["a",{"b":"cd"},?]"#,
            br#"["", "ab\ncd", 123.5, "tail"] trailing"#,
        ] {
            for padding in [0, 512, 2048, RESERVATION_MIN_BYTES] {
                let padded = if padding == RESERVATION_MIN_BYTES {
                    [input.to_vec(), vec![b' '; padding]].concat()
                } else {
                    [vec![b' '; padding], input.to_vec()].concat()
                };
                let input = padded.as_slice();
                let ty = crate::types::Type {
                    name: "array".into(),
                    nullable: false,
                    kind: crate::types::TypeKind::Array(Some(Box::new(crate::types::Type {
                        name: String::new(),
                        nullable: false,
                        kind: crate::types::TypeKind::Shape(
                            vec![crate::types::Field {
                                name: b"b".to_vec(),
                                ty: crate::types::Type::named("any".into()),
                                optional: false,
                            }],
                            false,
                        ),
                    }))),
                };
                for typed in [false, true] {
                    let run = |unbatched, steps, memory| {
                        let mut ctx = CallContext::new(CallOptions {
                            limits: Limits {
                                steps,
                                memory_bytes: memory,
                                ..Limits::default()
                            },
                            ..CallOptions::default()
                        });
                        let (result, position, failure) = if unbatched {
                            read(&mut ctx, input, typed.then_some(&ty))
                        } else {
                            read(
                                super::super::accounting::Steps::new(&mut ctx, input.len()),
                                input,
                                typed.then_some(&ty),
                            )
                        };
                        let stats = ctx.stats();
                        let outcome = result.map(|value| {
                            let mut encoder = CallContext::new(CallOptions::default());
                            crate::json::stringify(&mut encoder, &value)
                                .unwrap()
                                .as_bytes()
                                .unwrap()
                                .to_vec()
                        });
                        assert_eq!(ctx.stats().retained_memory_bytes, 0);
                        (
                            outcome,
                            position,
                            failure,
                            stats.steps,
                            stats.peak_memory_bytes,
                            stats.retained_memory_bytes,
                            ctx.checkpoint(),
                        )
                    };
                    let baseline = run(true, None, None);
                    assert_eq!(run(false, None, None), baseline);
                    for steps in 0..=baseline.3 + 1 {
                        for memory in 0..=baseline.4 + 1 {
                            assert_eq!(
                                run(false, Some(steps), Some(memory)),
                                run(true, Some(steps), Some(memory)),
                                "{input:?}, steps {steps}, memory {memory}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn object_prefix_sampling_preserves_results_errors_and_steps() {
        for padding in [0, 480, 500, 511, 512, 513, 1024] {
            for tail in [r#"[{"a":1,"a":2},{"a":3}]}"#, r#"[{"a":1},"\x"]}"#] {
                let input = format!(r#"{{"padding":"{}","rows":{tail}"#, "a".repeat(padding));
                for limit in [None, Some(0), Some(1), Some(10), Some(50), Some(100)] {
                    let run = |indexed| {
                        let mut ctx = CallContext::new(CallOptions {
                            limits: Limits {
                                steps: limit,
                                ..Limits::default()
                            },
                            ..CallOptions::default()
                        });
                        let mut parser = Parser::new(&mut ctx, input.as_bytes());
                        let result = match indexed {
                            Some(true) => parser.value_with::<true, false, false>(),
                            Some(false) => parser.value_with::<false, false, false>(),
                            None => parser.value(),
                        };
                        let failure = parser.failure;
                        drop(parser);
                        let steps = ctx.stats().steps;
                        let result = result
                            .map(|value| {
                                let mut out = CallContext::new(CallOptions::default());
                                crate::json::stringify(&mut out, &value)
                                    .unwrap()
                                    .as_bytes()
                                    .unwrap()
                                    .to_vec()
                            })
                            .map_err(|e| (e.kind, e.message));
                        assert_eq!(ctx.stats().retained_memory_bytes, 0);
                        (result, failure, steps)
                    };
                    assert_eq!(run(None), run(Some(false)), "{padding} {limit:?}");
                    assert_eq!(run(None), run(Some(true)), "{padding} {limit:?}");
                }
            }
        }
    }

    /// The escape-at-a-time string reader the batched one replaces, kept as
    /// the accounting oracle.
    fn reference(this: &mut Parser<'_, '_>) -> Result<Value> {
        this.pos += 1;
        let start = this.pos;
        loop {
            if this.pos >= this.input.len() {
                return this.err("unterminated JSON string", Failure::End);
            }
            let end = this.input.len().min(this.pos + CHUNK);
            let span = scan::text_span(&this.input[this.pos..end], Class::JsonParse);
            if span.len > 0 {
                this.ctx.charge(span.steps)?;
                this.pos += span.len;
                continue;
            }
            if this.input[this.pos] == b'"' {
                let value = this.ctx.bytes(&this.input[start..this.pos])?;
                this.pos += 1;
                return Ok(value);
            }
            break;
        }
        let mut out = Buffer::with_capacity(&mut this.ctx, this.pos - start)?;
        out.extend(&mut this.ctx, &this.input[start..this.pos])?;
        loop {
            if this.pos >= this.input.len() {
                return this.err("unterminated JSON string", Failure::End);
            }
            let end = this.input.len().min(this.pos + CHUNK);
            let span = scan::text_span(&this.input[this.pos..end], Class::JsonParse);
            if span.len > 0 {
                if span.runes != span.len {
                    this.ctx.charge(span.steps)?;
                }
                out.extend(&mut this.ctx, &this.input[this.pos..this.pos + span.len])?;
                this.pos += span.len;
                continue;
            }
            this.ctx.charge(1)?;
            let b = this.input[this.pos];
            this.pos += 1;
            match b {
                b'"' => return Value::from_bytes(&mut this.ctx, out),
                b'\\' => {
                    let Some(&b) = this.input.get(this.pos) else {
                        return this.err("incomplete JSON escape", Failure::End);
                    };
                    this.pos += 1;
                    match b {
                        b'"' | b'\\' | b'/' => out.push(&mut this.ctx, b)?,
                        b'b' => out.push(&mut this.ctx, 8)?,
                        b'f' => out.push(&mut this.ctx, 12)?,
                        b'n' => out.push(&mut this.ctx, b'\n')?,
                        b'r' => out.push(&mut this.ctx, b'\r')?,
                        b't' => out.push(&mut this.ctx, b'\t')?,
                        b'u' => {
                            let high = this.hex()?;
                            let cp = if (0xd800..=0xdbff).contains(&high) {
                                if this.input[this.pos..].starts_with(b"\\u") {
                                    let saved = this.pos;
                                    this.pos += 2;
                                    let low = this.hex()?;
                                    if (0xdc00..=0xdfff).contains(&low) {
                                        0x10000
                                            + ((high as u32 - 0xd800) << 10)
                                            + (low as u32 - 0xdc00)
                                    } else {
                                        this.pos = saved;
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
                            out.extend(&mut this.ctx, ch.encode_utf8(&mut buf).as_bytes())?;
                        }
                        _ => return this.err("invalid JSON escape", Failure::Escape(b)),
                    }
                }
                0..=31 => {
                    return this.err("unescaped control byte in JSON string", Failure::Literal(b));
                }
                _ => {
                    this.pos -= 1;
                    let (ch, n, _) = scan::rune(&this.input[this.pos..]);
                    this.pos += n;
                    let mut buf = [0; 4];
                    out.extend(&mut this.ctx, ch.encode_utf8(&mut buf).as_bytes())?;
                }
            }
        }
    }

    #[test]
    fn integer_literals_read_as_str_parse_does() {
        let mut cases: Vec<String> = [
            "0",
            "-0",
            "7",
            "-7",
            "9223372036854775807",
            "9223372036854775808",
            "-9223372036854775808",
            "-9223372036854775809",
            "99999999999999999999",
            "-100000000000000000000",
        ]
        .map(String::from)
        .to_vec();
        let mut n = 1u64;
        for _ in 0..200 {
            n = n
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let digits = (n >> 7).to_string();
            cases.push(digits[..1 + (n as usize % digits.len())].to_string());
            cases.push(format!(
                "-{}",
                &digits[..1 + (n as usize >> 3) % digits.len()]
            ));
        }
        for text in cases {
            assert_eq!(integer(text.as_bytes()), text.parse::<i64>().ok(), "{text}");
        }
    }

    #[test]
    fn batched_strings_match_escape_at_a_time_parsing() {
        let pieces: [&[u8]; 22] = [
            b"a",
            b"plain ",
            b"\\n",
            b"\\t",
            b"\\\"",
            b"\\\\",
            b"\\/",
            b"\\b",
            b"\\u00e9",
            b"\\ud83d\\ude42",
            b"\\ud800",
            b"\\u12",
            b"\\x",
            b"\\",
            "é".as_bytes(),
            "界".as_bytes(),
            "🙂".as_bytes(),
            b"\x01",
            b"\xff",
            b"\xe7\x95",
            b"\"",
            b"<>&",
        ];
        let mut seed = 0x853c49e6748fea9bu64;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for case in 0..600 {
            let mut input = b"\"".to_vec();
            let target = [1, 8, 60, 700, 4100, 9000][case % 6] + next(40);
            while input.len() < target {
                let piece = pieces[next(pieces.len())];
                for _ in 0..1 + next(3) * next(40) {
                    input.extend_from_slice(piece);
                }
            }
            if next(4) != 0 {
                input.push(b'"');
            }
            let mut plain = CallContext::new(CallOptions::default());
            let _ = reference(&mut Parser::new(&mut plain, &input));
            let steps = plain.stats().steps as usize;
            let peak = plain.stats().peak_memory_bytes;
            for limits in [
                (None, None),
                (Some(next(steps + 2) as u64), None),
                (None, Some(next(peak + 64))),
                (Some(next(steps + 2) as u64), Some(next(peak + 64))),
            ] {
                let run = |read: fn(&mut Parser<'_, '_>) -> Result<Value>| {
                    let mut ctx = CallContext::new(CallOptions {
                        limits: Limits {
                            steps: limits.0,
                            memory_bytes: limits.1.or(Some(usize::MAX)),
                            ..Limits::default()
                        },
                        ..CallOptions::default()
                    });
                    let mut parser = Parser::new(&mut ctx, &input);
                    let result = read(&mut parser)
                        .map(|value| value.as_bytes().unwrap().to_vec())
                        .map_err(|error| (error.kind, error.message));
                    let (pos, failure) = (parser.pos, parser.failure);
                    drop(parser);
                    let stats = ctx.stats();
                    // Only an exhausted step quota may stop at a different
                    // count and position, since pending steps are charged
                    // together; the call cannot continue after it either way.
                    let exact = !matches!(result, Err((ErrorKind::Steps, _)));
                    (
                        result,
                        exact.then_some(pos),
                        failure,
                        exact.then_some(stats.steps),
                        stats.peak_memory_bytes,
                        stats.retained_memory_bytes,
                    )
                };
                assert_eq!(
                    run(reference),
                    run(|parser| parser.string::<false>()),
                    "case {case} {limits:?}"
                );
            }
        }
    }
}
