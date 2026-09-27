use super::Number;
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    scan::{self, Class},
    value::Kind,
};
use std::fmt::Write;

// Zero means ordinary ASCII, one a six-byte escape, otherwise its short code.
const ASCII_ESCAPES: [u8; 256] = {
    let mut escapes = [0; 256];
    let mut byte = 0;
    while byte < 32 {
        escapes[byte] = 1;
        byte += 1;
    }
    escapes[b'"' as usize] = b'"';
    escapes[b'\\' as usize] = b'\\';
    escapes[b'\n' as usize] = b'n';
    escapes[b'\r' as usize] = b'r';
    escapes[b'\t' as usize] = b't';
    escapes[8] = b'b';
    escapes[12] = b'f';
    escapes[b'<' as usize] = 1;
    escapes[b'>' as usize] = 1;
    escapes[b'&' as usize] = 1;
    escapes
};

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
    /// charges as with `extend`.
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
        *pending += (bytes.len() as u64).div_ceil(64);
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
                crate::ops::format_float(&mut text, n);
                let text = std::str::from_utf8(text.bytes()).unwrap();
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
    // Steps are charged as when every span and escape was appended with
    // `Output::extend`, but settled in batches: before anything that can fail
    // or grow the buffer, so failures and growth follow the same charges, and
    // after each chunk of input, so cancellation stays responsive.
    let mut pending = 0;
    let mut i = 0;
    while i < input.len() {
        let checkpoint = input.len().min(i.saturating_add(CHUNK));
        while i < checkpoint {
            let b = input[i];
            let escape = ASCII_ESCAPES[usize::from(b)];
            if escape != 0 {
                pending += 1;
                i += 1;
                // Go reserves room for the longest escape before any ASCII escape.
                let reserved = out.buffer.data.len().saturating_add(6);
                if out.limit.is_some_and(|limit| reserved > limit) {
                    ctx.charge_pending(&mut pending)?;
                    out.check(ctx, reserved)?;
                }
                if escape != 1 {
                    out.put(ctx, &[b'\\', escape], &mut pending)?;
                } else {
                    let hex = b"0123456789abcdef";
                    let unicode = [
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        hex[(b >> 4) as usize],
                        hex[(b & 15) as usize],
                    ];
                    out.put(ctx, &unicode, &mut pending)?;
                }
                continue;
            }
            let window = &input[i..input.len().min(i + CHUNK)];
            if b < 128 {
                // A one-byte run between ASCII escapes needs no vector/SWAR mask.
                if window
                    .get(1)
                    .is_some_and(|&next| ASCII_ESCAPES[usize::from(next)] != 0)
                {
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
                    pending += span.steps;
                }
                out.put(ctx, &window[..span.len], &mut pending)?;
                i += span.len;
                continue;
            }
            // A rune the span stopped at: invalid, a line or paragraph separator,
            // or cut by the end of the chunk.
            pending += 1;
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
        if i < input.len() {
            ctx.charge_pending(&mut pending)?;
            ctx.checkpoint()?;
        }
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
