//! A tight loop for the instructions that need no scope lookup, frame change or
//! operator dispatch.
//!
//! Each instruction here advances, charges and behaves exactly as its arm in
//! [`Run::advance`], which runs every instruction this loop declines, including
//! these same ones whenever their conditions here do not hold.

use super::*;
use crate::bytecode::Function;

/// Runs the executing frame's simple instructions, stopping before the first
/// one it declines. A declined instruction is left unexecuted and uncharged.
pub(super) fn run(
    ctx: &mut CallContext,
    program: &Program,
    function: &Function,
    frame: &mut Frame,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
) -> Result<()> {
    // File programs, root bindings and host globals can shadow any local.
    if program.file || storage.bindings.is_some() || !ctx.options.globals.is_empty() {
        return Ok(());
    }
    loop {
        match function.code[frame.ip] {
            Op::Nil => {
                step(ctx, frame)?;
                stack.push(ctx, Value::nil())?;
            }
            Op::Pop => {
                step(ctx, frame)?;
                discard(stack.data.pop().unwrap());
            }
            Op::Dup => {
                step(ctx, frame)?;
                let value = copy(stack.data.last().unwrap());
                stack.push(ctx, value)?;
            }
            Op::Constant(n) => {
                step(ctx, frame)?;
                let value = ctx.import(&program.constants[n])?;
                stack.push(ctx, value)?;
            }
            Op::Load(n) => {
                let Some(slot) = own(function, frame, storage, n) else {
                    return Ok(());
                };
                step(ctx, frame)?;
                let mut value = storage.locals.data[slot]
                    .as_ref()
                    .map_or_else(Value::nil, copy);
                if let Kind::Offset(offset) = &value.0 {
                    return Err(offset.value_error());
                }
                if let Kind::Builtin(builtin) = value.0 {
                    value = builtin.read(ctx)?;
                }
                stack.push(ctx, value)?;
            }
            Op::LoadOptional(n, _) => {
                let Some(value) = storage.locals.data[frame.local_base + n].as_ref() else {
                    return Ok(());
                };
                step(ctx, frame)?;
                if let Kind::Offset(offset) = &value.0 {
                    return Err(offset.value_error());
                }
                let value = if let Kind::Builtin(builtin) = value.0 {
                    builtin.read(ctx)?
                } else {
                    copy(value)
                };
                stack.push(ctx, value)?;
            }
            Op::ReceiverBound(n, next) => {
                let Some(value) = storage.locals.data[frame.local_base + n].as_ref() else {
                    return Ok(());
                };
                step(ctx, frame)?;
                let value = copy(value);
                stack.push(ctx, value)?;
                frame.ip = next;
            }
            Op::Declare(n) => {
                let Some(slot) = own(function, frame, storage, n) else {
                    return Ok(());
                };
                step(ctx, frame)?;
                storage.locals.data[slot].get_or_insert_with(Value::nil);
            }
            Op::Store(n) => {
                let Some(slot) = own(function, frame, storage, n) else {
                    return Ok(());
                };
                step(ctx, frame)?;
                let value = stack.data.last().unwrap();
                address::refresh(ctx, slot, value, &mut storage.addresses.data, &[])?;
                store(storage, slot, copy(value));
            }
            Op::AddStore(n) => {
                let Some(slot) = own(function, frame, storage, n).filter(|_| plain_operand(stack))
                else {
                    return Ok(());
                };
                step(ctx, frame)?;
                let b = stack.data.pop().unwrap();
                let a = stack.data.pop().unwrap();
                let value = match ops::immediate(ctx, "+", &a, &b)? {
                    Some(value) => {
                        discard(a);
                        discard(b);
                        value
                    }
                    None => {
                        storage.locals.data[slot] = None;
                        ops::binary(ctx, "+", a, b)?
                    }
                };
                if !storage.addresses.data.is_empty() {
                    address::refresh(ctx, slot, &value, &mut storage.addresses.data, &[])?;
                }
                store(storage, slot, copy(&value));
                stack.push(ctx, value)?;
            }
            Op::Binary(op) => {
                if !plain_operand(stack) {
                    return Ok(());
                }
                step(ctx, frame)?;
                let b = stack.data.pop().unwrap();
                let a = stack.data.pop().unwrap();
                let value = match ops::immediate(ctx, op, &a, &b)? {
                    Some(value) => {
                        discard(a);
                        discard(b);
                        value
                    }
                    None => ops::binary(ctx, op, a, b)?,
                };
                stack.push(ctx, value)?;
            }
            Op::Index(n) => {
                // Exported values need their depth checked after indexing.
                if matches!(stack.data[stack.data.len() - n - 1].0, Kind::Instance(_))
                    || ctx.has_exports
                {
                    return Ok(());
                }
                step(ctx, frame)?;
                index(ctx, function, frame, stack, n)?;
            }
            Op::Array(n) => {
                step(ctx, frame)?;
                array(ctx, stack, n)?;
            }
            Op::IterNext => {
                step(ctx, frame)?;
                iterate(ctx, frame, stack)?;
            }
            Op::Jump(target) => {
                step(ctx, frame)?;
                frame.ip = target;
            }
            Op::JumpFalse(target) => {
                step(ctx, frame)?;
                if !truthy(stack.data.pop().unwrap()) {
                    frame.ip = target;
                }
            }
            Op::JumpTrue(target) => {
                step(ctx, frame)?;
                if truthy(stack.data.pop().unwrap()) {
                    frame.ip = target;
                }
            }
            Op::JumpNil(target) => {
                step(ctx, frame)?;
                if matches!(stack.data.last().unwrap().0, Kind::Nil) {
                    frame.ip = target;
                }
            }
            Op::LoopTest => {
                step(ctx, frame)?;
                if !truthy(stack.data.pop().unwrap()) {
                    frame.ip = frame.loops.data.last().unwrap().end;
                }
            }
            Op::LoopBody => {
                step(ctx, frame)?;
                let state = frame.loops.data.last_mut().unwrap();
                discard(std::mem::replace(
                    &mut state.last,
                    stack.data.pop().unwrap(),
                ));
                stack.data.truncate(state.base);
                storage.addresses.data.truncate(state.address_base);
                storage.bypasses.data.truncate(state.bypass_base);
                storage.texts.data.truncate(state.text_base);
                frame.arguments.data.truncate(state.argument_base);
                frame.ip = state.next;
            }
            Op::RootCall(_, expanded) => {
                // Without host bindings a resolved call needs no pending target.
                if expanded || !ctx.capability_names.data.is_empty() {
                    return Ok(());
                }
                step(ctx, frame)?;
            }
            _ => return Ok(()),
        }
    }
}

/// Indexes a value other than an instance by the `count` values above it.
#[inline(never)]
fn index(
    ctx: &mut CallContext,
    function: &Function,
    frame: &mut Frame,
    stack: &mut Buffer<Value>,
    count: usize,
) -> Result<()> {
    let base = stack.data.len() - count - 1;
    let root = &stack.data[base];
    let args = &stack.data[base + 1..];
    let value = if count == 1 {
        ops::index(ctx, root, &args[0])?
    } else {
        ops::index_many(ctx, root, args)?
    };
    if matches!(value.0, Kind::Host(_))
        && matches!(root.0, Kind::Hash(_))
        && matches!(function.code.get(frame.ip), Some(Op::CallValue))
    {
        // `receiver[:name](...)` keeps its root for the host method.
        let receiver = root.clone();
        if let Some(pending) = frame.arguments.data.last_mut() {
            pending.receiver = Some(receiver);
        }
    }
    stack.data.truncate(base);
    stack.push(ctx, value)
}

/// Collects the `count` topmost values into an array literal.
#[inline(never)]
fn array(ctx: &mut CallContext, stack: &mut Buffer<Value>, count: usize) -> Result<()> {
    let base = stack.data.len() - count;
    let mut values = Buffer::with_capacity(ctx, count)?;
    for value in stack.data.drain(base..) {
        ctx.charge(1)?;
        values.data.push(value);
    }
    let value = Value::from_array(ctx, values)?;
    stack.push(ctx, value)
}

/// Pushes the innermost loop's next element, or leaves the loop.
#[inline(never)]
fn iterate(ctx: &mut CallContext, frame: &mut Frame, stack: &mut Buffer<Value>) -> Result<()> {
    let state = frame.loops.data.last_mut().unwrap();
    if let Some(value) = state.next_value(ctx)? {
        crate::exports::check(ctx, &value)?;
        stack.push(ctx, value)?;
    } else {
        frame.ip = state.end;
    }
    Ok(())
}

/// Moves past the current instruction and charges its step, as dispatch does.
#[inline(always)]
fn step(ctx: &mut CallContext, frame: &mut Frame) -> Result<()> {
    frame.ip += 1;
    ctx.charge(1)
}

/// Returns the slot a local names when it resolves to the executing frame's
/// own slot without a scope walk, as it does once bound or when the function
/// neither captures it nor initializes a namespace.
fn own(function: &Function, frame: &Frame, storage: &Storage, local: usize) -> Option<usize> {
    let slot = frame.local_base + local;
    (storage.locals.data[slot].is_some()
        || (!function.initializer && function.captures.get(local).copied().flatten().is_none()))
    .then_some(slot)
}

/// Clones a value, copying nil, booleans and numbers inline rather than
/// through the general clone.
#[inline(always)]
fn copy(value: &Value) -> Value {
    match value.0 {
        Kind::Nil => Value::nil(),
        Kind::Bool(value) => Value::boolean(value),
        Kind::Int(value) => Value::int(value),
        Kind::Float(value) => Value::float(value),
        _ => value.clone(),
    }
}

/// Drops a value. Nil, booleans and numbers own no storage, so they are
/// forgotten rather than run through the general drop.
#[inline(always)]
fn discard(value: Value) {
    if matches!(
        value.0,
        Kind::Nil | Kind::Bool(_) | Kind::Int(_) | Kind::Float(_)
    ) {
        std::mem::forget(value);
    }
}

/// Reports whether a popped condition is truthy, discarding it.
#[inline(always)]
fn truthy(value: Value) -> bool {
    let truthy = value.truthy();
    discard(value);
    truthy
}

/// Writes a local's slot, discarding the value it held.
#[inline(always)]
fn store(storage: &mut Storage, slot: usize, value: Value) {
    if let Some(previous) = storage.locals.data[slot].replace(value) {
        discard(previous);
    }
}

/// Reports whether the left operand under the top of the stack is something
/// other than an instance, the only receiver whose class can define an
/// operator.
fn plain_operand(stack: &Buffer<Value>) -> bool {
    let [.., a, _] = stack.data.as_slice() else {
        return false;
    };
    !matches!(a.0, Kind::Instance(_))
}
