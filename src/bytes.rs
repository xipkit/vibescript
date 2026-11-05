//! Byte helpers shared across the builtins: a fixed-size text buffer and a
//! metered byte comparison.

use crate::{
    CallContext, Result,
    budget::CHUNK,
    checked::{Access, Checked, violation},
};
use std::fmt::{self, Write};

pub(crate) fn bytes_equal(ctx: &mut CallContext, a: &[u8], b: &[u8]) -> Result<bool> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (a, b) in a.chunks(CHUNK).zip(b.chunks(CHUNK)) {
        ctx.work_bytes(a.len())?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) struct Number {
    buf: [u8; 512],
    len: usize,
}
impl Number {
    pub fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }
    /// Appends text, reporting an internal error if the fixed buffer is full.
    pub fn append(&mut self, text: &str) -> Access<()> {
        self.write_str(text)
            .map_err(|_| violation("numeric text exceeds fixed buffer"))
    }

    /// Formats text, reporting an internal error if the fixed buffer is full.
    pub fn format(&mut self, args: fmt::Arguments<'_>) -> Access<()> {
        self.write_fmt(args)
            .map_err(|_| violation("numeric text exceeds fixed buffer"))
    }

    /// Returns the initialized text, reporting an invalid buffer length.
    pub fn bytes(&self) -> Access<&[u8]> {
        self.buf.span(0, self.len)
    }
}
impl Write for Number {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
        self.buf
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_text_rejects_overflow_without_losing_the_prefix() {
        let mut number = Number::new();
        number.append(&"1".repeat(512)).unwrap();
        assert!(number.append("2").is_err());
        assert_eq!(number.bytes().unwrap(), "1".repeat(512).as_bytes());
        number.len = 513;
        assert!(number.bytes().is_err());
    }
}
