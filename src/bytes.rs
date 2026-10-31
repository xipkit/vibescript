//! Byte helpers shared across the builtins: a fixed-size text buffer and a
//! metered byte comparison.

use crate::{CallContext, Result, budget::CHUNK};
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
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}
impl Write for Number {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}
