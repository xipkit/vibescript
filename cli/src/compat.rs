//! Text and path conventions of the Go reference CLI.
//!
//! Error messages quote values with Go's `%q`, name paths as `filepath.Abs`
//! spells them, and describe operating-system errors with Go's lowercase
//! `op path: reason` form, so the two command lines report failures alike.

use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
};

/// The raw bytes of an argument or path. Non-Unix platforms use lossy UTF-8.
pub fn bytes(text: &OsStr) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Cow::Borrowed(text.as_bytes())
    }
    #[cfg(target_os = "wasi")]
    {
        use std::os::wasi::ffi::OsStrExt;
        Cow::Borrowed(text.as_bytes())
    }
    #[cfg(not(any(unix, target_os = "wasi")))]
    {
        match text.to_string_lossy() {
            Cow::Borrowed(text) => Cow::Borrowed(text.as_bytes()),
            Cow::Owned(text) => Cow::Owned(text.into_bytes()),
        }
    }
}

/// Rebuilds an argument from bytes taken from [`bytes`].
pub fn os_string(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        OsStr::from_bytes(bytes).to_os_string()
    }
    #[cfg(target_os = "wasi")]
    {
        use std::os::wasi::ffi::OsStrExt;
        OsStr::from_bytes(bytes).to_os_string()
    }
    #[cfg(not(any(unix, target_os = "wasi")))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Quotes bytes as Go's `strconv.Quote` does: printable runes stay literal,
/// and escapes cover quotes, backslashes, control runes and invalid UTF-8.
pub fn quote(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('"');
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\u{7}' => out.push_str("\\a"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{b}' => out.push_str("\\v"),
                ch if printable(ch) => out.push(ch),
                ch if (ch as u32) < 0x80 => out.push_str(&format!("\\x{:02x}", ch as u32)),
                ch if (ch as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", ch as u32)),
                ch => out.push_str(&format!("\\U{:08x}", ch as u32)),
            }
        }
        for byte in chunk.invalid() {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out.push('"');
    out
}

/// Approximates Go's `strconv.IsPrint`: graphic runes and the ASCII space.
fn printable(ch: char) -> bool {
    if ch == ' ' {
        return true;
    }
    !(ch.is_control()
        || ch.is_whitespace()
        || matches!(ch as u32,
            0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x180e
            | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x206f
            | 0xfeff | 0xfff9..=0xfffb | 0xe000..=0xf8ff
            | 0xf0000..=0x10ffff))
}

/// Parses a boolean flag value as Go's `strconv.ParseBool` does.
pub fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Why an integer flag value was rejected, in Go's wording.
#[derive(Debug, Eq, PartialEq)]
pub enum IntError {
    Syntax,
    Range,
}

impl IntError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Syntax => "parse error",
            Self::Range => "value out of range",
        }
    }
}

/// Parses a 64-bit integer as Go's `strconv.ParseInt(text, 0, 64)` does:
/// an optional sign, a `0x`, `0o`, `0b` or leading-zero octal prefix, and
/// underscores between digits.
pub fn parse_int(text: &str) -> Result<i64, IntError> {
    let (negative, unsigned) = match text.as_bytes().first() {
        None => return Err(IntError::Syntax),
        Some(b'+') => (false, &text[1..]),
        Some(b'-') => (true, &text[1..]),
        _ => (false, text),
    };
    let magnitude = parse_uint(unsigned)?;
    if negative {
        if magnitude > 1 << 63 {
            return Err(IntError::Range);
        }
        Ok((magnitude as i64).wrapping_neg())
    } else {
        i64::try_from(magnitude).map_err(|_| IntError::Range)
    }
}

/// Parses an unsigned integer as Go's `strconv.ParseUint(text, 0, 64)` does.
pub fn parse_uint(text: &str) -> Result<u64, IntError> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Err(IntError::Syntax);
    }
    let (base, digits): (u64, &[u8]) = match bytes {
        [b'0', b'b' | b'B', _, ..] => (2, &bytes[2..]),
        [b'0', b'o' | b'O', _, ..] => (8, &bytes[2..]),
        [b'0', b'x' | b'X', _, ..] => (16, &bytes[2..]),
        [b'0', ..] => (8, &bytes[1..]),
        _ => (10, bytes),
    };
    let mut value: u64 = 0;
    let mut underscores = false;
    for &byte in digits {
        let digit = match byte {
            b'_' => {
                underscores = true;
                continue;
            }
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'z' => byte - b'a' + 10,
            b'A'..=b'Z' => byte - b'A' + 10,
            _ => return Err(IntError::Syntax),
        };
        if u64::from(digit) >= base {
            return Err(IntError::Syntax);
        }
        value = value
            .checked_mul(base)
            .and_then(|v| v.checked_add(u64::from(digit)))
            .ok_or(IntError::Range)?;
    }
    if underscores && !underscores_ok(bytes) {
        return Err(IntError::Syntax);
    }
    Ok(value)
}

/// Mirrors Go's `underscoreOK`: underscores only separate digits, and may
/// follow a base prefix.
fn underscores_ok(text: &[u8]) -> bool {
    // The previous character class: '^' start, '0' digit or prefix, '_' underscore, '!' other.
    let mut saw = b'^';
    let mut i = 0;
    let mut hex = false;
    if text.len() >= 2
        && text[0] == b'0'
        && matches!(text[1].to_ascii_lowercase(), b'b' | b'o' | b'x')
    {
        i = 2;
        saw = b'0';
        hex = text[1].eq_ignore_ascii_case(&b'x');
    }
    for &byte in &text[i..] {
        if byte.is_ascii_digit() || (hex && matches!(byte.to_ascii_lowercase(), b'a'..=b'f')) {
            saw = b'0';
        } else if byte == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
        } else {
            if saw == b'_' {
                return false;
            }
            saw = b'!';
        }
    }
    saw != b'_'
}

/// Trims leading and trailing whitespace as Go's `strings.TrimSpace` does.
pub fn trim_space(text: &str) -> &str {
    text.trim_matches(char::is_whitespace)
}

/// Returns the absolute, lexically cleaned form of `path`, as Go's `filepath.Abs` does.
pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(clean(path));
    }
    Ok(clean(&working_directory()?.join(path)))
}

/// Returns the working directory as Go's `os.Getwd` does: `$PWD` when it is
/// an absolute path to the same directory, so links in it are kept.
pub fn working_directory() -> io::Result<PathBuf> {
    let current = std::env::current_dir()?;
    #[cfg(unix)]
    if let Some(pwd) = std::env::var_os("PWD").map(PathBuf::from) {
        use std::os::unix::fs::MetadataExt;
        if pwd.is_absolute() {
            if let (Ok(named), Ok(actual)) = (std::fs::metadata(&pwd), std::fs::metadata(".")) {
                if named.dev() == actual.dev() && named.ino() == actual.ino() {
                    return Ok(pwd);
                }
            }
        }
    }
    Ok(current)
}

/// Lexically cleans a path as Go's `filepath.Clean` does: repeated separators,
/// `.` elements and `..` elements after a directory are removed.
pub fn clean(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(component),
            },
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

/// Describes an operating-system error in Go's lowercase wording, such as
/// `no such file or directory`, without Rust's `(os error N)` suffix.
pub fn reason(error: &io::Error) -> String {
    let text = error.to_string();
    let text = match (error.raw_os_error(), text.rfind(" (os error ")) {
        (Some(_), Some(index)) => &text[..index],
        _ => &text,
    };
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if error.raw_os_error().is_some() => {
            first.to_lowercase().chain(chars).collect()
        }
        _ => text.to_owned(),
    }
}

/// Formats a failed path operation as Go's `*fs.PathError` does: `op path: reason`.
pub fn path_error(op: &str, path: &Path, error: &io::Error) -> String {
    format!("{op} {}: {}", path.display(), reason(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_like_go() {
        assert_eq!(quote(b"unknown"), "\"unknown\"");
        assert_eq!(quote(b"a\tb"), "\"a\\tb\"");
        assert_eq!(quote(b"run\n"), "\"run\\n\"");
        assert_eq!(quote(b"\xff"), "\"\\xff\"");
        assert_eq!(quote("é\"\\".as_bytes()), "\"é\\\"\\\\\"");
        assert_eq!(quote(b"\x00\x7f"), "\"\\x00\\x7f\"");
        assert_eq!(quote("\u{a0}\u{2028}".as_bytes()), "\"\\u00a0\\u2028\"");
    }

    #[test]
    fn parses_integers_like_go() {
        for (text, value) in [
            ("0", 0),
            ("-1", -1),
            ("+7", 7),
            ("0x10", 16),
            ("0X_1f", 31),
            ("0o17", 15),
            ("017", 15),
            ("0b101", 5),
            ("1_000", 1000),
            ("0_7", 7),
            ("00", 0),
            ("-9223372036854775808", i64::MIN),
            ("9223372036854775807", i64::MAX),
        ] {
            assert_eq!(parse_int(text), Ok(value), "{text}");
        }
        for text in [
            "", "-", "x", "1_", "_1", "1__0", "0x", "08", "1.5", "0x_", " 1", "0_",
        ] {
            assert_eq!(parse_int(text), Err(IntError::Syntax), "{text}");
        }
        for text in [
            "9223372036854775808",
            "-9223372036854775809",
            "99999999999999999999",
        ] {
            assert_eq!(parse_int(text), Err(IntError::Range), "{text}");
        }
        assert_eq!(parse_uint("-1"), Err(IntError::Syntax));
    }

    #[test]
    fn cleans_paths_like_go() {
        for (path, cleaned) in [
            ("/a/b/../c", "/a/c"),
            ("/a/./b/", "/a/b"),
            ("/../a", "/a"),
            ("a/../..", ".."),
            ("", "."),
            ("a//b", "a/b"),
        ] {
            assert_eq!(clean(Path::new(path)), PathBuf::from(cleaned), "{path}");
        }
    }

    #[test]
    fn describes_os_errors_like_go() {
        // A real failure, since error numbers differ between platforms.
        let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("missing-for-test");
        let error = std::fs::metadata(&missing).unwrap_err();
        assert_eq!(reason(&error), "no such file or directory");
        assert_eq!(
            path_error("open", Path::new("/x"), &error),
            "open /x: no such file or directory"
        );
    }
}
