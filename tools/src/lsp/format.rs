//! The canonical source formatter shared with the reference's `vibes fmt`.

/// Formats source the way the reference's `vibes fmt` does: trailing spaces
/// and tabs are removed from every line, line endings become `\n`, trailing
/// blank lines are dropped, and the text ends with exactly one newline.
pub(crate) fn format_source(source: &str) -> String {
    let mut out = String::with_capacity(source.len() + 1);
    let mut blank_lines = 0;
    let mut wrote = false;
    let mut append = |line: &str| {
        let line = line.trim_end_matches([' ', '\t']);
        if line.is_empty() {
            blank_lines += 1;
            return;
        }
        for _ in 0..blank_lines {
            out.push('\n');
        }
        blank_lines = 0;
        out.push_str(line);
        out.push('\n');
        wrote = true;
    };
    let bytes = source.as_bytes();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                append(&source[start..index]);
                index += 1;
                start = index;
            }
            b'\r' => {
                append(&source[start..index]);
                index += if bytes.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < source.len() {
        append(&source[start..]);
    }
    if !wrote {
        return "\n".to_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_the_reference() {
        for (source, formatted) in [
            ("def run()  \n  1\t\nend", "def run()\n  1\nend\n"),
            ("a\rb\r", "a\nb\n"),
            ("\n\nx\n\n\ny\n\n\n", "\n\nx\n\n\ny\n"),
            ("", "\n"),
            (" \t\n\n", "\n"),
            ("x\r\n\r\ny", "x\n\ny\n"),
        ] {
            assert_eq!(format_source(source), formatted, "{source:?}");
        }
    }
}
