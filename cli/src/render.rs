//! Result and error rendering for the Go-style commands.

use std::fmt::{self, Write as _};
use vibescript::{Error, ErrorKind, StackFrame, Value};

/// The largest result rendering `vibes run` prints, matching the 1 MiB output guards.
pub const MAX_RESULT_BYTES: usize = 1 << 20;

/// Why a result could not be rendered.
pub enum Failure {
    TooLarge,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(
                f,
                "result rendering exceeds {MAX_RESULT_BYTES} bytes; reduce the returned value or stream it from the script"
            ),
        }
    }
}

enum Item<'a> {
    Value(&'a Value),
    Text(&'static str),
}

/// Renders a value as the reference's `Value.String` does: strings and
/// symbols raw, nil empty, floats in Go's shortest `%g` form, arrays as
/// `[a, b]` and hashes as `{key: value}`. Output over [`MAX_RESULT_BYTES`]
/// is refused rather than truncated.
pub fn value(root: &Value) -> Result<Vec<u8>, Failure> {
    let mut out = Vec::new();
    let mut pending = vec![Item::Value(root)];
    while let Some(item) = pending.pop() {
        match item {
            Item::Text(text) => out.extend_from_slice(text.as_bytes()),
            Item::Value(value) => leaf_or_open(value, &mut out, &mut pending),
        }
        if out.len() > MAX_RESULT_BYTES {
            return Err(Failure::TooLarge);
        }
    }
    Ok(out)
}

fn leaf_or_open<'a>(value: &'a Value, out: &mut Vec<u8>, pending: &mut Vec<Item<'a>>) {
    match value.type_name() {
        "nil" => {}
        "string" | "symbol" => out.extend_from_slice(value.as_bytes().unwrap_or_default()),
        "float" => out.extend_from_slice(float(value.as_float().unwrap_or_default()).as_bytes()),
        "array" => {
            let items = value.as_array().unwrap_or_default();
            out.push(b'[');
            pending.push(Item::Text("]"));
            for (index, item) in items.iter().enumerate().rev() {
                pending.push(Item::Value(item));
                if index > 0 {
                    pending.push(Item::Text(", "));
                }
            }
        }
        "hash" if !protected(value) => {
            let entries = value.as_hash().unwrap_or_default();
            out.push(b'{');
            pending.push(Item::Text("}"));
            for (index, (key, item)) in entries.iter().enumerate().rev() {
                pending.push(Item::Value(item));
                pending.push(Item::Text(": "));
                pending.push(Item::Value(key));
                if index > 0 {
                    pending.push(Item::Text(", "));
                }
            }
        }
        _ => {
            let _ = write!(Bounded(out), "{value}");
        }
    }
}

/// Error and match hashes print their own `to_s` entry.
fn protected(value: &Value) -> bool {
    let mut prefix = String::new();
    let _ = write!(Prefix(&mut prefix, 20), "{value:?}");
    prefix.starts_with("Value(Hash(Error") || prefix.starts_with("Value(Hash(Match")
}

/// Collects at most the given number of bytes, then stops the formatter.
struct Prefix<'a>(&'a mut String, usize);

impl fmt::Write for Prefix<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.push_str(text);
        if self.0.len() >= self.1 {
            return Err(fmt::Error);
        }
        Ok(())
    }
}

/// Appends formatted text, stopping just past the result limit.
struct Bounded<'a>(&'a mut Vec<u8>);

impl fmt::Write for Bounded<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0.extend_from_slice(text.as_bytes());
        if self.0.len() > MAX_RESULT_BYTES {
            return Err(fmt::Error);
        }
        Ok(())
    }
}

/// Formats a float as Go's `strconv.FormatFloat(f, 'g', -1, 64)` does, with
/// Ruby's spellings of the special values.
pub fn float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let negative = mantissa.starts_with('-');
    let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if !(-4..6).contains(&exponent) {
        out.push(digits[0] as char);
        if digits.len() > 1 {
            out.push('.');
            out.extend(digits[1..].iter().map(|&d| d as char));
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        let magnitude = exponent.unsigned_abs();
        if magnitude < 10 {
            out.push('0');
        }
        out.push_str(&magnitude.to_string());
        return out;
    }
    let point = exponent + 1;
    let digit = |index: i32| {
        usize::try_from(index)
            .ok()
            .and_then(|index| digits.get(index))
            .map_or('0', |&d| d as char)
    };
    if point > 0 {
        for index in 0..point {
            out.push(digit(index));
        }
    } else {
        out.push('0');
    }
    let fraction = (digits.len() as i32 - point).max(0);
    if fraction > 0 {
        out.push('.');
        for index in point..point + fraction {
            out.push(digit(index));
        }
    }
    out
}

/// Renders an engine error as its `Display` does. For an inline `snippet`,
/// the entrypoint's frames are named `<snippet>` and a parse error that ran
/// out of source reads `unexpected end of snippet` from column 1 or later, as
/// in the reference.
pub fn error(error: &Error, snippet: Option<&str>) -> String {
    let Some(diagnostic) = &error.diagnostic else {
        return error.to_string();
    };
    let mut text = String::new();
    if error.kind == ErrorKind::Syntax {
        let message = if snippet.is_some_and(|snippet| ends_snippet(error, snippet)) {
            "unexpected end of snippet"
        } else {
            &error.message
        };
        // Like the reference, a snippet reports the end of its input no
        // earlier than column 1.
        let mut position = diagnostic.position;
        if snippet.is_some() {
            position.column = position.column.max(1);
        }
        let _ = write!(
            text,
            "parse error at {}: {message}",
            location(diagnostic.filename.as_deref(), position)
        );
    } else {
        text.push_str(&error.message);
    }
    let _ = write!(text, "\n{}", diagnostic.code_frame);
    let count = diagnostic.frames.len();
    for (index, frame) in diagnostic.frames.iter().enumerate() {
        if count > 16 && (8..count - 8).contains(&index) {
            if index == 8 {
                let _ = write!(text, "\n  ... {} frames omitted ...", count - 16);
            }
            continue;
        }
        let _ = write!(
            text,
            "\n  at {} ({})",
            function(frame, snippet.is_some()),
            location(frame.filename.as_deref(), frame.position)
        );
    }
    text
}

/// Whether a snippet parse error reports running out of source.
fn ends_snippet(error: &Error, snippet: &str) -> bool {
    error.message.contains("end of input")
        || error.message.contains("end of source")
        || error.offset == Some(snippet.len())
}

fn function(frame: &StackFrame, snippet: bool) -> &str {
    if snippet && frame.filename.is_none() && &*frame.function == "<script>" {
        "<snippet>"
    } else {
        &frame.function
    }
}

/// Formats `file:line:column`, escaping control characters, backslashes and
/// invalid UTF-8 in the file name, or `line:column` without one.
fn location(filename: Option<&[u8]>, position: vibescript::Position) -> String {
    let mut text = String::new();
    if let Some(name) = filename {
        for chunk in name.utf8_chunks() {
            for ch in chunk.valid().chars() {
                if ch.is_control() || ch == '\\' {
                    text.extend(ch.escape_default());
                } else {
                    text.push(ch);
                }
            }
            for byte in chunk.invalid() {
                let _ = write!(text, "\\x{byte:02x}");
            }
        }
        text.push(':');
    }
    let _ = write!(text, "{}:{}", position.line, position.column);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibescript::{CallOptions, Engine};

    fn rendered(source: &str) -> String {
        let value = Engine::new()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap()
            .value;
        String::from_utf8(value_or_panic(&value)).unwrap()
    }

    fn value_or_panic(value: &Value) -> Vec<u8> {
        match super::value(value) {
            Ok(bytes) => bytes,
            Err(failure) => panic!("{failure}"),
        }
    }

    #[test]
    fn renders_values_as_the_reference_string_form() {
        assert_eq!(
            rendered("[1, nil, \"a\", 1e20, 2.0, :s, {a: nil, \"c d\": [nil]}, 1..2, 0.1 + 0.2]"),
            "[1, , a, 1e+20, 2, s, {a: , c d: []}, 1..2, 0.30000000000000004]"
        );
        assert_eq!(rendered("{}"), "{}");
        assert_eq!(rendered("[]"), "[]");
        assert_eq!(rendered("\"x\\ny\""), "x\ny");
        assert_eq!(rendered("12345678901234567890"), "12345678901234567890");
    }

    #[test]
    fn formats_floats_like_go() {
        for (value, text) in [
            (0.0, "0"),
            (-0.0, "-0"),
            (1.5, "1.5"),
            (2.0, "2"),
            (123456.0, "123456"),
            (1_000_000.0, "1e+06"),
            (1_234_567.5, "1.2345675e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e300, "1.5e+300"),
            (5e-324, "5e-324"),
            (123.456, "123.456"),
            (-0.5, "-0.5"),
            (f64::NAN, "NaN"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(float(value), text, "{value}");
        }
    }

    #[test]
    fn refuses_oversized_results() {
        let value = Value::array(vec![Value::bytes("abcdefgh"); 200_000]);
        assert!(matches!(super::value(&value), Err(Failure::TooLarge)));
        let value = Value::bytes(vec![b'x'; MAX_RESULT_BYTES]);
        assert_eq!(value_or_panic(&value).len(), MAX_RESULT_BYTES);
    }
}
