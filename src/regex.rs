use crate::{CallContext, Error, ErrorKind, Result, Value, scan, value::Kind};
use program::Program;
use search::Search;

pub(crate) mod matches;
pub(crate) mod operations;
mod parse;
mod program;
mod search;
pub(crate) mod substitute;
mod unicode;
mod utility;
pub(crate) mod value;

pub(crate) use utility::Utility;

pub(crate) const MAX_PATTERN: usize = 16 << 10;
pub(crate) const MAX_TEXT: usize = 1 << 20;
const MAX_INSTRUCTIONS: usize = 100_000;

/// Refuses a pattern over the size limit, naming the operation as Go does.
pub(crate) fn pattern_limit<T>(ctx: &mut CallContext, method: &str) -> Result<T> {
    ctx.guard(
        ErrorKind::Memory,
        &format!("{method} pattern exceeds limit {MAX_PATTERN} bytes"),
    )
}

/// Refuses an input over the regex size limit; `what` names it after `method`,
/// such as `Regex.match text`, as Go does.
fn text_limit(ctx: &mut CallContext, bytes: &[u8], method: &str, what: &str) -> Result<()> {
    if bytes.len() > MAX_TEXT {
        ctx.guard(
            ErrorKind::Memory,
            &format!("{method} {what} exceeds limit {MAX_TEXT} bytes"),
        )
    } else {
        ctx.checkpoint()
    }
}

fn window(ctx: &mut CallContext, subject: &Value, start: usize, end: usize) -> Result<Value> {
    let bytes = subject.require_bytes()?;
    if start == 0 && end == bytes.len() {
        Ok(subject.clone())
    } else {
        ctx.bytes(&bytes[start..end])
    }
}

pub(crate) fn member(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<Option<Value>> {
    if name != "match?" || !matches!(receiver.0, Kind::Bytes(_)) {
        return Ok(None);
    }
    if keywords {
        return Err(Error::new(
            ErrorKind::Argument,
            "string.match? does not accept keyword arguments",
        ));
    }
    if !(1..=2).contains(&args.len()) {
        return Err(Error::new(
            ErrorKind::Argument,
            "string.match? expects a pattern and optional offset",
        ));
    }
    if !matches!(args[0].0, Kind::Bytes(_) | Kind::Regex(_)) {
        return Err(Error::new(
            ErrorKind::Type,
            "string.match? pattern must be string or regex",
        ));
    }
    let negative = || {
        Error::new(
            ErrorKind::Argument,
            "string.match? offset must be non-negative integer",
        )
    };
    let offset = if args.len() == 2 {
        crate::sequence::integer(&args[1]).map_err(|mut error| {
            error.message = negative().message;
            error
        })?
    } else {
        0
    };
    if offset < 0 {
        return Err(negative());
    }
    let text = receiver.require_bytes()?;
    if matches!(&args[0].0, Kind::Bytes(pattern) if pattern.data().len() > MAX_PATTERN) {
        return pattern_limit(ctx, "string.match?");
    }
    text_limit(ctx, text, "string.match?", "text")?;
    let compiled = if matches!(args[0].0, Kind::Regex(_)) {
        None
    } else {
        Some(Program::compile(ctx, args[0].clone(), "string.match?")?)
    };
    let program = match &args[0].0 {
        Kind::Regex(regex) => regex.view(),
        _ => compiled.as_ref().unwrap().view(),
    };
    let mut position = 0;
    let mut remaining = offset as u64;
    while remaining > 0 && position < text.len() {
        ctx.charge(1)?;
        position += scan::rune(&text[position..]).1;
        remaining -= 1;
    }
    if remaining > 0 {
        return Ok(Some(Value::boolean(false)));
    }
    let mut search = Search::new(ctx, program, false)?;
    Ok(Some(Value::boolean(
        search.find(ctx, program, text, position)?.is_some(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn compiled_program_search_and_capture_charges_have_independent_lifetimes() {
        let mut ctx = CallContext::new(CallOptions::default());
        let program = Program::compile(&mut ctx, Value::bytes(b"(a+)(b?)"), "Regex.match").unwrap();
        assert!(ctx.stats().retained_memory_bytes > 0);
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        let captures = search
            .find(&mut ctx, program.view(), b"zaab", 0)
            .unwrap()
            .unwrap();
        assert_eq!(captures.data, [1, 4, 1, 3, 3, 4]);
        drop(search);
        drop(program);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            captures.data.capacity() * size_of::<usize>()
        );
        drop(captures);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn instruction_expansion_is_rejected_before_program_allocation() {
        let mut ctx = CallContext::new(CallOptions::default());
        let source = format!("(?:{}){{1000}}", "a".repeat(101));
        let error = Program::compile(&mut ctx, Value::bytes(source.into_bytes()), "Regex.match")
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert!(ctx.stats().peak_memory_bytes < 65536);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(error.class(), Some(crate::ErrorClass::Limit));
        ctx.charge(1).unwrap();
    }

    #[test]
    fn capture_state_storage_cannot_bypass_the_memory_budget() {
        let mut ctx = CallContext::new(CallOptions::default());
        let program =
            Program::compile(&mut ctx, Value::bytes(b"(a?){1000}"), "Regex.match").unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(baseline + 32768);
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        let error = search.find(&mut ctx, program.view(), b"a", 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        drop(search);
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }
}
