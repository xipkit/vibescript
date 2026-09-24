//! Canonical formatting of Vibescript source, as `vibes fmt` applies it.
//!
//! The canonical form normalizes `\r\n` and lone `\r` line endings to `\n`,
//! strips trailing spaces and tabs from every line, drops trailing blank
//! lines and ends with exactly one newline. Leading and interior blank lines
//! and all other bytes are kept, so formatting never changes what a script
//! means. The result matches the reference implementation's formatter byte
//! for byte.

/// Returns the canonical form of `source`.
///
/// ```
/// use vibescript_tools::format::format;
/// assert_eq!(format("def run()  \r\n  1\t\n\n\nend"), "def run()\n  1\n\n\nend\n");
/// assert_eq!(format(""), "\n");
/// ```
pub fn format(source: &str) -> String {
    String::from_utf8(format_bytes(source.as_bytes()))
        .expect("formatting only removes ASCII bytes at line ends")
}

/// Returns the canonical form of source bytes, keeping invalid UTF-8 as is.
///
/// ```
/// use vibescript_tools::format::format_bytes;
/// assert_eq!(format_bytes(b"x = \"\xff\"  \r\n"), b"x = \"\xff\"\n");
/// ```
pub fn format_bytes(source: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(source.len() + 1);
    let mut line_start = 0;
    let mut pending_blank_lines = 0;
    let mut wrote = false;
    let mut index = 0;
    while index < source.len() {
        if source[index] != b'\n' && source[index] != b'\r' {
            index += 1;
            continue;
        }
        let line = trim_end(&source[line_start..index]);
        wrote |= append_line(&mut out, line, &mut pending_blank_lines);
        if source[index] == b'\r' && source.get(index + 1) == Some(&b'\n') {
            index += 2;
        } else {
            index += 1;
        }
        line_start = index;
    }
    if line_start < source.len() {
        let line = trim_end(&source[line_start..]);
        wrote |= append_line(&mut out, line, &mut pending_blank_lines);
    }
    if !wrote {
        return b"\n".to_vec();
    }
    out
}

/// Reports whether `source` is already in canonical form.
///
/// ```
/// use vibescript_tools::format::is_formatted;
/// assert!(is_formatted("x = 1\n"));
/// assert!(!is_formatted("x = 1  \n"));
/// ```
pub fn is_formatted(source: &str) -> bool {
    format_bytes(source.as_bytes()) == source.as_bytes()
}

/// Appends a non-empty line after any pending blank lines; returns whether it wrote.
fn append_line(out: &mut Vec<u8>, line: &[u8], pending_blank_lines: &mut usize) -> bool {
    if line.is_empty() {
        *pending_blank_lines += 1;
        return false;
    }
    out.extend(std::iter::repeat_n(b'\n', *pending_blank_lines));
    *pending_blank_lines = 0;
    out.extend_from_slice(line);
    out.push(b'\n');
    true
}

fn trim_end(line: &[u8]) -> &[u8] {
    let end = line
        .iter()
        .rposition(|&byte| byte != b' ' && byte != b'\t')
        .map_or(0, |index| index + 1);
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_line_endings_and_whitespace() {
        assert_eq!(
            format("def run()\r\n  1\t \r  \n\nend\t \n\n"),
            "def run()\n  1\n\n\nend\n"
        );
        assert_eq!(format("def run()  \n  1\t \nend"), "def run()\n  1\nend\n");
        assert_eq!(format("\t \r\n"), "\n");
        assert_eq!(format("\n\nx"), "\n\nx\n");
        assert_eq!(format("# comment\n\n"), "# comment\n");
        assert_eq!(format_bytes(b"a\xff \r\n"), b"a\xff\n");
    }

    /// The reference's formatter fuzz properties, over generated input.
    #[test]
    fn output_is_canonical_and_idempotent() {
        let alphabet = [b' ', b'\t', b'\r', b'\n', b'a', b'#', 0xff];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let len = (state % 24) as usize;
            let source: Vec<u8> = (0..len)
                .map(|i| alphabet[((state >> (i * 2 % 60)) % alphabet.len() as u64) as usize])
                .collect();
            let once = format_bytes(&source);
            assert!(once.ends_with(b"\n"), "{source:?}");
            assert!(!once.contains(&b'\r'), "{source:?}");
            for line in once[..once.len() - 1].split(|&b| b == b'\n') {
                assert!(
                    !line.ends_with(b" ") && !line.ends_with(b"\t"),
                    "{source:?}"
                );
            }
            assert_eq!(format_bytes(&once), once, "{source:?}");
        }
    }
}
