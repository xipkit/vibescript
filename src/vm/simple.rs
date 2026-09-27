//! A tight loop for the instructions that need no scope lookup, frame change or
//! operator dispatch.
//!
//! Each instruction here advances, charges and behaves exactly as its arm in
//! [`Run::advance`], which runs every instruction this loop declines, including
//! these same ones whenever their conditions here do not hold.

use super::*;
use crate::bytecode::{Function, Method};

/// Runs the executing frame's simple instructions, stopping before the first
/// one it declines. A declined instruction is left unexecuted and uncharged.
pub(super) fn run(
    ctx: &mut CallContext,
    program: &Program,
    function: &Function,
    outer: &[Frame],
    frame: &mut Frame,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
) -> Result<()> {
    // A required file's own bindings live in its scope rather than in slots.
    if program.file {
        return Ok(());
    }
    // Root bindings, such as a capability's name, and host globals take the
    // place of a local that no slot binds, which the general loop resolves.
    let shadowed = storage.bindings.is_some() || !ctx.options.globals.is_empty();
    loop {
        match function.code[frame.ip] {
            Op::Nil => {
                step(ctx, frame)?;
                push(ctx, stack, Value::nil())?;
            }
            Op::Pop => {
                step(ctx, frame)?;
                discard(stack.data.pop().unwrap());
            }
            Op::Dup => {
                step(ctx, frame)?;
                let value = copy(stack.data.last().unwrap());
                push(ctx, stack, value)?;
            }
            Op::Constant(n) => {
                step(ctx, frame)?;
                let value = ctx.import(&program.constants[n])?;
                push(ctx, stack, value)?;
            }
            Op::Shared(slot) => {
                step(ctx, frame)?;
                let value = shared(ctx, program, storage, slot)?;
                push(ctx, stack, value)?;
            }
            Op::Load(n) => {
                let Some(slot) = local(ctx, outer, function, frame, storage, n, shadowed)? else {
                    return Ok(());
                };
                let mut value = storage.locals.data[slot]
                    .as_ref()
                    .map_or_else(Value::nil, copy);
                if let Kind::Offset(offset) = &value.0 {
                    return Err(offset.value_error());
                }
                if let Kind::Builtin(builtin) = value.0 {
                    value = builtin.read(ctx)?;
                }
                push(ctx, stack, value)?;
            }
            Op::LoadOptional(n, _, _) => {
                let own = frame.local_base + n;
                let slot = if storage.locals.data[own].is_some() {
                    step(ctx, frame)?;
                    own
                } else {
                    match bound(ctx, outer, function, frame, storage, n)? {
                        Some(slot) => slot,
                        None => return Ok(()),
                    }
                };
                let value = storage.locals.data[slot].as_ref().unwrap();
                if let Kind::Offset(offset) = &value.0 {
                    return Err(offset.value_error());
                }
                let value = if let Kind::Builtin(builtin) = value.0 {
                    builtin.read(ctx)?
                } else {
                    copy(value)
                };
                push(ctx, stack, value)?;
            }
            Op::ReceiverBound(n, next) => {
                let Some(value) = storage.locals.data[frame.local_base + n].as_ref() else {
                    return Ok(());
                };
                step(ctx, frame)?;
                let value = copy(value);
                push(ctx, stack, value)?;
                frame.ip = next;
            }
            Op::Declare(n) => {
                let Some(slot) = local(ctx, outer, function, frame, storage, n, shadowed)? else {
                    return Ok(());
                };
                storage.locals.data[slot].get_or_insert_with(Value::nil);
            }
            Op::Store(n) => {
                let Some(slot) = local(ctx, outer, function, frame, storage, n, shadowed)? else {
                    return Ok(());
                };
                let value = stack.data.last().unwrap();
                address::refresh(ctx, slot, value, &mut storage.addresses.data, &[])?;
                store(storage, slot, copy(value));
            }
            Op::AddStore(n) => {
                if !plain_operand(stack) {
                    return Ok(());
                }
                let Some(slot) = local(ctx, outer, function, frame, storage, n, shadowed)? else {
                    return Ok(());
                };
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
                push(ctx, stack, value)?;
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
                push(ctx, stack, value)?;
            }
            Op::Index(n) => {
                // Exported values need their depth checked after indexing,
                // unless the checker proved the result plain.
                if matches!(stack.data[stack.data.len() - n - 1].0, Kind::Instance(_))
                    || (ctx.has_exports && !function.plain_values.contains(frame.ip))
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
            Op::Direct(site, n) => {
                // Exported values need their arguments and result checked,
                // unless the checker proved them plain.
                let checked = ctx.has_exports
                    && !(function.plain_values.contains(frame.ip)
                        && function.plain_inputs.contains(frame.ip));
                if checked || !direct(ctx, program, frame, stack, site, n)? {
                    return Ok(());
                }
            }
            Op::AddressBound(n, next) => {
                let own = frame.local_base + n;
                let slot = if storage.locals.data[own].is_some() {
                    step(ctx, frame)?;
                    own
                } else {
                    match bound(ctx, outer, function, frame, storage, n)? {
                        Some(slot) => slot,
                        None => return Ok(()),
                    }
                };
                let value = storage.locals.data[slot].as_ref().unwrap().clone();
                storage
                    .addresses
                    .push(ctx, Address::new(Some(slot), value))?;
                frame.ip = next;
            }
            Op::PrepareMember(_, mutating) => {
                // A hash's fields can take a member's place.
                let receiver = if mutating {
                    storage.addresses.data.last().map(|address| &address.value)
                } else {
                    stack.data.last()
                };
                if matches!(receiver.unwrap().0, Kind::Hash(_)) {
                    return Ok(());
                }
                step(ctx, frame)?;
                if mutating {
                    storage.addresses.data.last().unwrap().check_present(ctx)?;
                }
            }
            Op::Mutate(site, n) => {
                // Only direct array updates, which need no scan of their
                // result, run here.
                let direct = !(ctx.has_exports && !function.plain_values.contains(frame.ip))
                    && updatable(storage.addresses.data.last().unwrap(), site, false);
                if !direct {
                    return Ok(());
                }
                step(ctx, frame)?;
                let address = storage.addresses.data.pop().unwrap();
                let base = stack.data.len() - n;
                let name = &program.members[site.name];
                let value = update(ctx, storage, address, site, name, &stack.data[base..])?;
                stack.data.truncate(base);
                push(ctx, stack, value)?;
            }
            Op::Shovel(site) => {
                if !updatable(storage.addresses.data.last().unwrap(), site, true) {
                    return Ok(());
                }
                step(ctx, frame)?;
                let value = stack.data.pop().unwrap();
                let address = storage.addresses.data.pop().unwrap();
                let args = std::slice::from_ref(&value);
                let result = update(ctx, storage, address, site, "push", args)?;
                push(ctx, stack, result)?;
            }
            Op::AddressTarget(n, read) => {
                // An instance reads its element through its own `[]`.
                let address = storage.addresses.data.last_mut().unwrap();
                if read && matches!(address.value.0, Kind::Instance(_)) {
                    return Ok(());
                }
                step(ctx, frame)?;
                address.check_present(ctx)?;
                address.selectors.ensure(ctx, n)?;
                let base = stack.data.len() - n;
                for value in stack.data.drain(base..) {
                    ctx.charge(1)?;
                    address.selectors.data.push(value);
                }
                if read {
                    let value = address.read_target(ctx)?;
                    push(ctx, stack, value)?;
                }
            }
            Op::AddressStore => {
                // Members, instances and typed instance variables are stored
                // through setters and guards.
                let address = storage.addresses.data.last().unwrap();
                if address.member_target
                    || matches!(address.value.0, Kind::Instance(_))
                    || address.object_binding().is_some()
                {
                    return Ok(());
                }
                step(ctx, frame)?;
                let address = storage.addresses.data.pop().unwrap();
                let value = stack.data.pop().unwrap();
                let value = assign(ctx, storage, address, value)?;
                push(ctx, stack, value)?;
            }
            Op::Shadow(slot) => {
                step(ctx, frame)?;
                store(storage, frame.local_base + slot, Value::nil());
            }
            Op::BlockArg(index, autosplat) => {
                // Exported values need their depth checked when read, unless
                // the checker proved the block's parameters plain.
                if ctx.has_exports && !function.plain_values.contains(frame.ip) {
                    return Ok(());
                }
                step(ctx, frame)?;
                let value = block_arg(frame, stack, index, autosplat).map_or_else(Value::nil, copy);
                push(ctx, stack, value)?;
            }
            Op::IterNext => {
                let plain = !ctx.has_exports || function.plain_values.contains(frame.ip);
                step(ctx, frame)?;
                iterate(ctx, frame, stack, plain)?;
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
                if expanded
                    || !ctx.capability_names.data.is_empty()
                    || !ctx.options.globals.is_empty()
                {
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

/// Calls a direct builtin member on the receiver under its `count`
/// arguments, reporting whether it could; one it declines is left for the
/// general loop, uncharged.
#[inline(never)]
fn direct(
    ctx: &mut CallContext,
    program: &Program,
    frame: &mut Frame,
    stack: &mut Buffer<Value>,
    site: crate::bytecode::CallSite,
    count: usize,
) -> Result<bool> {
    let base = stack.data.len() - count - 1;
    let method = site.method.unwrap();
    if !members::direct::accepts(method, &stack.data[base], count) {
        return Ok(false);
    }
    step(ctx, frame)?;
    let name = &program.members[site.name];
    let value = members::direct::call(
        ctx,
        method,
        name,
        &stack.data[base],
        &stack.data[base + 1..],
    )?
    .unwrap();
    stack.data.truncate(base);
    stack.push(ctx, value)?;
    Ok(true)
}

/// Whether an update through `address` is a direct array update the simple
/// loop runs: of an array reached by a present path whose root needs no
/// type guard, by a member [`members::direct::update`] serves, or by `<<`
/// when `shovel`.
fn updatable(address: &Address, site: crate::bytecode::CallSite, shovel: bool) -> bool {
    matches!(address.value.0, Kind::Array(_))
        && address.capability.is_none()
        && address.exported.is_none()
        && address.object_binding().is_none()
        && !address.is_missing()
        && (shovel
            || matches!(
                site.method,
                Some(Method::Push | Method::Pop | Method::Shift | Method::Prepend | Method::Insert)
            ))
}

/// Updates the array `address` reaches by `name` with `args`, as the general
/// loop's update does, returning the call's result.
#[inline(never)]
fn update(
    ctx: &mut CallContext,
    storage: &mut Storage,
    address: Address,
    site: crate::bytecode::CallSite,
    name: &str,
    args: &[Value],
) -> Result<Value> {
    address.apply(
        ctx,
        address::Bindings {
            recover: !storage.handlers.data.is_empty(),
            guard: None,
            locals: &mut storage.locals.data,
            globals: &mut storage.globals.data,
            namespaces: &mut storage.namespaces.data,
        },
        &mut storage.addresses.data,
        |ctx, receiver| match members::direct::update(ctx, site.method, name, receiver, args) {
            Ok(result) => result,
            Err(_) => unreachable!("the simple loop updates only arrays it serves"),
        },
    )
}

/// Assigns `value` through `address`, as the general loop's store does for
/// a root that needs no type guard.
#[inline(never)]
fn assign(
    ctx: &mut CallContext,
    storage: &mut Storage,
    address: Address,
    value: Value,
) -> Result<Value> {
    address.assign(
        ctx,
        address::Bindings {
            recover: !storage.handlers.data.is_empty(),
            guard: None,
            locals: &mut storage.locals.data,
            globals: &mut storage.globals.data,
            namespaces: &mut storage.namespaces.data,
        },
        &mut storage.addresses.data,
        value,
    )
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

/// Pushes the innermost loop's next element, or leaves the loop. Unless the
/// checker proved the elements `plain`, each is scanned for host methods.
#[inline(never)]
fn iterate(
    ctx: &mut CallContext,
    frame: &mut Frame,
    stack: &mut Buffer<Value>,
    plain: bool,
) -> Result<()> {
    let state = frame.loops.data.last_mut().unwrap();
    if let Some(value) = state.next_value(ctx)? {
        if !plain {
            crate::exports::check(ctx, &value)?;
        }
        stack.push(ctx, value)?;
    } else {
        frame.ip = state.end;
    }
    Ok(())
}

/// Pushes onto the operand stack, growing it out of line only when full.
#[inline(always)]
fn push(ctx: &mut CallContext, stack: &mut Buffer<Value>, value: Value) -> Result<()> {
    if stack.data.len() < stack.data.capacity() {
        stack.data.push(value);
        return Ok(());
    }
    stack.push(ctx, value)
}

/// Moves past the current instruction and charges its step, as dispatch does.
#[inline(always)]
fn step(ctx: &mut CallContext, frame: &mut Frame) -> Result<()> {
    frame.ip += 1;
    ctx.charge(1)
}

/// Moves past a local instruction and charges it, then charges each
/// enclosing frame its lookup walked through, as [`resolve_slot`] does,
/// returning the slot the local names; none, uncharged, when the lookup
/// needs more than slots, as in a namespace initializer.
#[inline(always)]
fn local(
    ctx: &mut CallContext,
    outer: &[Frame],
    function: &Function,
    frame: &mut Frame,
    storage: &Storage,
    local: usize,
    shadowed: bool,
) -> Result<Option<usize>> {
    let own = frame.local_base + local;
    if storage.locals.data[own].is_some() {
        step(ctx, frame)?;
        return Ok(Some(own));
    }
    unbound(ctx, outer, function, frame, storage, local, shadowed)
}

/// [`local`] for a local its own slot does not bind yet. When root bindings
/// or host globals can take its place, only a capturing frame's bound slot
/// is found here.
#[inline(never)]
fn unbound(
    ctx: &mut CallContext,
    outer: &[Frame],
    function: &Function,
    frame: &mut Frame,
    storage: &Storage,
    local: usize,
    shadowed: bool,
) -> Result<Option<usize>> {
    let Some((slot, hops)) = target(outer, function, frame, storage, local)
        .filter(|&(slot, _)| !shadowed || storage.locals.data[slot].is_some())
    else {
        return Ok(None);
    };
    step(ctx, frame)?;
    ctx.charge_each(hops)?;
    Ok(Some(slot))
}

/// [`local`] for a read that falls back to other names when no slot binds
/// the local: declines, uncharged, unless a capturing frame's slot does.
#[inline(never)]
fn bound(
    ctx: &mut CallContext,
    outer: &[Frame],
    function: &Function,
    frame: &mut Frame,
    storage: &Storage,
    local: usize,
) -> Result<Option<usize>> {
    let Some((slot, hops)) = target(outer, function, frame, storage, local)
        .filter(|&(slot, _)| storage.locals.data[slot].is_some())
    else {
        return Ok(None);
    };
    step(ctx, frame)?;
    ctx.charge_each(hops)?;
    Ok(Some(slot))
}

/// Finds the slot a local of the executing frame names, and how many
/// enclosing frames [`resolve_slot`] walks through to reach it: its own slot
/// once bound or when nothing captures it, or else the capturing frame's.
/// Returns none in a namespace initializer, whose unbound locals can name
/// the declaring frame's.
#[inline(always)]
fn target(
    outer: &[Frame],
    function: &Function,
    frame: &Frame,
    storage: &Storage,
    local: usize,
) -> Option<(usize, u64)> {
    let own = frame.local_base + local;
    if storage.locals.data[own].is_some() {
        return Some((own, 0));
    }
    if function.initializer {
        return None;
    }
    match function.captures.get(local).copied().flatten() {
        None => Some((own, 0)),
        Some(capture) => Some(captured(outer, frame, storage, own, capture)),
    }
}

/// Follows a captured local out through the frames that enclose the
/// executing one, as [`resolve_slot`] does: to the first binding slot, or
/// back to the executing frame's own slot when none binds it.
#[inline(never)]
fn captured(
    outer: &[Frame],
    frame: &Frame,
    storage: &Storage,
    own: usize,
    mut capture: crate::bytecode::Capture,
) -> (usize, u64) {
    let mut hops = 0;
    let mut current = frame;
    loop {
        for _ in 0..=capture.depth {
            hops += 1;
            current = &outer[current.parent.unwrap()];
        }
        let slot = current.local_base + capture.slot;
        if storage.locals.data[slot].is_some() {
            return (slot, hops);
        }
        let function = &current.program.functions[current.function.unwrap()];
        match function.captures.get(capture.slot).copied().flatten() {
            Some(next) => capture = next,
            None => return (own, hops),
        }
    }
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
