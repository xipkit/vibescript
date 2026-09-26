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
    frames.data.last_mut().unwrap().arguments.push(ctx, args)?;
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
        let args = frames
            .data
            .last_mut()
            .unwrap()
            .arguments
            .data
            .last_mut()
            .unwrap();
        let Some(Target::Format(index)) = args.target else {
            unreachable!()
        };
        if index == args.positional.data.len() {
            let args = frames
                .data
                .last_mut()
                .unwrap()
                .arguments
                .data
                .pop()
                .unwrap();
            let value = crate::format::format(
                ctx,
                args.positional.data[0].require_bytes()?,
                &args.positional.data[1..],
            )?;
            return stack.push(ctx, value);
        }
        ctx.charge(1)?;
        if let Some(value) = returned.take() {
            if matches!(value.0, Kind::Bytes(_)) {
                args.positional.data[index] = value;
            }
        } else if let Some(call) = operators::string(ctx, &args.positional.data[index])? {
            enter_arguments(
                program,
                ctx,
                frames,
                storage,
                call,
                Arguments::empty(),
                stack.data.len(),
            )?;
            frames.data.last_mut().unwrap().return_to = ReturnTo::Format;
            return Ok(());
        }
        frames
            .data
            .last_mut()
            .unwrap()
            .arguments
            .data
            .last_mut()
            .unwrap()
            .target = Some(Target::Format(index + 1));
    }
}
