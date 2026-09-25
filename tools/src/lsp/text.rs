//! Lines, UTF-16 positions and words in document text, as the reference counts them.

/// Splits a document the way LSP clients count lines: `\r\n`, bare `\n` and
/// bare `\r` each end a line.
pub(crate) fn split_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                lines.push(text[start..index].to_owned());
                start = index + 1;
            }
            b'\r' => {
                lines.push(text[start..index].to_owned());
                if bytes.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
                start = index + 1;
            }
            _ => (),
        }
        index += 1;
    }
    lines.push(text[start..].to_owned());
    lines
}

/// The protocol position of a byte offset in `text`: its line, counting
/// `\r\n`, `\n` and `\r` as line ends as [`split_lines`] does, and its
/// UTF-16 offset in that line.
pub(crate) fn position_at(text: &str, offset: usize) -> super::Position {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text.as_bytes()[..offset];
    let mut line = 0u32;
    let mut start = 0;
    let mut index = 0;
    while index < before.len() {
        match before[index] {
            b'\n' => {
                line += 1;
                start = index + 1;
            }
            b'\r' => {
                if text.as_bytes().get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
                line += 1;
                start = (index + 1).min(offset);
            }
            _ => (),
        }
        index += 1;
    }
    let character = text[start..offset].encode_utf16().count();
    super::Position {
        line,
        character: u32::try_from(character).unwrap_or(u32::MAX),
    }
}

/// The line at `index`, or an empty line past either end.
pub(crate) fn line_at<S: AsRef<str>>(lines: &[S], index: i64) -> &str {
    usize::try_from(index)
        .ok()
        .and_then(|index| lines.get(index))
        .map_or("", AsRef::as_ref)
}

/// Converts a zero-based character column to UTF-16 code units. Columns past
/// the line count one unit per missing character, so spans at the end of a
/// line still move forward.
pub(crate) fn utf16_character(line: &str, column: usize) -> usize {
    let mut units = 0;
    let mut count = 0;
    for c in line.chars() {
        if count >= column {
            return units;
        }
        units += c.len_utf16();
        count += 1;
    }
    units + (column - count)
}

/// Converts a UTF-16 offset to a character index, rounding a split surrogate
/// pair up and clamping at the line's end.
pub(crate) fn character_index(line: &str, offset: i64) -> usize {
    if offset <= 0 {
        return 0;
    }
    let mut index = 0;
    let mut consumed = 0;
    for c in line.chars() {
        if consumed >= offset {
            break;
        }
        consumed += c.len_utf16() as i64;
        index += 1;
    }
    index
}

/// The characters that make up a word: identifier characters plus `?` and `!`.
pub(crate) fn word_char(c: char) -> bool {
    c == '?' || c == '!' || vibescript::tooling::identifier_char(c)
}

/// The word under a position, together with its line's characters and the
/// word's bounds, so callers can inspect what surrounds it.
pub(crate) fn word_span<S: AsRef<str>>(
    lines: &[S],
    line: i64,
    character: i64,
) -> Option<(Vec<char>, usize, usize)> {
    let text = usize::try_from(line)
        .ok()
        .and_then(|line| lines.get(line))?;
    let text = text.as_ref();
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut cursor = character_index(text, character.max(0)).min(chars.len());
    if cursor == chars.len() {
        cursor -= 1;
    }
    if !word_char(chars[cursor]) {
        if cursor == 0 || !word_char(chars[cursor - 1]) {
            return None;
        }
        cursor -= 1;
    }
    let mut start = cursor;
    while start > 0 && word_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < chars.len() && word_char(chars[end]) {
        end += 1;
    }
    Some((chars, start, end))
}

/// The word under a position, or an empty string.
pub(crate) fn word_at<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> String {
    word_span(lines, line, character)
        .map(|(chars, start, end)| chars[start..end].iter().collect())
        .unwrap_or_default()
}

/// Blanks quoted string literals, including their quotes and escapes, so
/// structural scans do not trip on punctuation inside them. An unterminated
/// literal is blanked to the end.
pub(crate) fn mask_strings(chars: &[char]) -> Vec<char> {
    let mut masked = Vec::with_capacity(chars.len());
    let mut quote = None;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        match quote {
            Some(open) => {
                masked.push(' ');
                if c == '\\' && index + 1 < chars.len() {
                    index += 1;
                    masked.push(' ');
                } else if c == open {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                masked.push(' ');
            }
            None => masked.push(c),
        }
        index += 1;
    }
    masked
}

/// Blanks string literals and then the trailing comment.
pub(crate) fn mask_code(chars: &[char]) -> Vec<char> {
    let mut masked = mask_strings(chars);
    if let Some(comment) = masked.iter().position(|c| *c == '#') {
        masked[comment..].fill(' ');
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_like_clients() {
        for (text, want) in [
            ("a\nb", &["a", "b"][..]),
            ("a\r\nb", &["a", "b"]),
            ("a\rb\r", &["a", "b", ""]),
            ("a\r\nb\rc\n", &["a", "b", "c", ""]),
            ("", &[""]),
        ] {
            assert_eq!(split_lines(text), want, "{text:?}");
        }
    }

    #[test]
    fn positions_byte_offsets_in_utf16() {
        let text = "a\r\n😀b\rc";
        assert_eq!(position_at(text, 0), super::super::Position::new(0, 0));
        assert_eq!(position_at(text, 3), super::super::Position::new(1, 0));
        assert_eq!(position_at(text, 7), super::super::Position::new(1, 2));
        assert_eq!(position_at(text, 9), super::super::Position::new(2, 0));
        assert_eq!(position_at(text, 99), super::super::Position::new(2, 1));
    }

    #[test]
    fn converts_utf16_positions() {
        assert_eq!(utf16_character("😀😀x", 2), 4);
        assert_eq!(utf16_character("ab", 5), 5);
        assert_eq!(character_index("😀😀x y", 4), 2);
        assert_eq!(character_index("😀x", 1), 1);
        assert_eq!(character_index("ab", 9), 2);
        assert_eq!(character_index("ab", -3), 0);
    }

    #[test]
    fn finds_words() {
        let lines = split_lines("def run()\n  to_int(\"1\")\nend\n");
        assert_eq!(word_at(&lines, 1, 4), "to_int");
        assert_eq!(word_at(&split_lines("😀😀x y\n"), 0, 4), "x");
        assert_eq!(word_at(&lines, 1, 8), "to_int");
        assert_eq!(word_at(&lines, 5, 0), "");
        assert_eq!(word_at(&lines, -1, 0), "");
        assert_eq!(word_at(&split_lines("empty? x"), 0, 6), "empty?");
        assert_eq!(
            word_at(&split_lines("Ⅻ"), 0, 0),
            "",
            "Nl is not a letter in Go"
        );
    }

    #[test]
    fn masks_strings_and_comments() {
        let chars: Vec<char> = "f(\"a,)\\\"b\", 'c#', d) # e(".chars().collect();
        let masked: String = mask_code(&chars).into_iter().collect();
        assert_eq!(
            masked,
            format!("f({},{}, d){}", " ".repeat(8), " ".repeat(5), " ".repeat(5))
        );
    }
}
