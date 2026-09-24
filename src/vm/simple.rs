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
                stack.data.pop().unwrap();
            }
            Op::Dup => {
                step(ctx, frame)?;
                let value = stack.data.last().unwrap().clone();
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
                let mut value = storage.locals.data[slot].clone().unwrap_or_default();
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
                    value.clone()
                };
                stack.push(ctx, value)?;
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
                storage.locals.data[slot] = Some(value.clone());
            }
            Op::AddStore(n) => {
                let Some(slot) = own(function, frame, storage, n).filter(|_| numbers(stack)) else {
                    return Ok(());
                };
                step(ctx, frame)?;
                let b = stack.data.pop().unwrap();
                let a = stack.data.pop().unwrap();
                let value = match ops::immediate(ctx, "+", &a, &b)? {
                    Some(value) => value,
                    None => {
                        storage.locals.data[slot] = None;
                        ops::binary(ctx, "+", a, b)?
                    }
                };
                if !storage.addresses.data.is_empty() {
                    address::refresh(ctx, slot, &value, &mut storage.addresses.data, &[])?;
                }
                storage.locals.data[slot] = Some(value.clone());
                stack.push(ctx, value)?;
            }
            Op::Binary(op) => {
                // Numbers never reach an operator a class defines.
                if !numbers(stack) {
                    return Ok(());
                }
                step(ctx, frame)?;
                let b = stack.data.pop().unwrap();
                let a = stack.data.pop().unwrap();
                let value = match ops::immediate(ctx, op, &a, &b)? {
                    Some(value) => value,
                    None => ops::binary(ctx, op, a, b)?,
                };
                stack.push(ctx, value)?;
            }
            Op::Jump(target) => {
                step(ctx, frame)?;
                frame.ip = target;
            }
            Op::JumpFalse(target) => {
                step(ctx, frame)?;
                if !stack.data.pop().unwrap().truthy() {
                    frame.ip = target;
                }
            }
            Op::JumpTrue(target) => {
                step(ctx, frame)?;
                if stack.data.pop().unwrap().truthy() {
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
                if !stack.data.pop().unwrap().truthy() {
                    frame.ip = frame.loops.data.last().unwrap().end;
                }
            }
            Op::LoopBody => {
                step(ctx, frame)?;
                let state = frame.loops.data.last_mut().unwrap();
                state.last = stack.data.pop().unwrap();
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

/// Reports whether the two topmost values are both integers or both floats.
fn numbers(stack: &Buffer<Value>) -> bool {
    let [.., a, b] = stack.data.as_slice() else {
        return false;
    };
    matches!(
        (&a.0, &b.0),
        (Kind::Int(_), Kind::Int(_)) | (Kind::Float(_), Kind::Float(_))
    )
}
