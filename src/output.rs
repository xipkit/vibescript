use crate::{CallContext, Error, ErrorKind, Result, Value, text, value::Kind as ValueKind};
use std::sync::Arc;

pub(crate) type Writer = Arc<dyn Fn(&mut CallContext, &[u8]) -> Result<()> + Send + Sync>;

const LIMIT: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Puts,
    Print,
    Warn,
    Inspect,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Puts => "puts",
            Self::Print => "print",
            Self::Warn => "warn",
            Self::Inspect => "p",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "puts" => Some(Self::Puts),
            "print" => Some(Self::Print),
            "warn" => Some(Self::Warn),
            "p" => Some(Self::Inspect),
            _ => None,
        }
    }

    pub fn validate(self, ctx: &mut CallContext, keywords: bool, block: bool) -> Result<()> {
        ctx.checkpoint()?;
        let error = if keywords {
            Some("does not accept keyword arguments")
        } else if block {
            Some("does not accept blocks")
        } else {
            None
        };
        if let Some(message) = error {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{} {message}", self.name()),
            ));
        }
        self.writer(ctx)?;
        Ok(())
    }

    fn writer(self, ctx: &CallContext) -> Result<&Writer> {
        let (writer, label) = if self == Self::Warn {
            (&ctx.error_writer, "error")
        } else {
            (&ctx.output_writer, "output")
        };
        writer.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::Runtime,
                format!("{} {label} writer is not configured", self.name()),
            )
        })
    }

    pub fn write(self, ctx: &mut CallContext, value: &Value) -> Result<()> {
        let result = if self == Self::Inspect {
            text::inspect::output(ctx, value, LIMIT)
        } else {
            if self == Self::Print {
                if let ValueKind::Bytes(bytes) = &value.0 {
                    if bytes.data.len() > LIMIT {
                        return ctx.guard(ErrorKind::OutputLimit, &self.limit_message());
                    }
                    ctx.work_bytes(bytes.data.len())?;
                    return self.write_bytes(ctx, &bytes.data);
                }
            }
            text::bounded::output(ctx, value, LIMIT, self != Self::Print)
        };
        let bytes = result.map_err(|mut error| {
            if error.kind == ErrorKind::OutputLimit {
                error.message = self.limit_message();
            }
            error
        })?;
        self.write_bytes(ctx, &bytes.data)
    }

    fn limit_message(self) -> String {
        format!("{} output exceeds limit {LIMIT} bytes", self.name())
    }

    pub fn write_empty(self, ctx: &mut CallContext) -> Result<()> {
        if self == Self::Puts {
            self.write_bytes(ctx, b"\n")?;
        }
        Ok(())
    }

    fn write_bytes(self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        let writer = self.writer(ctx)?.clone();
        ctx.checkpoint()?;
        let result = writer(ctx, bytes);
        ctx.checkpoint()?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, budget::Buffer};

    #[test]
    fn deadline_expiry_in_a_writer_wins_over_its_replacement_error() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.output_writer = Some(Arc::new(|ctx, _| {
            ctx.options.deadline = Some(std::time::Instant::now());
            Err(Error::new(ErrorKind::Host, "hidden deadline"))
        }));
        assert_eq!(
            Kind::Puts.write_empty(&mut ctx).unwrap_err().kind,
            ErrorKind::Deadline
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Deadline);
    }

    #[test]
    fn expanded_values_are_refused_before_allocating_output() {
        for kind in [Kind::Puts, Kind::Print, Kind::Warn, Kind::Inspect] {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut value = ctx.bytes(&vec![b'x'; 16384]).unwrap();
            for _ in 0..7 {
                let mut children = Buffer::with_capacity(&mut ctx, 2).unwrap();
                children.data.extend([value.clone(), value]);
                value = Value::from_array(&mut ctx, children).unwrap();
            }
            let baseline = ctx.stats();
            let error = kind.write(&mut ctx, &value).unwrap_err();
            assert_eq!(error.kind, ErrorKind::OutputLimit);
            assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                baseline.retained_memory_bytes
            );
            ctx.checkpoint().unwrap();
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn projection_fails_memory_before_allocating_and_keeps_exhaustion_latched() {
        for kind in [Kind::Puts, Kind::Warn, Kind::Inspect] {
            let mut ctx = CallContext::new(CallOptions::default());
            let value = ctx.bytes(&vec![b'x'; 16384]).unwrap();
            let baseline = ctx.stats();
            ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 1024);
            assert_eq!(
                kind.write(&mut ctx, &value).unwrap_err().kind,
                ErrorKind::Memory
            );
            assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn payload_scans_exhaust_work_before_invoking_writer() {
        for kind in [Kind::Puts, Kind::Print, Kind::Warn, Kind::Inspect] {
            let mut ctx = CallContext::new(CallOptions::default());
            let writer: Writer = Arc::new(|_, _| panic!("writer called after scan exhaustion"));
            ctx.output_writer = Some(writer.clone());
            ctx.error_writer = Some(writer);
            let value = ctx.bytes(&vec![b'x'; 16384]).unwrap();
            ctx.options.limits.steps = Some(ctx.stats().steps + 1);
            assert_eq!(
                kind.write(&mut ctx, &value).unwrap_err().kind,
                ErrorKind::Steps
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
        }
    }
}
