use crate::{
    CallContext, Error, ErrorKind, HostFunction, Result, Value,
    budget::Buffer,
    bytecode::{Op, Program},
    json, ops,
};

struct Frame {
    function: usize,
    ip: usize,
    base: usize,
    locals: Buffer<Option<Value>>,
}

pub(crate) fn execute(
    program: &Program,
    hosts: &[HostFunction],
    ctx: &mut CallContext,
    function: usize,
    args: &[Value],
) -> Result<Value> {
    ctx.checkpoint()?;
    let mut stack = Buffer::empty();
    let mut frames = Buffer::empty();
    let mut input = Buffer::with_capacity(ctx, args.len())?;
    for arg in args {
        input.data.push(ctx.import(arg)?);
    }
    enter(program, ctx, &mut frames, function, &input.data, 0)?;
    drop(input);
    loop {
        ctx.charge(1)?;
        let frame = frames.data.last_mut().unwrap();
        let op = program.functions[frame.function].code[frame.ip];
        frame.ip += 1;
        match op {
            Op::Constant(n) => {
                let v = ctx.import(&program.constants[n])?;
                stack.push(ctx, v)?;
            }
            Op::Nil => stack.push(ctx, Value::nil())?,
            Op::Load(n) => {
                let v = frame.locals.data[n].clone().ok_or_else(|| {
                    Error::new(ErrorKind::Name, "local variable is uninitialized")
                })?;
                stack.push(ctx, v)?;
            }
            Op::Store(n) => {
                frame.locals.data[n] = Some(stack.data.last().unwrap().clone());
            }
            Op::ReleaseLocal(n) => {
                // Arguments have finished evaluating; the operand stack now owns the receiver.
                frame.locals.data[n] = None;
            }
            Op::Pop => {
                stack.data.pop().unwrap();
            }
            Op::Dup => {
                let v = stack.data.last().unwrap().clone();
                stack.push(ctx, v)?;
            }
            Op::Dup2 => {
                let len = stack.data.len();
                let a = stack.data[len - 2].clone();
                let b = stack.data[len - 1].clone();
                stack.push(ctx, a)?;
                stack.push(ctx, b)?;
            }
            Op::Unary(op) => {
                let value = stack.data.pop().unwrap();
                let result = ops::unary(op, value)?;
                stack.push(ctx, result)?;
            }
            Op::Binary(op) => {
                let b = stack.data.pop().unwrap();
                let a = stack.data.pop().unwrap();
                let value = ops::binary(ctx, op, a, b)?;
                stack.push(ctx, value)?;
            }
            Op::Array(n) => {
                let base = stack.data.len() - n;
                let mut values = Buffer::with_capacity(ctx, n)?;
                for value in stack.data.drain(base..) {
                    ctx.charge(1)?;
                    values.data.push(value);
                }
                let value = Value::from_array(ctx, values)?;
                stack.push(ctx, value)?;
            }
            Op::Hash(n) => {
                let base = stack.data.len() - n * 2;
                let mut values = Buffer::with_capacity(ctx, n)?;
                let mut iter = stack.data.drain(base..);
                while let Some(key) = iter.next() {
                    let value = iter.next().unwrap();
                    json::insert(ctx, &mut values, key, value)?;
                }
                drop(iter);
                let value = Value::from_hash(ctx, values)?;
                stack.push(ctx, value)?;
            }
            Op::Index => {
                let key = stack.data.pop().unwrap();
                let root = stack.data.pop().unwrap();
                let value = ops::index(ctx, &root, &key)?;
                stack.push(ctx, value)?;
            }
            Op::SetIndex(n) => {
                let value = stack.data.pop().unwrap();
                let key = stack.data.pop().unwrap();
                let root = stack.data.pop().unwrap();
                frame.locals.data[n] = None;
                let new = ops::set_index(ctx, root, key, value.clone())?;
                frame.locals.data[n] = Some(new);
                stack.push(ctx, value)?;
            }
            Op::Call(function, n) => {
                let base = stack.data.len() - n;
                enter(
                    program,
                    ctx,
                    &mut frames,
                    function,
                    &stack.data[base..],
                    base,
                )?;
                stack.data.truncate(base);
            }
            Op::Host(host, n) => {
                let base = stack.data.len() - n;
                ctx.checkpoint()?;
                let result = hosts[host](ctx, &stack.data[base..]);
                ctx.checkpoint()?;
                let result = result?;
                let value = ctx.import(&result)?;
                stack.data.truncate(base);
                stack.push(ctx, value)?;
            }
            Op::Method(method, n) => {
                let base = stack.data.len() - n - 1;
                let root = std::mem::take(&mut stack.data[base]);
                let value = ops::method(ctx, method, root, &stack.data[base + 1..])?;
                stack.data.truncate(base);
                stack.push(ctx, value)?;
            }
            Op::JsonParse => {
                let input = stack.data.pop().unwrap();
                let value = json::parse(ctx, input.require_bytes()?)?;
                stack.push(ctx, value)?;
            }
            Op::JsonStringify => {
                let input = stack.data.pop().unwrap();
                let value = json::stringify(ctx, &input)?;
                stack.push(ctx, value)?;
            }
            Op::Jump(target) => frame.ip = target,
            Op::JumpFalse(target) => {
                if !stack.data.pop().unwrap().truthy() {
                    frame.ip = target;
                }
            }
            Op::JumpTrue(target) => {
                if stack.data.pop().unwrap().truthy() {
                    frame.ip = target;
                }
            }
            Op::Return => {
                let value = stack.data.pop().unwrap();
                let frame = frames.data.pop().unwrap();
                stack.data.truncate(frame.base);
                if frames.data.is_empty() {
                    ctx.checkpoint()?;
                    return Ok(value);
                }
                stack.push(ctx, value)?;
            }
        }
    }
}

fn enter(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    function: usize,
    args: &[Value],
    base: usize,
) -> Result<()> {
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.fail(ErrorKind::Recursion, "recursion limit exceeded");
    }
    let fun = &program.functions[function];
    if args.len() != fun.arity {
        return Err(Error::new(
            ErrorKind::Argument,
            format!(
                "{} expects {} arguments, got {}",
                fun.name,
                fun.arity,
                args.len()
            ),
        ));
    }
    let mut locals = Buffer::with_capacity(ctx, fun.locals)?;
    for _ in 0..fun.locals {
        ctx.charge(1)?;
        locals.data.push(None);
    }
    for (i, arg) in args.iter().enumerate() {
        ctx.charge(1)?;
        locals.data[i] = Some(arg.clone());
    }
    frames.push(
        ctx,
        Frame {
            function,
            ip: 0,
            base,
            locals,
        },
    )
}
