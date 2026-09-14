use crate::{Diagnostic, Error, Position};
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
    text: Box<str>,
    checkpoints: Vec<Checkpoint>,
}

impl Source {
    pub fn new(text: &str) -> Self {
        let mut checkpoints = vec![Checkpoint {
            offset: 0,
            line: 1,
            column: 1,
        }];
        let mut position = Position { line: 1, column: 1 };
        for (offset, ch) in text.char_indices() {
            if offset - checkpoints.last().unwrap().offset as usize >= STRIDE {
                checkpoints.push(Checkpoint {
                    offset: offset as u32,
                    line: position.line as u32,
                    column: position.column as u32,
                });
            }
            advance(&mut position, ch);
        }
        Self {
            text: text.into(),
            checkpoints,
        }
    }

    pub fn position(&self, offset: u32) -> Position {
        let offset = boundary(&self.text, offset as usize);
        let checkpoint = &self.checkpoints[self
            .checkpoints
            .partition_point(|p| p.offset as usize <= offset)
            - 1];
        let mut position = Position {
            line: checkpoint.line as usize,
            column: checkpoint.column as usize,
        };
        for ch in self.text[checkpoint.offset as usize..offset].chars() {
            advance(&mut position, ch);
        }
        position
    }

    pub fn frame(&self, offset: u32) -> String {
        frame(
            &self.text,
            boundary(&self.text, offset as usize),
            self.position(offset),
        )
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

fn frame(text: &str, offset: usize, position: Position) -> String {
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
    let prefix = start > 0 && text.as_bytes()[start - 1] != b'\n';
    let suffix = end < text.len() && text.as_bytes()[end] != b'\n';
    let mut caret = String::new();
    if prefix {
        caret.push_str("   ");
    }
    caret.extend(
        text[start..offset]
            .chars()
            .map(|ch| if ch == '\t' { '\t' } else { ' ' }),
    );
    let label = position.line.to_string();
    format!(
        "  --> line {}, column {}\n {} | {}{}{}\n {} | {}^",
        position.line,
        position.column,
        label,
        if prefix { "..." } else { "" },
        &text[start..end],
        if suffix { "..." } else { "" },
        " ".repeat(label.len()),
        caret
    )
}

pub(crate) fn parse_error(source: &str, mut error: Error) -> Error {
    if let Some(offset) = error
        .offset
        .filter(|_| source.len() <= crate::syntax::MAX_SOURCE)
    {
        let offset = boundary(source, offset);
        let mut position = Position { line: 1, column: 1 };
        for ch in source[..offset].chars() {
            advance(&mut position, ch);
        }
        error.diagnostic = Some(Arc::new(Diagnostic {
            position,
            code_frame: frame(source, offset, position),
            frames: Vec::new(),
        }));
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;

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
