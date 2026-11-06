use super::*;
use crate::arguments::Target;

pub(super) fn start(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    mut args: Arguments,
) -> Result<()> {
    crate::format::validate(
        ctx,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )?;
    args.target = Some(Target::Format(1));
    storage.arguments.push(ctx, args)?;
    resume(program, ctx, frames, storage, stack, None)
}

pub(super) fn resume(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    mut returned: Option<Value>,
) -> Result<()> {
    loop {
        let args = storage.arguments.top_mut()?;
        let Some(Target::Format(index)) = args.target else {
            return Err(crate::checked::internal(
                "format arguments have no format target",
            ));
        };
        if index == args.positional.data.len() {
            let args = storage.arguments.pop()?;
            let value = crate::format::format(
                ctx,
                args.positional.at(0)?.require_bytes()?,
                args.positional.tail(1)?,
            )?;
            return stack.push(ctx, value);
        }
        ctx.charge(1)?;
        if let Some(value) = returned.take() {
            if matches!(value.0, Kind::Bytes(_)) {
                *args.positional.at_mut(index)? = value;
            }
        } else if let Some(call) = operators::string(ctx, args.positional.at(index)?)? {
            enter_arguments(
                program,
                ctx,
                frames,
                storage,
                call,
                Arguments::empty(),
                stack.data.len(),
            )?;
            frames.top_mut()?.return_to = ReturnTo::Format;
            return Ok(());
        }
        storage.arguments.top_mut()?.target = Some(Target::Format(index + 1));
    }
}
