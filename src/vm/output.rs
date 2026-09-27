use super::*;
use crate::{arguments::Target, output::Kind as Output};

pub(super) fn start(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    kind: Output,
    mut args: Arguments,
) -> Result<()> {
    kind.validate(
        ctx,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )?;
    if args.positional.data.is_empty() {
        kind.write_empty(ctx)?;
        return stack.push(ctx, Value::nil());
    }
    args.target = Some(Target::Output(kind, 0));
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
        let args = storage.arguments.data.last_mut().unwrap();
        let Some(Target::Output(kind, index)) = args.target else {
            unreachable!()
        };
        if index == args.positional.data.len() {
            let args = storage.arguments.data.pop().unwrap();
            let value = if kind != Output::Inspect {
                Value::nil()
            } else if args.positional.data.len() == 1 {
                args.positional.data[0].clone()
            } else {
                Value::from_array(ctx, args.positional)?
            };
            return stack.push(ctx, value);
        }
        ctx.charge(1)?;
        let original = args.positional.data[index].clone();
        if let Some(value) = returned.take() {
            let rendered = if matches!(value.0, Kind::Bytes(_)) {
                &value
            } else {
                &original
            };
            kind.write(ctx, rendered)?;
        } else {
            if kind != Output::Inspect {
                if let Some(call) = operators::string(ctx, &original)? {
                    enter_arguments(
                        program,
                        ctx,
                        frames,
                        storage,
                        call,
                        Arguments::empty(),
                        stack.data.len(),
                    )?;
                    frames.data.last_mut().unwrap().return_to = ReturnTo::Output;
                    return Ok(());
                }
            }
            kind.write(ctx, &original)?;
        }
        storage.arguments.data.last_mut().unwrap().target = Some(Target::Output(kind, index + 1));
    }
}
