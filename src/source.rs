use crate::{
    CallContext, Diagnostic, Error, Position, Result,
    budget::{Buffer, Charge},
};
use std::fmt::{self, Write};
use std::sync::Arc;

const STRIDE: usize = 4096;
const WINDOW: usize = 160;

#[derive(Debug)]
struct Checkpoint {
    offset: u32,
    line: u32,
    column: u32,
}

#[derive(Debug)]
pub(crate) struct Source {
    pub filename: Option<Arc<[u8]>>,
    text: Box<str>,
    checkpoints: Vec<Checkpoint>,
}

impl Source {
    /// What [`Self::compile`] makes of a text of `length` bytes at most:
    /// its copy, and its index of positions while it grows.
    pub fn bytes(length: usize) -> usize {
        let checkpoints = (length / STRIDE + 1).next_power_of_two();
        length.saturating_add(checkpoints.saturating_mul(3 * std::mem::size_of::<Checkpoint>()))
    }

    #[cfg(test)]
    pub fn new(text: &str) -> Self {
        Self::compile(text, &()).expect("unmetered source indexing cannot fail")
    }

    pub fn compile(text: &str, work: &dyn crate::compilation::Work) -> Result<Self> {
        let mut checkpoints = vec![Checkpoint {
            offset: 0,
            line: 1,
            column: 1,
        }];
        let mut position = Position { line: 1, column: 1 };
        for (offset, ch) in text.char_indices() {
            work.charge(1)?;
            if offset - checkpoints.last().unwrap().offset as usize >= STRIDE {
                checkpoints.push(Checkpoint {
                    offset: offset as u32,
                    line: position.line as u32,
                    column: position.column as u32,
                });
            }
            advance(&mut position, ch);
        }
        work.bytes(text.len())?;
        Ok(Self {
            filename: None,
            text: text.into(),
            checkpoints,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    fn location(&self, offset: u32) -> (&str, Position) {
        let offset = boundary(&self.text, offset as usize);
        let checkpoint = &self.checkpoints[self
            .checkpoints
            .partition_point(|p| p.offset as usize <= offset)
            - 1];
        (
            &self.text[checkpoint.offset as usize..offset],
            Position {
                line: checkpoint.line as usize,
                column: checkpoint.column as usize,
            },
        )
    }

    pub fn position(&self, offset: u32) -> Position {
        let (text, mut position) = self.location(offset);
        for ch in text.chars() {
            advance(&mut position, ch);
        }
        position
    }

    pub fn position_metered(&self, ctx: &mut CallContext, offset: u32) -> Result<Position> {
        ctx.work_bytes(self.location(offset).0.len())?;
        Ok(self.position(offset))
    }

    pub fn frame(&self, offset: u32) -> String {
        frame(
            &self.text,
            boundary(&self.text, offset as usize),
            self.position(offset),
            self.filename.as_deref(),
        )
    }

    pub fn frame_metered(
        &self,
        ctx: &mut CallContext,
        offset: u32,
        position: Position,
    ) -> Result<(String, Option<Charge>)> {
        ctx.work_bytes(WINDOW * 12)?;
        ctx.work_bytes(self.filename.as_ref().map_or(0, |name| name.len()))?;
        let snippet = Snippet::new(
            &self.text,
            boundary(&self.text, offset as usize),
            position,
            self.filename.as_deref(),
        );
        formatted(ctx, format_args!("{snippet}"))
    }
}

fn advance(position: &mut Position, ch: char) {
    if ch == '\n' {
        position.line += 1;
        position.column = 1;
    } else {
        position.column += 1;
    }
}

fn boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

struct Snippet<'a> {
    text: &'a str,
    caret: &'a str,
    prefix: bool,
    suffix: bool,
    position: Position,
    filename: Option<&'a [u8]>,
}

impl<'a> Snippet<'a> {
    fn new(text: &'a str, offset: usize, position: Position, filename: Option<&'a [u8]>) -> Self {
        let mut start = offset;
        for ch in text[..offset].chars().rev().take(WINDOW / 2) {
            if ch == '\n' {
                break;
            }
            start -= ch.len_utf8();
        }
        let mut end = start;
        let mut count = 0;
        for ch in text[start..].chars().take(WINDOW) {
            if ch == '\n' {
                break;
            }
            end += ch.len_utf8();
            count += 1;
        }
        if count < WINDOW {
            for ch in text[..start].chars().rev().take(WINDOW - count) {
                if ch == '\n' {
                    break;
                }
                start -= ch.len_utf8();
            }
        }
        Self {
            text: &text[start..end],
            caret: &text[start..offset],
            prefix: start > 0 && text.as_bytes()[start - 1] != b'\n',
            suffix: end < text.len() && text.as_bytes()[end] != b'\n',
            position,
            filename,
        }
    }
}

impl fmt::Display for Snippet<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Position { line, column } = self.position;
        f.write_str("  --> ")?;
        if self.filename.is_some() {
            write!(
                f,
                "{}",
                Location {
                    filename: self.filename,
                    position: self.position
                }
            )?;
        } else {
            write!(f, "line {line}, column {column}")?;
        }
        write!(
            f,
            "\n {line} | {}{}{}\n ",
            if self.prefix { "..." } else { "" },
            self.text,
            if self.suffix { "..." } else { "" }
        )?;
        for _ in 0..line.checked_ilog10().unwrap_or(0) + 1 {
            f.write_char(' ')?;
        }
        f.write_str(" | ")?;
        if self.prefix {
            f.write_str("   ")?;
        }
        for ch in self.caret.chars() {
            f.write_char(if ch == '\t' { '\t' } else { ' ' })?;
        }
        f.write_char('^')
    }
}

fn frame(text: &str, offset: usize, position: Position, filename: Option<&[u8]>) -> String {
    Snippet::new(text, offset, position, filename).to_string()
}

pub(crate) struct Location<'a> {
    pub filename: Option<&'a [u8]>,
    pub position: Position,
}

impl fmt::Display for Location<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(mut bytes) = self.filename {
            while !bytes.is_empty() {
                let (text, invalid) = match std::str::from_utf8(bytes) {
                    Ok(text) => (text, 0),
                    Err(error) => (
                        std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
                        error
                            .error_len()
                            .unwrap_or(bytes.len() - error.valid_up_to()),
                    ),
                };
                for ch in text.chars() {
                    if ch.is_control() || ch == '\\' {
                        write!(f, "{}", ch.escape_default())?;
                    } else {
                        f.write_char(ch)?;
                    }
                }
                bytes = &bytes[text.len()..];
                for byte in &bytes[..invalid] {
                    write!(f, "\\x{byte:02x}")?;
                }
                bytes = &bytes[invalid..];
            }
            f.write_char(':')?;
        }
        write!(f, "{}:{}", self.position.line, self.position.column)
    }
}

pub(crate) fn formatted(
    ctx: &mut CallContext,
    args: fmt::Arguments<'_>,
) -> Result<(String, Option<Charge>)> {
    struct Count(usize);
    impl fmt::Write for Count {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            self.0 = self.0.checked_add(text.len()).ok_or(fmt::Error)?;
            Ok(())
        }
    }
    let mut length = Count(0);
    if length.write_fmt(args).is_err() {
        return ctx.fail(crate::ErrorKind::Memory, "diagnostic size overflow");
    }
    ctx.work_bytes(length.0)?;
    let mut buffer = Buffer::with_capacity(ctx, length.0)?;
    std::io::Write::write_fmt(&mut buffer.data, args).unwrap();
    debug_assert_eq!(buffer.data.len(), length.0);
    let (bytes, charge) = buffer.into_parts();
    Ok((String::from_utf8(bytes).unwrap(), charge))
}

pub(crate) fn parse_error(
    source: &str,
    filename: Option<&Arc<[u8]>>,
    mut error: Error,
    work: &dyn crate::compilation::Work,
) -> Error {
    if let Err(error) = work.checkpoint() {
        return error;
    }
    if error.kind == crate::ErrorKind::Syntax && error.diagnostics().is_empty() {
        let start = boundary(source, error.offset.unwrap_or(0));
        let end = start + source[start..].chars().next().map_or(0, char::len_utf8);
        let diagnostic = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::SYNTAX,
            crate::diagnostic::Span::new(start, end),
            error.message.clone(),
        )
        .in_file(filename.cloned());
        error = error.with_diagnostic(diagnostic);
    }
    if let Some(offset) = error
        .offset
        .filter(|_| source.len() <= crate::syntax::MAX_SOURCE)
    {
        let mut offset = boundary(source, offset);
        let mut position = Position { line: 1, column: 1 };
        for ch in source[..offset].chars() {
            if let Err(error) = work.charge(1) {
                return error;
            }
            advance(&mut position, ch);
        }
        // Go stamps the end of input with the position of the final
        // character, or column 0 of the line after a final line break.
        let mut framed = position;
        if offset == source.len() {
            match source.chars().next_back() {
                Some(last) if last != '\n' => {
                    position.column -= 1;
                    framed = position;
                    offset -= last.len_utf8();
                }
                _ => position.column = 0,
            }
        }
        let build = || {
            let mut charge = match error.retained_charge.take() {
                Some(charge) => match Arc::try_unwrap(charge) {
                    Ok(charge) => Some(charge),
                    Err(shared) => work.reserve(shared.bytes())?,
                },
                None => work.reserve(
                    error.allocation_bytes() + size_of::<Charge>() + 2 * size_of::<usize>(),
                )?,
            };
            Charge::merge(
                &mut charge,
                work.reserve(
                    size_of::<Diagnostic>()
                        + 2 * size_of::<usize>()
                        + filename.map_or(0, |name| name.len() + 2 * size_of::<usize>()),
                )?,
            );
            work.bytes(WINDOW * 12)?;
            work.bytes(filename.map_or(0, |name| name.len()))?;
            let snippet = Snippet::new(source, offset, framed, filename.map(|name| &**name));
            let (code_frame, storage) =
                crate::compilation::formatted(work, format_args!("{snippet}"))?;
            Charge::merge(&mut charge, storage);
            error.diagnostic = Some(Arc::new(Diagnostic {
                filename: filename.cloned(),
                position,
                code_frame,
                frames: Vec::new(),
            }));
            error.retained_charge = charge.map(Arc::new);
            Ok::<_, Error>(error)
        };
        return build().unwrap_or_else(|error| error);
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_preserve_unicode_and_escape_control_characters_and_invalid_bytes() {
        for (filename, rendered) in [
            ("pkg/日本語.vibe".as_bytes(), "pkg/日本語.vibe"),
            (b"pkg/a\n\r\t\\.vibe".as_slice(), "pkg/a\\n\\r\\t\\\\.vibe"),
            (
                b"pkg/\xff\xc0\x80\xe2\x98".as_slice(),
                "pkg/\\xff\\xc0\\x80\\xe2\\x98",
            ),
            (b"a\0\x1b\x7f".as_slice(), "a\\u{0}\\u{1b}\\u{7f}"),
        ] {
            let mut source = Source::new("1/0");
            source.filename = Some(filename.into());
            let expected = format!("  --> {rendered}:1:2\n 1 | 1/0\n   |  ^");
            assert_eq!(source.frame(1), expected);
            let mut ctx = CallContext::new(crate::CallOptions::default());
            let (text, charge) = source
                .frame_metered(&mut ctx, 1, source.position(1))
                .unwrap();
            assert_eq!(text, expected);
            assert!(ctx.stats().retained_memory_bytes >= text.len());
            drop((text, charge));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn sparse_positions_agree_with_a_linear_unicode_walk() {
        for part in ["α🐈x", "\n", "\r\nα\t", "a\n🐈b"] {
            let text = part.repeat(5000);
            let source = Source::new(&text);
            let mut position = Position { line: 1, column: 1 };
            for (offset, ch) in text.char_indices() {
                if offset % 83 < 4 || offset % STRIDE < 5 {
                    for byte in offset..offset + ch.len_utf8() {
                        assert_eq!(source.position(byte as u32), position, "offset {byte}");
                    }
                }
                if ch == '\n' {
                    position.line += 1;
                    position.column = 1;
                } else {
                    position.column += 1;
                }
            }
            assert_eq!(source.position(text.len() as u32), position);
            assert_eq!(source.position(u32::MAX), position);
            assert!(source.checkpoints.len() <= text.len() / STRIDE + 1);
        }
        let source = Source::new("");
        assert_eq!(source.position(0), Position { line: 1, column: 1 });
        assert_eq!(source.frame(0), "  --> line 1, column 1\n 1 | \n   | ^");
    }

    #[test]
    fn clipped_frames_match_a_whole_line_oracle_at_every_column() {
        for length in [0, 1, 159, 160, 161, 319, 320] {
            let line: String = "α🐈z".chars().cycle().take(length).collect();
            let chars: Vec<_> = line.chars().collect();
            let text = format!("before\n{line}\nafter");
            let source = Source::new(&text);
            for (column, byte) in line
                .char_indices()
                .map(|(offset, _)| offset)
                .chain([line.len()])
                .enumerate()
            {
                let start = column.saturating_sub(80).min(length.saturating_sub(160));
                let end = (start + 160).min(length);
                let shown: String = chars[start..end].iter().collect();
                let prefix = if start > 0 { "..." } else { "" };
                let suffix = if end < length { "..." } else { "" };
                let expected = format!(
                    "  --> line 2, column {}\n 2 | {prefix}{shown}{suffix}\n   | {}^",
                    column + 1,
                    " ".repeat(column - start + prefix.len()),
                );
                assert_eq!(source.frame((7 + byte) as u32), expected);
            }
        }
    }

    #[test]
    fn frames_preserve_tabs_and_handle_crlf_and_empty_final_lines() {
        let source = Source::new("first\r\n\tα / 0\r\n");
        assert_eq!(
            source.frame(11),
            "  --> line 2, column 4\n 2 | \tα / 0\r\n   | \t  ^"
        );
        assert_eq!(
            source.frame(u32::MAX),
            "  --> line 3, column 1\n 3 | \n   | ^"
        );
    }
}
