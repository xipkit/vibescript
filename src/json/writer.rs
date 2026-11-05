use crate::bytes::Number;
use crate::checked::violation;
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    scan::{self, Class},
    value::Kind,
};

/// The longest escape, `\u00XX`.
const LONGEST: usize = 6;

/// The kind of a `\u00XX` escape in [`ESCAPES`].
const LONG: u8 = 1;

/// How each ASCII byte is written. An entry holds the escape's text, then
/// its length, then its kind: zero for a byte that is written as itself,
/// `LONG` for a `\u00XX` escape, otherwise the letter of a two-byte escape.
static ESCAPES: [[u8; 8]; 128] = {
    let hex = b"0123456789abcdef";
    let mut escapes = [[0; 8]; 128];
    let mut byte = 0;
    while byte < 128 {
        let letter = match byte as u8 {
            b'"' => b'"',
            b'\\' => b'\\',
            b'\n' => b'n',
            b'\r' => b'r',
            b'\t' => b't',
            8 => b'b',
            12 => b'f',
            _ => 0,
        };
        if letter != 0 {
            escapes[byte] = [b'\\', letter, 0, 0, 0, 0, 2, letter];
        } else if byte < 32 || matches!(byte as u8, b'<' | b'>' | b'&') {
            let digits = [hex[byte >> 4], hex[byte & 15]];
            escapes[byte] = [b'\\', b'u', b'0', b'0', digits[0], digits[1], 6, LONG];
        }
        byte += 1;
    }
    escapes
};

/// The kind of escape `byte` is written as, or zero when it has none.
#[inline(always)]
fn escape_kind(byte: u8) -> u8 {
    ESCAPES.get(usize::from(byte)).map_or(0, |escape| escape[7])
}

/// Bytes of output a run of escapes is assembled in.
const BLOCK: usize = 128;

pub(super) struct Output {
    pub buffer: Buffer<u8>,
    limit: Option<usize>,
    /// Report failures as the script-facing `JSON.stringify` does.
    script: bool,
}

impl Output {
    pub fn new(limit: Option<usize>, script: bool) -> Self {
        Self {
            buffer: Buffer::empty(),
            limit,
            script,
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

    /// Appends `bytes` as [`Self::extend`] does, adding its work to `pending`
    /// instead of charging it. Pending steps are settled before the limit
    /// check can fail or the buffer grows, so both happen after the same
    /// charges as with `extend`. The caller settles `pending` at least once
    /// per chunk of input, so the counter stays far below overflowing.
    #[inline(always)]
    fn put(&mut self, ctx: &mut CallContext, bytes: &[u8], pending: &mut u64) -> Result<()> {
        let length = self.buffer.data.len().saturating_add(bytes.len());
        if length > self.buffer.data.capacity() || self.limit.is_some_and(|limit| length > limit) {
            self.grow(ctx, length, pending)?;
        }
        if let [byte] = bytes {
            self.buffer.data.push(*byte);
        } else {
            self.buffer.data.extend_from_slice(bytes);
        }
        *pending = pending.wrapping_add((bytes.len() as u64).div_ceil(64));
        Ok(())
    }

    #[cold]
    fn grow(&mut self, ctx: &mut CallContext, length: usize, pending: &mut u64) -> Result<()> {
        ctx.charge_pending(pending)?;
        self.check(ctx, length)?;
        self.ensure(
            ctx,
            length.max(self.buffer.data.capacity().saturating_mul(2)),
        )
    }

    #[inline(always)]
    fn put_escape(&mut self, ctx: &mut CallContext, code: u8, pending: &mut u64) -> Result<()> {
        let length = self.buffer.data.len().saturating_add(2);
        if length > self.buffer.data.capacity() || self.limit.is_some_and(|limit| length > limit) {
            self.grow(ctx, length, pending)?;
        }
        self.buffer.data.extend_from_slice(&[b'\\', code]);
        *pending = pending.wrapping_add(1);
        Ok(())
    }

    /// Appends the escapes `input` starts with, along with each ordinary byte
    /// that stands alone before another of them, and returns the input
    /// consumed and the steps that appending them one at a time defers.
    /// `input` begins at an escape or at the start of a span. The run stops
    /// while the longest escape still fits under the capacity and the limit,
    /// so none of those appends could have failed or grown the buffer;
    /// nothing is consumed when there is no such room.
    ///
    /// The run is assembled in a local block, out of line, so that its
    /// cursors and step count stay in registers. Appending escape by escape
    /// carries the output length and the deferred steps through memory, and
    /// that loop ran a tenth slower or faster with where the linker happened
    /// to put it.
    #[inline(never)]
    fn put_run(&mut self, input: &[u8]) -> (usize, u64) {
        let data = &mut self.buffer.data;
        let room = data
            .capacity()
            .min(self.limit.unwrap_or(usize::MAX))
            .saturating_sub(data.len());
        // An escape is stored as its whole table entry, so each byte needs
        // room for one.
        let Some(last) = room.min(BLOCK).checked_sub(8) else {
            return (0, 0);
        };
        let mut block = [0; BLOCK];
        let (mut rest, mut written, mut steps) = (input, 0, 0u64);
        while let [byte, tail @ ..] = rest {
            if written > last {
                break;
            }
            let Some(escape) = ESCAPES.get(usize::from(*byte)) else {
                break;
            };
            let length = usize::from(escape[6]);
            // At most two steps a byte, so the count cannot overflow.
            if length != 0 {
                block[written..written + 8].copy_from_slice(escape);
                written += length;
                // One step for the escape and one for appending it.
                steps = steps.wrapping_add(2);
            } else if tail.first().is_some_and(|&next| escape_kind(next) != 0) {
                // A one-byte span. A longer one is left to the span scanner.
                block[written] = *byte;
                written += 1;
                steps = steps.wrapping_add(1);
            } else {
                break;
            }
            rest = tail;
        }
        data.extend_from_slice(&block[..written]);
        (input.len() - rest.len(), steps)
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

/// Where in the walk an encoding failure happened, which decides the path of
/// keys and indexes the reference names in front of it.
#[derive(Clone, Copy)]
enum Site {
    /// Inside the current element of every open container.
    Value,
    /// Before the next element of the innermost container.
    Next,
    /// In the innermost container itself, outside any of its elements.
    Container,
}

/// Encodes `root` using an explicit, budget-accounted frame stack.
///
/// Nesting is bounded by the frame count rather than the native stack, and a
/// container is rejected at entry once `MAX_VALUE_DEPTH` containers are open.
pub(super) fn write_value(ctx: &mut CallContext, root: &Value, out: &mut Output) -> Result<()> {
    write_value_with::<true>(ctx, root, out)
}

fn write_value_with<const BATCH: bool>(
    ctx: &mut CallContext,
    root: &Value,
    out: &mut Output,
) -> Result<()> {
    let mut budget = super::accounting::Steps::new(ctx, CHUNK);
    #[cfg(test)]
    {
        budget.unbatched = !BATCH;
    }
    let ctx = &mut budget;
    let mut frames: Buffer<Frame<'_>> = Buffer::empty();
    let mut current = root;
    let (error, site) = 'failed: loop {
        match start(ctx, current, out, frames.data.len()) {
            Ok(Some(frame)) => frames.push(ctx, frame)?,
            Ok(None) => {}
            Err(error) => break 'failed (error, Site::Value),
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
                    let items: &[Value] = items;
                    if let Some(item) = items.get(*next) {
                        if *next > 0 {
                            if let Err(error) = out.push(ctx, b',') {
                                break 'failed (error, Site::Next);
                            }
                        }
                        *next += 1;
                        current = item;
                        break;
                    }
                    if let Err(error) = out.push(ctx, b']') {
                        break 'failed (error, Site::Container);
                    }
                    frames.data.pop();
                }
                Frame::Hash { entries, next } => {
                    let entries: &[(Value, Value)] = entries;
                    if let Some((key, value)) = entries.get(*next) {
                        if *next > 0 {
                            if let Err(error) = out.push(ctx, b',') {
                                break 'failed (error, Site::Next);
                            }
                        }
                        if let Err(error) = write_string(ctx, key.require_bytes()?, out, open) {
                            break 'failed (error, Site::Container);
                        }
                        if let Err(error) = out.push(ctx, b':') {
                            break 'failed (error, Site::Next);
                        }
                        *next += 1;
                        current = value;
                        break;
                    }
                    if let Err(error) = out.push(ctx, b'}') {
                        break 'failed (error, Site::Container);
                    }
                    frames.data.pop();
                }
            }
        }
    };
    if !out.script {
        return Err(error);
    }
    let detail = match error.kind {
        ErrorKind::OutputLimit => format!(
            "JSON.stringify output exceeds limit {} bytes",
            super::MAX_PAYLOAD
        ),
        ErrorKind::Json => match current.0 {
            Kind::Float(n) => {
                let mut text = Number::new();
                crate::ops::format_float(&mut text, n)?;
                let text = std::str::from_utf8(text.bytes()?)
                    .map_err(|_| violation("formatted float must be UTF-8"))?;
                format!("JSON.stringify failed: json: unsupported value: {text}")
            }
            _ => format!(
                "JSON.stringify unsupported value type {}",
                current.type_name()
            ),
        },
        // The depth limit is reported without the path to it.
        ErrorKind::Recursion => {
            return Err(error.with_message("JSON.stringify exceeded max depth".to_owned()));
        }
        _ => return Err(error),
    };
    let path = Path {
        frames: &frames.data,
        site,
    };
    let (message, _charge) = crate::source::formatted(ctx, format_args!("{path}{detail}"))?;
    Err(error.with_message(message))
}

/// The reference's prefix naming each open container's element on the way to
/// a failure, outermost first.
struct Path<'f, 'a> {
    frames: &'f [Frame<'a>],
    site: Site,
}

impl std::fmt::Display for Path<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let last = self.frames.len().wrapping_sub(1);
        for (depth, frame) in self.frames.iter().enumerate() {
            // Each open container has advanced past the element being written,
            // except the innermost one before its next element.
            let next = match frame {
                Frame::Array { next, .. } | Frame::Hash { next, .. } => *next,
            };
            let index = match self.site {
                Site::Next if depth == last => next,
                Site::Container if depth == last => break,
                _ => next.wrapping_sub(1),
            };
            match frame {
                Frame::Array { .. } => write!(f, "JSON.stringify array index {index}: ")?,
                Frame::Hash { entries, .. } => {
                    let mut quoted = Vec::new();
                    crate::shapes::quote(
                        entries[index].0.as_bytes().unwrap_or_default(),
                        &mut quoted,
                    );
                    write!(
                        f,
                        "JSON.stringify key {}: ",
                        String::from_utf8_lossy(&quoted)
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Writes a scalar completely, or opens a container and returns its frame.
/// `open` is the number of containers currently open around `value`.
fn start<'a>(
    ctx: &mut super::accounting::Steps<'_>,
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
        Kind::EnumMember(m) => write_string(ctx, m.definition()?.symbol.as_bytes(), out, open)?,
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
            text.format(format_args!("{n}"))?;
            out.extend(ctx, text.bytes()?)?;
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
                scientific.format(format_args!("{n:e}"))?;
                let scientific = std::str::from_utf8(scientific.bytes()?)
                    .map_err(|_| violation("scientific number must be UTF-8"))?;
                let (mantissa, exponent) = scientific
                    .split_once('e')
                    .ok_or(violation("scientific exponent expected"))?;
                let exponent: i32 = exponent
                    .parse()
                    .map_err(|_| violation("scientific exponent must be an integer"))?;
                text.format(format_args!("{mantissa}e{exponent:+}"))?;
            } else {
                text.format(format_args!("{n}"))?;
            }
            out.extend(ctx, text.bytes()?)?;
        }
        Kind::Bytes(h) | Kind::Symbol(h) => write_string(ctx, h.data(), out, open)?,
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
    // Steps are charged as when every span and escape was appended with
    // `Output::extend`, but settled in batches: before anything that can fail
    // or grow the buffer, so failures and growth follow the same charges, and
    // after each chunk of input, so cancellation stays responsive. Settling
    // that often, with at most two steps per input byte, keeps `pending` far
    // from overflowing, so the loop adds to it without checks.
    let mut pending: u64 = 0;
    let mut settle_at = CHUNK;
    let mut i = 0;
    'bytes: while i < input.len() {
        if i >= settle_at {
            ctx.charge_pending(&mut pending)?;
            ctx.checkpoint()?;
            settle_at = i + CHUNK;
        }
        let mut b = input[i];
        let mut escape = escape_kind(b);
        while escape != 0 {
            pending = pending.wrapping_add(1);
            i += 1;
            // Go reserves room for the longest escape before any ASCII escape.
            let reserved = out.buffer.data.len().saturating_add(LONGEST);
            if out.limit.is_some_and(|limit| reserved > limit) {
                ctx.charge_pending(&mut pending)?;
                out.check(ctx, reserved)?;
            }
            if escape != LONG {
                out.put_escape(ctx, escape, &mut pending)?;
            } else {
                let [text @ .., _, _] = ESCAPES[usize::from(b)];
                out.put(ctx, &text, &mut pending)?;
            }
            let Some(&next) = input.get(i) else {
                break 'bytes;
            };
            b = next;
            escape = escape_kind(b);
            // A lone escape or a pair costs least appended in place, as
            // above. A third in a row starts a run.
            if escape != 0 && input.get(i + 1).is_some_and(|&next| escape_kind(next) != 0) {
                let (run, steps) = out.put_run(&input[i..]);
                i += run;
                pending = pending.wrapping_add(steps);
                continue 'bytes;
            }
        }
        let window = &input[i..input.len().min(i + CHUNK)];
        if b < 128 {
            // A one-byte run between ASCII escapes needs no vector/SWAR mask.
            if window.get(1).is_some_and(|&next| escape_kind(next) != 0) {
                // With another escape close behind it, it starts a run.
                if window
                    .get(2..4)
                    .is_some_and(|ahead| ahead.iter().any(|&next| escape_kind(next) != 0))
                {
                    let (run, steps) = out.put_run(&input[i..]);
                    if run != 0 {
                        i += run;
                        pending = pending.wrapping_add(steps);
                        continue;
                    }
                }
                out.put(ctx, &window[..1], &mut pending)?;
                i += 1;
                continue;
            }
            // An ordinary run ending at an escape or the window is exactly
            // the span `text_span` would find, so it skips the rune scan.
            let n = scan::prefix(window, Class::JsonStringify);
            if window.get(n).is_none_or(|&next| next < 128) {
                out.put(ctx, &window[..n], &mut pending)?;
                i += n;
                continue;
            }
        }
        let span = scan::text_span(window, Class::JsonStringify);
        if span.len > 0 {
            if span.runes != span.len {
                pending = pending.wrapping_add(span.steps);
            }
            out.put(ctx, &window[..span.len], &mut pending)?;
            i += span.len;
            continue;
        }
        // A rune the span stopped at: invalid, a line or paragraph separator,
        // or cut by the end of the chunk.
        pending = pending.wrapping_add(1);
        let (ch, n, valid) = scan::rune(&input[i..]);
        let replacement: &[u8] = if ch == '\u{2028}' {
            b"\\u2028"
        } else if ch == '\u{2029}' {
            b"\\u2029"
        } else if !valid {
            b"\\ufffd"
        } else {
            &input[i..i + n]
        };
        i += n;
        out.put(ctx, replacement, &mut pending)?;
    }
    ctx.charge_pending(&mut pending)?;
    out.push(ctx, b'"')?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn every_writer_quota_boundary_matches_unbatched_accounting() {
        let value = Value::array(vec![
            Value::int(7),
            Value::hash(vec![(b"a".to_vec(), Value::bytes(b"x\ny"))]),
            Value::nil(),
        ]);
        let run = |batched, steps, memory| {
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps,
                    memory_bytes: memory,
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let mut out = Output::new(Some(1024), true);
            let result = if batched {
                write_value_with::<true>(&mut ctx, &value, &mut out)
            } else {
                write_value_with::<false>(&mut ctx, &value, &mut out)
            };
            let stats = ctx.stats();
            let bytes = out.buffer.data.clone();
            drop(out);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            (
                result,
                bytes,
                stats.steps,
                stats.peak_memory_bytes,
                stats.retained_memory_bytes,
                ctx.checkpoint(),
            )
        };
        let baseline = run(false, None, None);
        assert_eq!(run(true, None, None), baseline);
        for steps in 0..=baseline.2 + 1 {
            for memory in 0..=baseline.3 + 1 {
                assert_eq!(
                    run(true, Some(steps), Some(memory)),
                    run(false, Some(steps), Some(memory)),
                    "steps {steps}, memory {memory}"
                );
            }
        }
    }

    #[test]
    fn escape_runs_stop_at_the_limit_inside_a_larger_capacity() {
        let input = b"\n\t\"\\a\n<\n\n\n&a\r";
        for limit in 0..96 {
            let run = |write: fn(&mut CallContext, &[u8], &mut Output, usize) -> Result<()>| {
                let mut ctx = CallContext::new(CallOptions::default());
                let mut out = Output {
                    buffer: Buffer::untracked(Vec::with_capacity(256)),
                    limit: Some(limit),
                    script: true,
                };
                let result = write(&mut ctx, input, &mut out, 0);
                (
                    result.map_err(|error| (error.kind, error.message)),
                    out.buffer.data.clone(),
                    ctx.stats().steps,
                )
            };
            assert_eq!(run(write_string), run(reference), "limit {limit}");
        }
    }

    /// The per-span and per-escape writer this module batches, kept as the
    /// accounting oracle.
    fn reference(ctx: &mut CallContext, input: &[u8], out: &mut Output, open: usize) -> Result<()> {
        out.check(
            ctx,
            out.buffer
                .data
                .len()
                .saturating_add(input.len())
                .saturating_add(2),
        )?;
        let headroom = if input.len() >= CHUNK { open } else { 0 };
        let minimum = out.buffer.data.len() + input.len() + 2 + headroom;
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

    #[test]
    fn batched_escaping_matches_per_escape_output_and_accounting() {
        let pieces: [&[u8]; 20] = [
            b"a",
            b"plain text ",
            b"\"",
            b"\\",
            b"\n",
            b"\t",
            b"\r",
            b"\x08",
            b"\x0c",
            b"\x00",
            b"\x1f",
            b"<>&",
            b"\x7f",
            "é".as_bytes(),
            "界".as_bytes(),
            "🙂".as_bytes(),
            "\u{2028}\u{2029}".as_bytes(),
            b"\xff",
            b"\xe7\x95",
            b"\xf0\x9f\x99",
        ];
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move |bound: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % bound as u64) as usize
        };
        for case in 0..400 {
            let mut input = Vec::new();
            let target = [0, 1, 7, 100, 4095, 4097, 9000][case % 7] + next(64);
            while input.len() < target {
                let piece = pieces[next(pieces.len())];
                for _ in 0..1 + next(3) * next(40) {
                    input.extend_from_slice(piece);
                }
            }
            let open = next(3);
            let mut plain = CallContext::new(CallOptions::default());
            let mut expected = Output::new(None, true);
            reference(&mut plain, &input, &mut expected, open).unwrap();
            let steps = plain.stats().steps;
            let bytes = expected.buffer.data.len();
            for limits in [
                (None, None, None),
                (Some(next(steps as usize + 2) as u64), None, None),
                (None, Some(next(bytes * 2 + 64)), None),
                (None, None, Some(next(bytes + 8))),
                (
                    Some(next(steps as usize + 2) as u64),
                    Some(next(bytes * 2 + 64)),
                    Some(next(bytes + 8)),
                ),
            ] {
                let run = |write: fn(&mut CallContext, &[u8], &mut Output, usize) -> Result<()>| {
                    let mut ctx = CallContext::new(CallOptions {
                        limits: Limits {
                            steps: limits.0,
                            memory_bytes: limits.1.or(Some(usize::MAX)),
                            ..Limits::default()
                        },
                        ..CallOptions::default()
                    });
                    let mut out = Output::new(limits.2, true);
                    let result = write(&mut ctx, &input, &mut out, open);
                    let stats = ctx.stats();
                    (
                        result.map(|()| {
                            (
                                out.buffer.data.clone(),
                                stats.steps,
                                stats.peak_memory_bytes,
                                stats.retained_memory_bytes,
                            )
                        }),
                        stats.peak_memory_bytes,
                    )
                };
                let (want, want_peak) = run(reference);
                let (got, got_peak) = run(write_string);
                match (want, got) {
                    (Ok(want), Ok(got)) => assert_eq!(want, got, "case {case} {limits:?}"),
                    (Err(want), Err(got)) => {
                        assert_eq!(
                            (want.kind, want.message),
                            (got.kind, got.message),
                            "case {case} {limits:?}"
                        );
                        assert_eq!(want_peak, got_peak, "case {case} {limits:?}");
                    }
                    (want, got) => panic!("case {case} {limits:?}: {want:?} vs {got:?}"),
                }
            }
        }
    }
}
