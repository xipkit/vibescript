use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK},
    iteration::Progress,
    scan,
    value::{Bytes, Heap, Kind},
};

#[derive(Clone, Copy)]
enum Method {
    Char,
    Byte,
    Codepoint,
    Line,
}

impl Method {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "each_char" => Some(Self::Char),
            "each_byte" => Some(Self::Byte),
            "each_codepoint" => Some(Self::Codepoint),
            "each_line" => Some(Self::Line),
            _ => None,
        }
    }
}

pub(crate) fn method(name: &str) -> bool {
    Method::parse(name).is_some()
}

pub(crate) struct Driver {
    method: Method,
    receiver: Value,
    position: usize,
    pub waiting: bool,
}

impl Driver {
    pub fn new(
        ctx: &mut CallContext,
        name: &str,
        receiver: &Value,
        args: &[Value],
        keywords: bool,
        block: bool,
    ) -> Result<Option<Self>> {
        let Some(method) = Method::parse(name) else {
            return Ok(None);
        };
        if !matches!(receiver.0, Kind::Bytes(_)) {
            return Ok(None);
        }
        ctx.checkpoint()?;
        if !args.is_empty() || keywords {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} does not take arguments"),
            ));
        }
        if !block {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} requires a block"),
            ));
        }
        Ok(Some(Self {
            method,
            receiver: receiver.clone(),
            position: 0,
            waiting: false,
        }))
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        // A discarded callback result must be released before allocating the next element.
        drop(returned);
        self.waiting = false;
        ctx.charge(1)?;
        ctx.checkpoint()?;
        let bytes = self.receiver.require_bytes()?;
        if self.position == bytes.len() {
            return Ok(Progress::Done(self.receiver.clone()));
        }
        let value = match self.method {
            Method::Byte => {
                let value = Value::int(i64::from(bytes[self.position]));
                self.position += 1;
                value
            }
            Method::Char | Method::Codepoint => {
                let (rune, width, _) = scan::rune(&bytes[self.position..]);
                self.position += width;
                if matches!(self.method, Method::Char) {
                    let mut encoded = [0; 4];
                    ctx.bytes(rune.encode_utf8(&mut encoded).as_bytes())?
                } else {
                    Value::int(i64::from(u32::from(rune)))
                }
            }
            Method::Line => {
                let end = line_end(ctx, bytes, self.position)?;
                let value = line(ctx, &self.receiver, self.position, end)?;
                self.position = end;
                value
            }
        };
        self.waiting = true;
        Ok(Progress::Yield([value, Value::nil(), Value::nil()], 1))
    }
}

fn line_end(ctx: &mut CallContext, bytes: &[u8], mut position: usize) -> Result<usize> {
    while position < bytes.len() {
        ctx.checkpoint()?;
        let end = position + (bytes.len() - position).min(CHUNK);
        if let Some(offset) = bytes[position..end].iter().position(|&byte| byte == b'\n') {
            ctx.work_bytes(offset + 1)?;
            return Ok(position + offset + 1);
        }
        ctx.work_bytes(end - position)?;
        position = end;
    }
    Ok(position)
}

fn line(ctx: &mut CallContext, receiver: &Value, start: usize, end: usize) -> Result<Value> {
    let bytes = receiver.require_bytes()?;
    if start == 0 && end == bytes.len() {
        Ok(receiver.clone())
    } else {
        ctx.bytes(&bytes[start..end])
    }
}

pub(crate) fn lines(ctx: &mut CallContext, receiver: &Value) -> Result<Value> {
    let bytes = receiver.require_bytes()?;
    let mut projected = Heap::<Value>::header_bytes();
    ctx.check_memory(projected)?;
    let mut position = 0;
    let mut count = 0;
    while position < bytes.len() {
        ctx.charge(1)?;
        let end = line_end(ctx, bytes, position)?;
        let copy = if position == 0 && end == bytes.len() {
            0
        } else {
            Bytes::header_bytes().saturating_add(end - position)
        };
        let Some(next) = projected
            .checked_add(copy)
            .and_then(|n| n.checked_add(size_of::<Value>()))
        else {
            return ctx.fail(ErrorKind::Memory, "lines output size overflow");
        };
        ctx.check_memory(next)?;
        projected = next;
        count += 1;
        position = end;
    }
    let mut output = Buffer::with_capacity(ctx, count)?;
    position = 0;
    while position < bytes.len() {
        ctx.charge(1)?;
        let end = line_end(ctx, bytes, position)?;
        output.data.push(line(ctx, receiver, position, end)?);
        position = end;
    }
    Value::from_array(ctx, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn lines_reserve_all_storage_once_and_reuse_a_whole_line() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(b"a\r\nb\xff").unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        let storage =
            Heap::<Value>::header_bytes() + 2 * (size_of::<Value>() + Bytes::header_bytes()) + 5;
        ctx.options.limits.memory_bytes = Some(baseline + storage);
        let output = lines(&mut ctx, &input).unwrap();
        let rows = output.as_array().unwrap();
        assert_eq!(rows[0].as_bytes().unwrap(), b"a\r\n");
        assert_eq!(rows[1].as_bytes().unwrap(), b"b\xff");
        assert_eq!(ctx.stats().retained_memory_bytes, baseline + storage);
        assert_eq!(ctx.stats().peak_memory_bytes, baseline + storage);
        drop(output);
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);

        for bytes in [b"".as_slice(), b"tail", b"line\n"] {
            let mut ctx = CallContext::new(CallOptions::default());
            let input = ctx.bytes(bytes).unwrap();
            let baseline = ctx.stats().retained_memory_bytes;
            let slots = usize::from(!bytes.is_empty());
            let storage = Heap::<Value>::header_bytes() + slots * size_of::<Value>();
            ctx.options.limits.memory_bytes = Some(baseline + storage);
            let output = lines(&mut ctx, &input).unwrap();
            assert_eq!(output.as_array().unwrap().len(), slots);
            if slots != 0 {
                assert_eq!(
                    output.as_array().unwrap()[0].as_bytes().unwrap().as_ptr(),
                    input.as_bytes().unwrap().as_ptr()
                );
            }
            assert_eq!(ctx.stats().retained_memory_bytes, baseline + storage);
        }
    }

    #[test]
    fn line_projection_rejects_expansion_before_allocating() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(&vec![b'\n'; 8192]).unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 16384);
        assert_eq!(lines(&mut ctx, &input).unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        assert!(ctx.stats().steps - baseline.steps < 1000);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn discarded_callback_results_are_released_before_the_next_line() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(b"aaa\nbbb\n").unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        let mut driver = Driver::new(&mut ctx, "each_line", &input, &[], false, true)
            .unwrap()
            .unwrap();
        ctx.options.limits.memory_bytes = Some(baseline + Bytes::header_bytes() + 4);
        let Progress::Yield([first, _, _], 1) = driver.advance(&mut ctx, None).unwrap() else {
            panic!()
        };
        assert_eq!(first.as_bytes().unwrap(), b"aaa\n");
        let Progress::Yield([second, _, _], 1) = driver.advance(&mut ctx, Some(first)).unwrap()
        else {
            panic!()
        };
        assert_eq!(second.as_bytes().unwrap(), b"bbb\n");
        assert!(matches!(
            driver.advance(&mut ctx, Some(second)).unwrap(),
            Progress::Done(_)
        ));
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        assert_eq!(
            ctx.stats().peak_memory_bytes,
            baseline + Bytes::header_bytes() + 4
        );
    }

    #[test]
    fn a_retained_line_counts_against_the_next_yield() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(b"aaa\nbbb\n").unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        let mut driver = Driver::new(&mut ctx, "each_line", &input, &[], false, true)
            .unwrap()
            .unwrap();
        ctx.options.limits.memory_bytes = Some(baseline + Bytes::header_bytes() + 4);
        let Progress::Yield([first, _, _], 1) = driver.advance(&mut ctx, None).unwrap() else {
            panic!()
        };
        let error = driver.advance(&mut ctx, Some(Value::nil())).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline + Bytes::header_bytes() + 4
        );
        drop(first);
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn long_line_scans_observe_work_limits_before_allocating() {
        let mut ctx = CallContext::new(CallOptions::default());
        let input = ctx.bytes(&vec![b'a'; 1 << 20]).unwrap();
        let mut driver = Driver::new(&mut ctx, "each_line", &input, &[], false, true)
            .unwrap()
            .unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.steps = Some(baseline.steps + 10);
        let error = driver.advance(&mut ctx, None).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Steps);
    }
}
