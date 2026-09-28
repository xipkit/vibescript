use super::{MethodKind, argument};
use crate::{CallContext, Error, ErrorKind, Result, Value, value::Kind};

pub(super) fn aggregate(
    name: &str,
    method: MethodKind,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<()> {
    let sum = method == MethodKind::Sum;
    if args.len() > usize::from(sum) {
        return Err(argument(&format!(
            "range.{name} {}",
            if sum {
                "expects at most one argument"
            } else {
                "does not take arguments"
            }
        )));
    }
    if keywords {
        return Err(argument(&format!(
            "range.{name} does not take keyword arguments"
        )));
    }
    if block {
        return Err(argument(&format!(
            "range.{name} does not {} a block",
            if sum { "take" } else { "accept" }
        )));
    }
    if sum && args.first().is_some_and(|value| !value.is_integer()) {
        return Err(argument("range.sum expects an integer initial value"));
    }
    Ok(())
}

/// Validates `int.times`, `int.upto`, `int.downto`, `int.step` and
/// `range.step` in the reference order, before any iteration state is built.
pub(super) fn stepping(
    ctx: &mut CallContext,
    name: &str,
    method: MethodKind,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<()> {
    use MethodKind::*;
    let big = |value: &Value| matches!(value.0, Kind::Big(_));
    let expected = |message: String| Error::new(ErrorKind::Type, message);
    let blocked = |message: String| {
        if block {
            Ok(())
        } else {
            Err(argument(&message))
        }
    };
    if matches!(receiver.0, Kind::Range(_)) {
        if method != Step {
            return Ok(());
        }
        if args.len() != 1 {
            return Err(argument("range.step expects one integer argument"));
        }
        if keywords {
            return Err(argument("range.step does not take keyword arguments"));
        }
        if big(&args[0]) {
            return ctx.guard(
                ErrorKind::Arithmetic,
                "range.step step must fit in a 64-bit integer",
            );
        }
        let stride = args[0]
            .as_int()
            .ok_or_else(|| expected("range.step expects an integer step".into()))?;
        if stride <= 0 {
            return Err(argument("range.step step must be positive"));
        }
        return blocked("range.step requires a block".into());
    }
    if !matches!(receiver.0, Kind::Int(_) | Kind::Big(_)) {
        return Ok(());
    }
    match method {
        Times => {
            if !args.is_empty() {
                return Err(argument("int.times does not take arguments"));
            }
            blocked("int.times requires a block".into())?;
            if big(receiver) {
                return ctx.guard(
                    ErrorKind::Arithmetic,
                    "int.times count must fit in a 64-bit integer",
                );
            }
            return Ok(());
        }
        Step if args.is_empty() || args.len() > 2 => {
            return Err(argument("int.step expects a limit and an optional step"));
        }
        Upto | Downto if args.len() != 1 => {
            return Err(argument(&format!(
                "int.{name} expects one integer argument"
            )));
        }
        Step | Upto | Downto => {}
        _ => return Ok(()),
    }
    if keywords {
        return Err(argument(&format!(
            "int.{name} does not take keyword arguments"
        )));
    }
    if big(receiver) || args.iter().any(big) {
        if !args[0].is_integer() {
            return Err(argument(&format!("int.{name} expects an integer limit")));
        }
        if method != Step {
            blocked(format!("int.{name} requires a block"))?;
        }
        return ctx.guard(
            ErrorKind::Arithmetic,
            &format!("int.{name} bounds must fit in a 64-bit integer"),
        );
    }
    if args[0].as_int().is_none() {
        return Err(expected(format!("int.{name} expects an integer limit")));
    }
    if let Some(stride) = args.get(1) {
        match stride.as_int() {
            None => return Err(expected("int.step expects an integer step".into())),
            Some(0) => return Err(argument("int.step step must not be zero")),
            Some(_) => {}
        }
    }
    blocked(format!("int.{name} requires a block"))
}
