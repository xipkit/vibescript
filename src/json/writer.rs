use super::Number;
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    scan::{self, Class},
    value::Kind,
};
use std::fmt::Write;

pub(super) struct Output {
    pub buffer: Buffer<u8>,
    limit: Option<usize>,
}

impl Output {
    pub fn new(limit: Option<usize>) -> Self {
        Self {
            buffer: Buffer::empty(),
            limit,
        }
    }

    fn check(&self, ctx: &mut CallContext, length: usize) -> Result<()> {
        if self.limit.is_some_and(|limit| length > limit) {
            return ctx.guard(ErrorKind::OutputLimit, "JSON output exceeds 1 MiB");
        }
        Ok(())
    }

    fn ensure(&mut self, ctx: &mut CallContext, capacity: usize) -> Result<()> {
        self.buffer
            .ensure(ctx, capacity.min(self.limit.unwrap_or(usize::MAX)))
    }

    fn push(&mut self, ctx: &mut CallContext, byte: u8) -> Result<()> {
        self.check(ctx, self.buffer.data.len() + 1)?;
        if self.buffer.data.len() == self.buffer.data.capacity() {
            self.ensure(ctx, self.buffer.data.capacity().max(4).saturating_mul(2))?;
        }
        self.buffer.data.push(byte);
        Ok(())
    }

    fn extend(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        let length = self.buffer.data.len().saturating_add(bytes.len());
        self.check(ctx, length)?;
        if length > self.buffer.data.capacity() {
            self.ensure(
                ctx,
                length.max(self.buffer.data.capacity().saturating_mul(2)),
            )?;
        }
        self.buffer.extend(ctx, bytes)
    }
}

/// A container whose elements are still being written. Frames borrow the
/// value tree, so only the cursor position is stored per open container.
enum Frame<'a> {
    Array {
        items: &'a [Value],
        next: usize,
    },
    Hash {
        entries: &'a [(Value, Value)],
        next: usize,
    },
}

/// Encodes `root` using an explicit, budget-accounted frame stack.
///
/// Nesting is bounded by the frame count rather than the native stack, and a
/// container is rejected at entry once `MAX_VALUE_DEPTH` containers are open.
pub(super) fn write_value<'a>(
    ctx: &mut CallContext,
    root: &'a Value,
    out: &mut Output,
) -> Result<()> {
    let mut frames: Buffer<Frame<'a>> = Buffer::empty();
    let mut current = root;
    loop {
        if let Some(frame) = start(ctx, current, out, frames.data.len())? {
            frames.push(ctx, frame)?;
        }
        // Advance the innermost open container to its next element, closing
        // every container that has been exhausted.
        loop {
            let open = frames.data.len();
            let Some(frame) = frames.data.last_mut() else {
                return Ok(());
            };
            match frame {
                Frame::Array { items, next } => {
                    let items: &'a [Value] = items;
                    if let Some(item) = items.get(*next) {
                        if *next > 0 {
                            out.push(ctx, b',')?;
                        }
                        *next += 1;
                        current = item;
                        break;
                    }
                    out.push(ctx, b']')?;
                    frames.data.pop();
                }
                Frame::Hash { entries, next } => {
                    let entries: &'a [(Value, Value)] = entries;
                    if let Some((key, value)) = entries.get(*next) {
                        if *next > 0 {
                            out.push(ctx, b',')?;
                        }
                        write_string(ctx, key.require_bytes()?, out, open)?;
                        out.push(ctx, b':')?;
                        *next += 1;
                        current = value;
                        break;
                    }
                    out.push(ctx, b'}')?;
                    frames.data.pop();
                }
            }
        }
    }
}

/// Writes a scalar completely, or opens a container and returns its frame.
/// `open` is the number of containers currently open around `value`.
fn start<'a>(
    ctx: &mut CallContext,
    value: &'a Value,
    out: &mut Output,
    open: usize,
) -> Result<Option<Frame<'a>>> {
    ctx.charge(1)?;
    match &value.0 {
        Kind::Regex(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a regex")),
        Kind::Host(method) => return Err(method.value_error()),
        Kind::Function(function) => return Err(function.value_error()),
        Kind::Builtin(_) | Kind::Offset(_) => {
            return Err(Error::new(ErrorKind::Json, "cannot encode a builtin"));
        }
        Kind::Instance(_) => return Err(Error::new(ErrorKind::Json, "cannot encode an instance")),
        Kind::Namespace(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a module")),
        Kind::Shape(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a type literal")),
        Kind::Enum(_) => return Err(Error::new(ErrorKind::Json, "cannot encode an enum type")),
        Kind::EnumMember(m) => write_string(ctx, m.definition().symbol.as_bytes(), out, open)?,
        Kind::Money(_) => return Err(Error::new(ErrorKind::Json, "cannot encode money")),
        Kind::Duration(_) => return Err(Error::new(ErrorKind::Json, "cannot encode a duration")),
        Kind::Time(_) | Kind::Zoned(_) => {
            return Err(Error::new(ErrorKind::Json, "cannot encode a time"));
        }
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
        Kind::Bytes(h) | Kind::Symbol(h) => write_string(ctx, &h.data, out, open)?,
        Kind::Array(h) => {
            enter(ctx, open)?;
            out.push(ctx, b'[')?;
            return Ok(Some(Frame::Array {
                items: &h.buffer.data,
                next: 0,
            }));
        }
        Kind::Hash(h) => {
            enter(ctx, open)?;
            out.push(ctx, b'{')?;
            return Ok(Some(Frame::Hash {
                entries: &h.buffer.data,
                next: 0,
            }));
        }
    }
    Ok(None)
}

/// Rejects a container once `MAX_VALUE_DEPTH` containers are already open.
fn enter(ctx: &mut CallContext, open: usize) -> Result<()> {
    if open >= MAX_VALUE_DEPTH {
        return ctx.guard(ErrorKind::Recursion, "JSON nesting too deep");
    }
    Ok(())
}

/// Writes a quoted string. `open` counts the containers enclosing it; a long
/// string reserves that many extra bytes so the closing delimiters that may
/// immediately follow do not force the buffer to double.
fn write_string(ctx: &mut CallContext, input: &[u8], out: &mut Output, open: usize) -> Result<()> {
    out.check(
        ctx,
        out.buffer
            .data
            .len()
            .saturating_add(input.len())
            .saturating_add(2),
    )?;
    let headroom = if input.len() >= CHUNK { open } else { 0 };
    let Some(minimum) = out
        .buffer
        .data
        .len()
        .checked_add(input.len())
        .and_then(|n| n.checked_add(2))
        .and_then(|n| n.checked_add(headroom))
    else {
        return ctx.fail(ErrorKind::Memory, "JSON output size overflow");
    };
    if minimum > out.buffer.data.capacity() {
        out.ensure(
            ctx,
            minimum.max(out.buffer.data.capacity().saturating_mul(2)),
        )?;
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
        if b.is_ascii() {
            // Go reserves room for the longest escape before any ASCII escape.
            out.check(ctx, out.buffer.data.len().saturating_add(6))?;
        }
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
