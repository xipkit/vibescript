use crate::{
    CallContext, Error, ErrorKind, HostFunction, Result, Value,
    budget::Buffer,
    bytecode::{Op, Program, Selection},
    hash::Hash,
    json, ops,
    range::Range,
    value::Kind,
};

struct Frame {
    function: usize,
    ip: usize,
    base: usize,
    locals: Buffer<Option<Value>>,
    loops: Buffer<LoopState>,
}

struct LoopState {
    base: usize,
    next: usize,
    end: usize,
    expression: bool,
    source: Value,
    position: i128,
    length: i128,
    last: Value,
    broken: bool,
    break_value: Option<Value>,
}

impl LoopState {
    fn next_value(&mut self, ctx: &mut CallContext) -> Result<Option<Value>> {
        if self.position >= self.length {
            return Ok(None);
        }
        ctx.charge(1)?;
        let value = match &self.source.0 {
            Kind::Array(h) => h.buffer.data[self.position as usize].clone(),
            Kind::Hash(h) => {
                let (key, value) = &h.buffer.data[self.position as usize];
                ctx.array(&[key.clone(), value.clone()])?
            }
            Kind::Range(r) => {
                let start = r.start.unwrap();
                let direction = if start <= r.end.unwrap() { 1 } else { -1 };
                Value::int((i128::from(start) + self.position * direction) as i64)
            }
            _ => unreachable!(),
        };
        self.position += 1;
        Ok(Some(value))
    }

    fn result(self) -> Value {
        if let Some(value) = self.break_value {
            return value;
        }
        if self.expression {
            if self.broken {
                Value::nil()
            } else {
                self.source
            }
        } else {
            self.last
        }
    }
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
                let v = frame.locals.data[n].clone().unwrap_or_default();
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
            Op::AddStore(n) => {
                let b = stack.data.pop().unwrap();
                let a = stack.data.pop().unwrap();
                frame.locals.data[n] = None;
                let value = ops::binary(ctx, "+", a, b)?;
                frame.locals.data[n] = Some(value.clone());
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
                let mut values = Hash::empty();
                values.buffer.ensure(ctx, n)?;
                let mut iter = stack.data.drain(base..);
                while let Some(key) = iter.next() {
                    let value = iter.next().unwrap();
                    values.insert(ctx, key, value)?;
                }
                drop(iter);
                let value = Value::from_hash(ctx, values)?;
                stack.push(ctx, value)?;
            }
            Op::Range(start, end, exclusive) => {
                let end = if end {
                    Some(stack.data.pop().unwrap().require_int()?)
                } else {
                    None
                };
                let start = if start {
                    Some(stack.data.pop().unwrap().require_int()?)
                } else {
                    None
                };
                let value = Value(Kind::Range(Range::new(ctx, start, end, exclusive)?));
                stack.push(ctx, value)?;
            }
            Op::Index(n) => {
                let base = stack.data.len() - n - 1;
                let root = &stack.data[base];
                let args = &stack.data[base + 1..];
                let value = if n == 1 {
                    ops::index(ctx, root, &args[0])?
                } else {
                    crate::sequence::slice(ctx, root, args, false)?
                };
                stack.data.truncate(base);
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
            Op::SetIndexFromValue(n) => {
                let key = stack.data.pop().unwrap();
                let root = stack.data.pop().unwrap();
                let value = stack.data.pop().unwrap();
                frame.locals.data[n] = None;
                let new = ops::set_index(ctx, root, key, value.clone())?;
                frame.locals.data[n] = Some(new);
                stack.push(ctx, value)?;
            }
            Op::IndexResult => {
                let value = stack.data.pop().unwrap();
                stack.data.pop().unwrap();
                stack.data.pop().unwrap();
                stack.push(ctx, value)?;
            }
            Op::Extract(selection) => {
                let source = stack.data.last().unwrap();
                let values = source
                    .as_array()
                    .unwrap_or_else(|| std::slice::from_ref(source));
                let value = match selection {
                    Selection::At(n) => values.get(n).cloned().unwrap_or_default(),
                    Selection::Rest { leading, trailing } => {
                        let start = leading.min(values.len());
                        let end = values.len().saturating_sub(trailing).max(start);
                        ctx.array(&values[start..end])?
                    }
                    Selection::Tail {
                        leading,
                        trailing,
                        index,
                    } => {
                        let pos = values
                            .len()
                            .saturating_sub(trailing)
                            .max(leading)
                            .saturating_add(index);
                        values.get(pos).cloned().unwrap_or_default()
                    }
                };
                stack.push(ctx, value)?;
            }
            Op::CaseCompare(target, splat) => {
                let candidate = stack.data.pop().unwrap();
                let target = if target {
                    Some(stack.data.pop().unwrap())
                } else {
                    None
                };
                let matched = ops::case_matches(ctx, target.as_ref(), &candidate, splat)?;
                stack.push(ctx, Value::boolean(matched))?;
            }
            Op::LoopStart {
                iterable,
                expression,
                next,
                end,
            } => {
                let source = if iterable {
                    stack.data.pop().unwrap()
                } else {
                    Value::nil()
                };
                let length = if iterable {
                    match &source.0 {
                        Kind::Array(h) => h.buffer.data.len() as i128,
                        Kind::Hash(h) => h.buffer.data.len() as i128,
                        Kind::Range(r) => r.length()?,
                        _ => return Err(Error::new(ErrorKind::Type, "cannot iterate this value")),
                    }
                } else {
                    0
                };
                frame.loops.push(
                    ctx,
                    LoopState {
                        base: stack.data.len(),
                        next,
                        end,
                        expression,
                        source,
                        position: 0,
                        length,
                        last: Value::nil(),
                        broken: false,
                        break_value: None,
                    },
                )?;
            }
            Op::LoopTest => {
                if !stack.data.pop().unwrap().truthy() {
                    frame.ip = frame.loops.data.last().unwrap().end;
                }
            }
            Op::IterNext => {
                let state = frame.loops.data.last_mut().unwrap();
                if let Some(value) = state.next_value(ctx)? {
                    stack.push(ctx, value)?;
                } else {
                    frame.ip = state.end;
                }
            }
            Op::LoopBody => {
                let state = frame.loops.data.last_mut().unwrap();
                state.last = stack.data.pop().unwrap();
                stack.data.truncate(state.base);
                frame.ip = state.next;
            }
            Op::LoopEnd => {
                let state = frame.loops.data.pop().unwrap();
                stack.data.truncate(state.base);
                stack.push(ctx, state.result())?;
            }
            Op::Break(has_value) => {
                let state = frame
                    .loops
                    .data
                    .last_mut()
                    .ok_or_else(|| Error::new(ErrorKind::Argument, "break outside loop"))?;
                state.broken = true;
                state.break_value = if has_value {
                    Some(stack.data.pop().unwrap())
                } else {
                    None
                };
                stack.data.truncate(state.base);
                frame.ip = state.end;
            }
            Op::Next => {
                let state = frame
                    .loops
                    .data
                    .last()
                    .ok_or_else(|| Error::new(ErrorKind::Argument, "next outside loop"))?;
                stack.data.truncate(state.base);
                frame.ip = state.next;
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
            loops: Buffer::empty(),
        },
    )
}
