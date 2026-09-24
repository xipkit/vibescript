//! Content-Length framing for JSON-RPC over byte streams.

use std::io::{self, BufRead, Read, Write};

/// The largest message body the server reads, as in the reference.
pub(crate) const MAX_PAYLOAD: usize = 8 << 20;

/// Header blocks larger than this are treated as corrupt framing.
const MAX_HEADERS: usize = 64 << 10;

/// One framed input.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    Message(Vec<u8>),
    /// A body over [`MAX_PAYLOAD`], discarded without buffering.
    Oversized(usize),
    /// The input ended between messages.
    End,
}

/// Reads one framed message.
///
/// Header names match case-insensitively, lines without a colon are ignored,
/// and the last `Content-Length` wins. Input that ends inside the headers is
/// a clean end; input that ends inside a body is an error.
pub(crate) fn read(reader: &mut impl BufRead) -> io::Result<Frame> {
    let mut length: Option<i64> = None;
    let mut consumed = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .by_ref()
            .take((MAX_HEADERS + 1 - consumed) as u64)
            .read_until(b'\n', &mut line)?;
        consumed += read;
        if read == 0 || !line.ends_with(b"\n") {
            if consumed > MAX_HEADERS {
                return Err(invalid("header block exceeds 64 KiB"));
            }
            return Ok(Frame::End);
        }
        while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
            line.pop();
        }
        if line.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let Some((name, value)) = text.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            let value = value.trim();
            length = Some(value.parse().map_err(|error: std::num::ParseIntError| {
                // The reference's words, from Go's strconv.Atoi.
                let reason = match error.kind() {
                    std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => {
                        "value out of range"
                    }
                    _ => "invalid syntax",
                };
                invalid(&format!(
                    "invalid Content-Length: strconv.Atoi: parsing {}: {reason}",
                    go_quote(value)
                ))
            })?);
        }
    }
    let length = match length {
        Some(length) if length >= 0 => length as u64,
        _ => return Err(invalid("missing Content-Length header")),
    };
    if length > MAX_PAYLOAD as u64 {
        let skipped = io::copy(&mut reader.by_ref().take(length), &mut io::sink())?;
        if skipped < length {
            return Err(truncated());
        }
        return Ok(Frame::Oversized(length as usize));
    }
    let mut body = vec![0; length as usize];
    reader.read_exact(&mut body).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            truncated()
        } else {
            error
        }
    })?;
    Ok(Frame::Message(body))
}

/// Writes one framed message and flushes it.
pub(crate) fn write(writer: &mut impl Write, body: &str) -> io::Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

/// Quotes text as Go's `strconv.Quote` does for the characters headers hold.
fn go_quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let code = c as u32;
                if code < 0x80 {
                    out.push_str(&format!("\\x{code:02x}"));
                } else {
                    out.push_str(&format!("\\u{code:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

fn truncated() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "read payload body: unexpected EOF",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(input: &[u8]) -> Vec<io::Result<Frame>> {
        let mut reader = io::BufReader::new(input);
        let mut out = Vec::new();
        loop {
            let frame = read(&mut reader);
            let done = !matches!(frame, Ok(Frame::Message(_) | Frame::Oversized(_)));
            out.push(frame);
            if done {
                return out;
            }
        }
    }

    #[test]
    fn reads_headers_like_the_reference() {
        let input =
            b"X-Other: 1\r\ncontent-length:  2 \r\nnot a header\r\n\r\n{}Content-Length: 3\n\n[1]";
        let frames = frames(input);
        assert_eq!(frames[0].as_ref().unwrap(), &Frame::Message(b"{}".to_vec()));
        assert_eq!(
            frames[1].as_ref().unwrap(),
            &Frame::Message(b"[1]".to_vec())
        );
        assert_eq!(frames[2].as_ref().unwrap(), &Frame::End);
        // Input ending inside the headers is a clean end.
        assert_eq!(frames_end(b"Content-Length: 5\r\n"), Frame::End);
        assert_eq!(frames_end(b""), Frame::End);
    }

    fn frames_end(input: &[u8]) -> Frame {
        frames(input).pop().unwrap().unwrap()
    }

    #[test]
    fn rejects_corrupt_framing() {
        for (input, message) in [
            (&b"\r\n{}"[..], "missing Content-Length header"),
            (
                b"Content-Length: -1\r\n\r\n",
                "missing Content-Length header",
            ),
            (
                b"Content-Length: x\r\n\r\n",
                "invalid Content-Length: strconv.Atoi: parsing \"x\": invalid syntax",
            ),
            (
                b"Content-Length: \x01\r\n\r\n",
                "invalid Content-Length: strconv.Atoi: parsing \"\\x01\": invalid syntax",
            ),
            (
                b"Content-Length: 99999999999999999999\r\n\r\n",
                "invalid Content-Length: strconv.Atoi: parsing \"99999999999999999999\": value out of range",
            ),
            (
                b"Content-Length: 4\r\n\r\n{}",
                "read payload body: unexpected EOF",
            ),
        ] {
            let error = frames(input).pop().unwrap().unwrap_err();
            assert_eq!(error.to_string(), message);
        }
        let mut huge = b"X: ".to_vec();
        huge.resize(MAX_HEADERS + 10, b'a');
        let error = frames(&huge).pop().unwrap().unwrap_err();
        assert_eq!(error.to_string(), "header block exceeds 64 KiB");
    }

    #[test]
    fn skips_oversized_bodies_without_losing_the_stream() {
        let mut input = format!("Content-Length: {}\r\n\r\n", MAX_PAYLOAD + 1).into_bytes();
        input.resize(input.len() + MAX_PAYLOAD + 1, b' ');
        input.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
        let frames = frames(&input);
        assert_eq!(
            frames[0].as_ref().unwrap(),
            &Frame::Oversized(MAX_PAYLOAD + 1)
        );
        assert_eq!(frames[1].as_ref().unwrap(), &Frame::Message(b"{}".to_vec()));
        // A body exactly at the limit is read.
        let mut exact = format!("Content-Length: {MAX_PAYLOAD}\r\n\r\n").into_bytes();
        exact.resize(exact.len() + MAX_PAYLOAD, b' ');
        assert!(
            matches!(&self::frames(&exact)[0], Ok(Frame::Message(body)) if body.len() == MAX_PAYLOAD)
        );
    }

    #[test]
    fn writes_content_length_frames() {
        let mut out = Vec::new();
        write(&mut out, "{\"a\":\"é\"}").unwrap();
        assert_eq!(out, "Content-Length: 10\r\n\r\n{\"a\":\"é\"}".as_bytes());
    }
}
