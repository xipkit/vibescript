use super::{MethodKind, argument};
use crate::{CallContext, ErrorKind, Result, Value, value::Kind};

pub(super) fn check(
    ctx: &mut CallContext,
    name: &str,
    method: MethodKind,
    receiver: &Value,
    args: &[Value],
    block: bool,
) -> Result<()> {
    use MethodKind::*;
    if matches!(receiver.0, Kind::Range(_)) && method == Step {
        if matches!(args[0].0, Kind::Big(_)) {
            return ctx.guard(
                ErrorKind::Arithmetic,
                "range.step step must fit in a 64-bit integer",
            );
        }
        return Ok(());
    }
    if !matches!(receiver.0, Kind::Int(_) | Kind::Big(_))
        || !matches!(method, Times | Upto | Downto | Step)
    {
        return Ok(());
    }
    let big_receiver = matches!(receiver.0, Kind::Big(_));
    if method == Times {
        if block && big_receiver {
            return ctx.guard(
                ErrorKind::Arithmetic,
                "int.times count must fit in a 64-bit integer",
            );
        }
        if block
            && receiver
                .as_int()
                .is_some_and(|n| n as i128 > isize::MAX as i128)
        {
            return ctx.guard(ErrorKind::Arithmetic, "int.times value too large");
        }
        return Ok(());
    }
    if !big_receiver && !args.iter().any(|value| matches!(value.0, Kind::Big(_))) {
        return Ok(());
    }
    if !args[0].is_integer() {
        return Err(argument(&format!("int.{name} expects an integer limit")));
    }
    if method != Step && !block {
        return Err(argument(&format!("int.{name} requires a block")));
    }
    ctx.guard(
        ErrorKind::Arithmetic,
        &format!("int.{name} bounds must fit in a 64-bit integer"),
    )
}
