use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, value::Kind};

mod prepare;
mod project;
mod render;
#[cfg(test)]
mod tests;

const LIMIT: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Function {
    Format,
    Sprintf,
}

impl Function {
    pub fn name(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::Sprintf => "sprintf",
        }
    }

    pub fn validate(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: bool,
        block: bool,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let message = if keywords {
            Some("does not take keyword arguments")
        } else if block {
            Some("does not accept blocks")
        } else if args.is_empty() {
            Some("expects a format string")
        } else if !matches!(args[0].0, Kind::Bytes(_)) {
            Some("expects a string format")
        } else {
            None
        };
        if let Some(message) = message {
            return Err(error(ctx, format_args!("{} {message}", self.name()))?);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Field {
    flags: u8,
    width: Option<usize>,
    precision: Option<usize>,
}

const SHARP: u8 = 1;
const PLUS: u8 = 2;
const SPACE: u8 = 4;
const ZERO: u8 = 8;
const MINUS: u8 = 16;

impl Field {
    fn flag(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    fn take_flag(&mut self, byte: u8) -> bool {
        let flag = match byte {
            b'#' => SHARP,
            b'+' => PLUS,
            b' ' => SPACE,
            b'0' => ZERO,
            b'-' => MINUS,
            _ => return false,
        };
        self.flags |= flag;
        true
    }
}

struct Argument {
    value: Value,
    text: Option<usize>,
}

struct Prepared {
    pattern: Buffer<u8>,
    arguments: Buffer<Argument>,
}

pub(crate) fn format(ctx: &mut CallContext, pattern: &[u8], values: &[Value]) -> Result<Value> {
    ctx.work_bytes(pattern.len())?;
    let prepared = prepare::prepare(ctx, pattern, values)?;
    render::render(ctx, &prepared)
}

fn error(ctx: &mut CallContext, args: std::fmt::Arguments<'_>) -> Result<Error> {
    let (message, _charge) = crate::source::formatted(ctx, args)?;
    Ok(Error::new(ErrorKind::Argument, message))
}

fn limit<T>(ctx: &mut CallContext, label: &str) -> Result<T> {
    ctx.guard(
        ErrorKind::OutputLimit,
        &format!("format {label} exceeds limit {LIMIT} bytes"),
    )
}

fn integer(value: &Value) -> Option<i64> {
    match value.0 {
        Kind::Int(n) => Some(n),
        Kind::Float(n)
            if n.is_finite() && (-9223372036854775808.0..9223372036854775808.0).contains(&n) =>
        {
            Some(n as i64)
        }
        _ => None,
    }
}

fn float(value: &Value) -> f64 {
    match &value.0 {
        Kind::Int(n) => *n as f64,
        Kind::Big(n) => n.to_float(),
        Kind::Float(n) => *n,
        _ => unreachable!(),
    }
}

fn string_like(value: &Value) -> bool {
    !matches!(
        value.0,
        Kind::Nil | Kind::Bool(_) | Kind::Int(_) | Kind::Big(_) | Kind::Float(_)
    )
}

fn precision_bytes(ctx: &mut CallContext, bytes: &[u8], count: usize) -> Result<usize> {
    let mut offset = 0;
    for _ in 0..count {
        if offset == bytes.len() {
            break;
        }
        ctx.charge(1)?;
        offset += crate::scan::rune(&bytes[offset..]).1;
    }
    Ok(offset)
}
