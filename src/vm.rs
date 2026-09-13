use crate::{
    CallContext, Error, ErrorKind, HostCallback, Result, Value,
    address::{self, Address},
    arguments::{Arguments, Binding, Block},
    budget::Buffer,
    bytecode::{ArgumentOp, Invocation, Op, Program, Selection},
    hash::Hash,
    iteration::{self, Iteration, Progress},
    json, members, ops,
    range::Range,
    value::Kind,
};

struct Frame {
    function: Option<usize>,
    mutating: bool,
    ip: usize,
    iteration_base: usize,
    base: usize,
    local_base: usize,
    address_base: usize,
    bypass_base: usize,
    loops: Buffer<LoopState>,
    arguments: Buffer<Arguments>,
    binding: Buffer<Binding>,
    parent: Option<usize>,
    home: Option<usize>,
    block: Option<Block>,
    block_args: Buffer<Value>,
}

struct Storage {
    iterations: Buffer<Iteration>,
    locals: Buffer<Option<Value>>,
    addresses: Buffer<Address>,
    bypasses: Buffer<usize>,
}

struct LoopState {
    base: usize,
    address_base: usize,
    bypass_base: usize,
    argument_base: usize,
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
    hosts: &[HostCallback],
    ctx: &mut CallContext,
    function: usize,
    args: &[Value],
    keywords: &[(String, Value)],
) -> Result<Value> {
    ctx.checkpoint()?;
    let mut stack = Buffer::empty();
    let mut frames = Buffer::empty();
    let mut storage = Storage {
        iterations: Buffer::empty(),
        locals: Buffer::empty(),
        addresses: Buffer::empty(),
        bypasses: Buffer::empty(),
    };
    let mut input = Arguments::empty();
    input.options_hash = false;
    input.positional = Buffer::with_capacity(ctx, args.len())?;
    for arg in args {
        input.positional.data.push(ctx.import(arg)?);
    }
    for (name, value) in keywords {
        let key = ctx.bytes(name.as_bytes())?;
        let value = ctx.import(value)?;
        input.keywords.insert(ctx, key, value)?;
    }
    enter_arguments(program, ctx, &mut frames, &mut storage, function, input, 0)?;
    loop {
        ctx.charge(1)?;
        let current = frames.data.len() - 1;
        if frames.data[current].function.is_none() {
            let iteration = &mut storage.iterations.data[frames.data[current].iteration_base];
            let returned = if iteration.waiting {
                Some(stack.data.pop().unwrap())
            } else {
                None
            };
            match iteration.advance(ctx, returned)? {
                Progress::Yield(args, count) => {
                    let block = frames.data[current].block.unwrap();
                    enter_block(
                        program,
                        ctx,
                        &mut frames,
                        &mut storage,
                        block,
                        &args[..count],
                        stack.data.len(),
                    )?;
                }
                Progress::Done(mut value) => {
                    let mutation = iteration.mutation.take();
                    if frames.data[current].mutating {
                        let address = storage.addresses.data.pop().unwrap();
                        if let Some(mutation) = mutation {
                            value = address.apply(
                                ctx,
                                &mut storage.locals.data,
                                &mut storage.addresses.data,
                                |ctx, receiver| mutation.apply(ctx, receiver, value),
                            )?;
                        }
                    }
                    unwind(&mut frames, &mut storage, &mut stack, current);
                    stack.push(ctx, value)?;
                }
            }
            continue;
        }
        let op = {
            let frame = &mut frames.data[current];
            let op = program.functions[frame.function.unwrap()].code[frame.ip];
            frame.ip += 1;
            op
        };
        let mut slot =
            |slot, skip| resolve_slot(program, ctx, &frames, &storage, current, slot, skip);
        let op = match op {
            Op::Load(n) => Op::Load(slot(n, false)?),
            Op::Bypass(n) => Op::Bypass(slot(n, false)?),
            Op::LoadOptional(n, name) => Op::LoadOptional(slot(n, false)?, name),
            Op::Declare(n) => Op::Declare(slot(n, false)?),
            Op::Store(n) => Op::Store(slot(n, false)?),
            Op::AddStore(n) => Op::AddStore(slot(n, false)?),
            Op::AddressLocal(n) => Op::AddressLocal(slot(n, false)?),
            Op::AddressBound(n, next) => Op::AddressBound(slot(n, false)?, next),
            Op::ResolveCall(n, name) => Op::ResolveCall(slot(n, true)?, name),
            op => op,
        };
        let frame = &mut frames.data[current];
        match op {
            Op::Constant(n) => {
                let v = ctx.import(&program.constants[n])?;
                stack.push(ctx, v)?;
            }
            Op::Nil => stack.push(ctx, Value::nil())?,
            Op::Load(n) => {
                let v = storage.locals.data[n].clone().unwrap_or_default();
                stack.push(ctx, v)?;
            }
            Op::LoadOptional(slot, name) => {
                if let Some(value) = &storage.locals.data[slot] {
                    stack.push(ctx, value.clone())?;
                } else if let Some(&function) = program.names.get(&program.members[name]) {
                    enter_auto(
                        program,
                        ctx,
                        &mut frames,
                        &mut storage,
                        function,
                        stack.data.len(),
                    )?;
                } else if let Some(host) = program
                    .hosts
                    .iter()
                    .position(|h| h == &program.members[name])
                {
                    return Err(callable_value_error(&program.hosts[host], "method"));
                } else {
                    return Err(Error::new(
                        ErrorKind::Name,
                        format!("undefined variable {}", program.members[name]),
                    ));
                }
            }
            Op::Unbound(name) => {
                return Err(Error::new(
                    ErrorKind::Name,
                    format!("undefined variable {}", program.members[name]),
                ));
            }
            Op::NonCallable => {
                return Err(Error::new(
                    ErrorKind::Type,
                    "attempted to call non-callable value",
                ));
            }
            Op::Bind(param, next) => {
                if let Some(value) = frame.binding.data[0].value(ctx, param)? {
                    let slot = program.functions[frame.function.unwrap()].params[param].slot;
                    storage.locals.data[frame.local_base + slot] = Some(value);
                    frame.ip = next;
                }
            }
            Op::BindEnd => frame.binding = Buffer::empty(),
            Op::Declare(slot) => {
                storage.locals.data[slot].get_or_insert_with(Value::nil);
            }
            Op::Bypass(slot) => storage.bypasses.push(ctx, slot)?,
            Op::BypassEnd(n) => {
                storage
                    .bypasses
                    .data
                    .truncate(storage.bypasses.data.len() - n);
            }
            Op::Shadow(slot) => storage.locals.data[frame.local_base + slot] = Some(Value::nil()),
            Op::BlockArg(index, autosplat) => {
                let args = &frame.block_args.data;
                let args = if autosplat && args.len() == 1 {
                    args[0].as_array().unwrap_or(args)
                } else {
                    args.as_slice()
                };
                stack.push(ctx, args.get(index).cloned().unwrap_or_default())?;
            }
            Op::Attach(function) => {
                frame.arguments.data.last_mut().unwrap().block = Some(Block {
                    function,
                    parent: current,
                });
            }
            Op::BlockGiven(arguments, block) => {
                if arguments || block {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        if arguments {
                            "block_given? takes no arguments"
                        } else {
                            "block_given? does not accept a block"
                        },
                    ));
                }
                stack.push(ctx, Value::boolean(frame.block.is_some()))?;
            }
            Op::CheckBlock => {
                if frame.block.is_none() {
                    return Err(Error::new(ErrorKind::Argument, "no block given"));
                }
            }
            Op::Yield(n) => {
                let block = frame.block.unwrap();
                let base = stack.data.len() - n;
                enter_block(
                    program,
                    ctx,
                    &mut frames,
                    &mut storage,
                    block,
                    &stack.data[base..],
                    base,
                )?;
                stack.data.truncate(base);
            }
            Op::Store(n) => {
                let value = stack.data.last().unwrap();
                address::refresh(ctx, n, value, &mut storage.addresses.data, &[])?;
                storage.locals.data[n] = Some(value.clone());
            }
            Op::Pop => {
                stack.data.pop().unwrap();
            }
            Op::Dup => {
                let v = stack.data.last().unwrap().clone();
                stack.push(ctx, v)?;
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
                storage.locals.data[n] = None;
                let value = ops::binary(ctx, "+", a, b)?;
                address::refresh(ctx, n, &value, &mut storage.addresses.data, &[])?;
                storage.locals.data[n] = Some(value.clone());
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
            Op::AddressLocal(n) => {
                let value = storage.locals.data[n].clone().unwrap_or_default();
                storage.addresses.push(ctx, Address::new(Some(n), value))?;
            }
            Op::AddressBound(slot, next) => {
                if let Some(value) = &storage.locals.data[slot] {
                    storage
                        .addresses
                        .push(ctx, Address::new(Some(slot), value.clone()))?;
                    frame.ip = next;
                }
            }
            Op::AddressValue => {
                let value = stack.data.pop().unwrap();
                storage.addresses.push(ctx, Address::new(None, value))?;
            }
            Op::AddressIndex(n) => {
                let base = stack.data.len() - n;
                storage
                    .addresses
                    .data
                    .last_mut()
                    .unwrap()
                    .index(ctx, &stack.data[base..])?;
                stack.data.truncate(base);
            }
            Op::AddressMember(site) => {
                let name = &program.members[site.name];
                let address = storage.addresses.data.last_mut().unwrap();
                let key = ctx.bytes(name.as_bytes())?;
                let data = if let Kind::Hash(hash) = &address.value.0 {
                    hash.find(ctx, name.as_bytes())?.is_some()
                } else {
                    false
                };
                if data {
                    address.index(ctx, &[key])?;
                } else {
                    let address = storage.addresses.data.pop().unwrap();
                    let value = address.apply(
                        ctx,
                        &mut storage.locals.data,
                        &mut storage.addresses.data,
                        |ctx, receiver| members::call(ctx, site, name, receiver, &[]),
                    )?;
                    storage.addresses.push(ctx, Address::new(None, value))?;
                }
            }
            Op::AddressMemberTarget(site, read) => {
                let address = storage.addresses.data.last_mut().unwrap();
                let name = &program.members[site.name];
                let key = ctx.bytes(name.as_bytes())?;
                address.selectors.push(ctx, key)?;
                if read {
                    let (_, value) = members::call(ctx, site, name, address.value.clone(), &[])?;
                    stack.push(ctx, value)?;
                }
            }
            Op::AddressTarget(n, read) => {
                let base = stack.data.len() - n;
                let address = storage.addresses.data.last_mut().unwrap();
                address.selectors.ensure(ctx, n)?;
                for value in stack.data.drain(base..) {
                    ctx.charge(1)?;
                    address.selectors.data.push(value);
                }
                if read {
                    let value = address.read_target(ctx)?;
                    stack.push(ctx, value)?;
                }
            }
            Op::AddressStore => {
                let address = storage.addresses.data.pop().unwrap();
                let value = stack.data.pop().unwrap();
                let value = address.assign(
                    ctx,
                    &mut storage.locals.data,
                    &mut storage.addresses.data,
                    value,
                )?;
                stack.push(ctx, value)?;
            }
            Op::AddressDrop => {
                storage.addresses.data.pop().unwrap();
            }
            Op::Mutate(site, n) => {
                let base = stack.data.len() - n;
                let address = storage.addresses.data.pop().unwrap();
                let value = address.apply(
                    ctx,
                    &mut storage.locals.data,
                    &mut storage.addresses.data,
                    |ctx, receiver| {
                        members::call(
                            ctx,
                            site,
                            &program.members[site.name],
                            receiver,
                            &stack.data[base..],
                        )
                    },
                )?;
                stack.data.truncate(base);
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
                        address_base: storage.addresses.data.len(),
                        bypass_base: storage.bypasses.data.len(),
                        argument_base: frame.arguments.data.len(),
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
                storage.addresses.data.truncate(state.address_base);
                storage.bypasses.data.truncate(state.bypass_base);
                frame.arguments.data.truncate(state.argument_base);
                frame.ip = state.next;
            }
            Op::LoopEnd => {
                let state = frame.loops.data.pop().unwrap();
                stack.data.truncate(state.base);
                storage.addresses.data.truncate(state.address_base);
                storage.bypasses.data.truncate(state.bypass_base);
                frame.arguments.data.truncate(state.argument_base);
                stack.push(ctx, state.result())?;
            }
            Op::Break(has_value) => {
                if frame.loops.data.is_empty() {
                    if frame.parent.is_none() {
                        return Err(Error::new(ErrorKind::Argument, "break outside loop"));
                    }
                    let target = (0..current)
                        .rev()
                        .find(|&index| {
                            !frames.data[index].loops.data.is_empty()
                                || frames.data[index].parent.is_none()
                        })
                        .ok_or_else(|| Error::new(ErrorKind::Argument, "break outside loop"))?;
                    let value = if has_value {
                        stack.data.pop().unwrap()
                    } else {
                        Value::nil()
                    };
                    if !frames.data[target].loops.data.is_empty() {
                        unwind(&mut frames, &mut storage, &mut stack, target + 1);
                        let frame = &mut frames.data[target];
                        let state = frame.loops.data.last_mut().unwrap();
                        state.broken = true;
                        state.break_value = has_value.then_some(value);
                        stack.data.truncate(state.base);
                        storage.addresses.data.truncate(state.address_base);
                        storage.bypasses.data.truncate(state.bypass_base);
                        frame.arguments.data.truncate(state.argument_base);
                        frame.ip = state.end;
                        continue;
                    }
                    if frames.data[target].block.is_none() {
                        return Err(Error::new(ErrorKind::Argument, "break outside loop"));
                    }
                    unwind(&mut frames, &mut storage, &mut stack, target);
                    if frames.data.is_empty() {
                        ctx.checkpoint()?;
                        return Ok(value);
                    }
                    stack.push(ctx, value)?;
                    continue;
                }
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
                storage.addresses.data.truncate(state.address_base);
                storage.bypasses.data.truncate(state.bypass_base);
                frame.arguments.data.truncate(state.argument_base);
                frame.ip = state.end;
            }
            Op::Next(has_value) => {
                if frame.loops.data.is_empty() && frame.parent.is_some() {
                    let value = if has_value {
                        stack.data.pop().unwrap()
                    } else {
                        Value::nil()
                    };
                    unwind(&mut frames, &mut storage, &mut stack, current);
                    stack.push(ctx, value)?;
                    continue;
                }
                let state = frame
                    .loops
                    .data
                    .last()
                    .ok_or_else(|| Error::new(ErrorKind::Argument, "next outside loop"))?;
                stack.data.truncate(state.base);
                storage.addresses.data.truncate(state.address_base);
                storage.bypasses.data.truncate(state.bypass_base);
                frame.arguments.data.truncate(state.argument_base);
                frame.ip = state.next;
            }
            Op::Call(function, n) => {
                let base = stack.data.len() - n;
                enter(
                    program,
                    ctx,
                    &mut frames,
                    &mut storage,
                    function,
                    &stack.data[base..],
                    base,
                )?;
                stack.data.truncate(base);
            }
            Op::AutoCall(function) => {
                enter_auto(
                    program,
                    ctx,
                    &mut frames,
                    &mut storage,
                    function,
                    stack.data.len(),
                )?;
            }
            Op::HostValue(host) => {
                return Err(callable_value_error(&program.hosts[host], "method"));
            }
            Op::Arguments => frame.arguments.push(ctx, Arguments::empty())?,
            Op::ResolveCall(slot, name) => {
                let name = &program.members[name];
                let target = if storage.locals.data.get(slot).is_some_and(Option::is_some) {
                    Invocation::NonCallable
                } else if let Some(&function) = program.names.get(name) {
                    Invocation::Function(function)
                } else if let Some(host) = program.hosts.iter().position(|h| h == name) {
                    Invocation::Host(host)
                } else {
                    return Err(Error::new(
                        ErrorKind::Name,
                        format!("undefined variable {name}"),
                    ));
                };
                let mut arguments = Arguments::empty();
                arguments.target = Some(target);
                frame.arguments.push(ctx, arguments)?;
            }
            Op::Argument(op) => {
                let value = stack.data.pop().unwrap();
                let name = if let ArgumentOp::Keyword(name) = op {
                    &program.members[name]
                } else {
                    ""
                };
                frame
                    .arguments
                    .data
                    .last_mut()
                    .unwrap()
                    .push(ctx, op, name, value)?;
            }
            Op::Invoke(target) => {
                let args = frame.arguments.data.pop().unwrap();
                let target = if matches!(target, Invocation::Resolved) {
                    args.target.unwrap()
                } else {
                    target
                };
                match target {
                    Invocation::Function(function) => {
                        enter_arguments(
                            program,
                            ctx,
                            &mut frames,
                            &mut storage,
                            function,
                            args,
                            stack.data.len(),
                        )?;
                    }
                    Invocation::Host(host) => {
                        ctx.checkpoint()?;
                        let result =
                            hosts[host](ctx, &args.positional.data, &args.keywords.buffer.data);
                        ctx.checkpoint()?;
                        let value = ctx.import(&result?)?;
                        stack.push(ctx, value)?;
                    }
                    Invocation::Member(site, mutating) => {
                        let name = &program.members[site.name];
                        let receiver = if mutating {
                            &storage.addresses.data.last().unwrap().value
                        } else {
                            stack.data.last().unwrap()
                        };
                        let arity = args
                            .block
                            .map(|block| program.functions[block.function].block_arity);
                        if let Some(iteration) = iteration::start(
                            ctx,
                            name,
                            receiver,
                            &args.positional.data,
                            !args.keywords.buffer.data.is_empty(),
                            arity,
                        )? {
                            if !mutating {
                                stack.data.pop();
                            }
                            ctx.charge(1)?;
                            if frames.data.len() >= ctx.options.limits.recursion {
                                return ctx.fail(ErrorKind::Recursion, "recursion limit exceeded");
                            }
                            let mut frame =
                                new_frame(ctx, program, &mut storage, None, stack.data.len())?;
                            frame.mutating = mutating;
                            if mutating {
                                // The native frame owns this address, including on nonlocal exits.
                                frame.address_base -= 1;
                            }
                            frame.block = args.block;
                            frame.arguments.push(ctx, args)?;
                            storage.iterations.push(ctx, iteration)?;
                            frames.push(ctx, frame)?;
                            continue;
                        }
                        let value = if mutating {
                            let address = storage.addresses.data.pop().unwrap();
                            address.apply(
                                ctx,
                                &mut storage.locals.data,
                                &mut storage.addresses.data,
                                |ctx, receiver| {
                                    members::call_keywords(ctx, site, name, receiver, &args)
                                },
                            )?
                        } else {
                            let receiver = stack.data.pop().unwrap();
                            members::call_keywords(ctx, site, name, receiver, &args)?.1
                        };
                        stack.push(ctx, value)?;
                    }
                    Invocation::Json(parse) => {
                        if !args.keywords.buffer.data.is_empty() {
                            return Err(Error::new(
                                ErrorKind::Argument,
                                "JSON methods do not accept keyword arguments",
                            ));
                        }
                        ops::arity(&args.positional.data, 1)?;
                        let value = &args.positional.data[0];
                        let result = if parse {
                            json::parse(ctx, value.require_bytes()?)?
                        } else {
                            json::stringify(ctx, value)?
                        };
                        stack.push(ctx, result)?;
                    }
                    Invocation::NonCallable => {
                        return Err(Error::new(
                            ErrorKind::Type,
                            "attempted to call non-callable value",
                        ));
                    }
                    Invocation::Resolved => unreachable!(),
                }
            }
            Op::Host(host, n) => {
                let base = stack.data.len() - n;
                ctx.checkpoint()?;
                let result = hosts[host](ctx, &stack.data[base..], &[]);
                ctx.checkpoint()?;
                let result = result?;
                let value = ctx.import(&result)?;
                stack.data.truncate(base);
                stack.push(ctx, value)?;
            }
            Op::Method(site, n) => {
                let base = stack.data.len() - n - 1;
                let root = std::mem::take(&mut stack.data[base]);
                let (_, value) = members::call(
                    ctx,
                    site,
                    &program.members[site.name],
                    root,
                    &stack.data[base + 1..],
                )?;
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
            Op::Return | Op::Finish => {
                let value = stack.data.pop().unwrap();
                let target = if matches!(op, Op::Return) && frame.parent.is_some() {
                    frame
                        .home
                        .ok_or_else(|| Error::new(ErrorKind::Argument, "unexpected return"))?
                } else {
                    current
                };
                unwind(&mut frames, &mut storage, &mut stack, target);
                if frames.data.is_empty() {
                    ctx.checkpoint()?;
                    return Ok(value);
                }
                stack.push(ctx, value)?;
            }
        }
    }
}

fn callable_value_error(name: &str, kind: &str) -> Error {
    Error::new(
        ErrorKind::Type,
        format!("{name} is a {kind} and cannot be used as a value; call it with {name}(...)"),
    )
}

fn enter_auto(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    function: usize,
    base: usize,
) -> Result<()> {
    let fun = &program.functions[function];
    if !fun.params.is_empty() {
        return Err(callable_value_error(&fun.name, "function"));
    }
    enter(program, ctx, frames, storage, function, &[], base)
}

fn enter(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    function: usize,
    args: &[Value],
    base: usize,
) -> Result<()> {
    let fun = &program.functions[function];
    if !fun.plain {
        let args = Arguments::from_values(ctx, args)?;
        return enter_arguments(program, ctx, frames, storage, function, args, base);
    }
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.fail(ErrorKind::Recursion, "recursion limit exceeded");
    }
    if args.len() != fun.params.len() {
        return Err(Error::new(
            ErrorKind::Argument,
            format!(
                "{} expects {} arguments, got {}",
                fun.name,
                fun.params.len(),
                args.len()
            ),
        ));
    }
    let mut frame = new_frame(ctx, program, storage, Some(function), base)?;
    frame.home = (function != 0).then_some(frames.data.len());
    for (param, arg) in fun.params.iter().zip(args) {
        ctx.charge(1)?;
        storage.locals.data[frame.local_base + param.slot] = Some(arg.clone());
    }
    frames.push(ctx, frame)
}

fn enter_arguments(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    function: usize,
    arguments: Arguments,
    base: usize,
) -> Result<()> {
    let fun = &program.functions[function];
    if fun.plain && arguments.keywords.buffer.data.is_empty() {
        enter(
            program,
            ctx,
            frames,
            storage,
            function,
            &arguments.positional.data,
            base,
        )?;
        frames.data.last_mut().unwrap().block = arguments.block;
        return Ok(());
    }
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.fail(ErrorKind::Recursion, "recursion limit exceeded");
    }
    let block = arguments.block;
    let binding = Binding::new(ctx, &fun.params, arguments)?;
    let mut frame = new_frame(ctx, program, storage, Some(function), base)?;
    frame.home = (function != 0).then_some(frames.data.len());
    frame.block = block;
    if fun.defaults {
        frame.binding = Buffer::with_capacity(ctx, 1)?;
        frame.binding.data.push(binding);
    } else {
        for (i, param) in fun.params.iter().enumerate() {
            storage.locals.data[frame.local_base + param.slot] = binding.value(ctx, i)?;
        }
    }
    frames.push(ctx, frame)
}

fn new_frame(
    ctx: &mut CallContext,
    program: &Program,
    storage: &mut Storage,
    function: Option<usize>,
    base: usize,
) -> Result<Frame> {
    let local_base = storage.locals.data.len();
    let locals = function.map_or(0, |index| program.functions[index].locals);
    let Some(capacity) = local_base.checked_add(locals) else {
        return ctx.fail(ErrorKind::Memory, "allocation size overflow");
    };
    storage.locals.ensure(ctx, capacity)?;
    for _ in 0..locals {
        ctx.charge(1)?;
        storage.locals.data.push(None);
    }
    Ok(Frame {
        function,
        mutating: false,
        ip: 0,
        iteration_base: storage.iterations.data.len(),
        base,
        local_base,
        address_base: storage.addresses.data.len(),
        bypass_base: storage.bypasses.data.len(),
        loops: Buffer::empty(),
        arguments: Buffer::empty(),
        binding: Buffer::empty(),
        parent: None,
        home: None,
        block: None,
        block_args: Buffer::empty(),
    })
}

fn resolve_slot(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    mut frame: usize,
    mut slot: usize,
    skip: bool,
) -> Result<usize> {
    let own = frames.data[frame].local_base + slot;
    loop {
        let local = frames.data[frame].local_base + slot;
        if storage.locals.data[local].is_some() {
            if !skip {
                return Ok(local);
            }
            ctx.charge(storage.bypasses.data.len() as u64)?;
            if !storage.bypasses.data.contains(&local) {
                return Ok(local);
            }
        }
        let Some(capture) = program.functions[frames.data[frame].function.unwrap()]
            .captures
            .get(slot)
            .copied()
            .flatten()
        else {
            return Ok(if skip { usize::MAX } else { own });
        };
        for _ in 0..=capture.depth {
            ctx.charge(1)?;
            frame = frames.data[frame].parent.unwrap();
        }
        slot = capture.slot;
    }
}

fn enter_block(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    block: Block,
    args: &[Value],
    base: usize,
) -> Result<()> {
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.fail(ErrorKind::Recursion, "recursion limit exceeded");
    }
    let mut frame = new_frame(ctx, program, storage, Some(block.function), base)?;
    frame.parent = Some(block.parent);
    frame.home = frames.data[block.parent].home;
    frame.block = frames.data[block.parent].block;
    frame.block_args.extend(ctx, args)?;
    frames.push(ctx, frame)
}

fn unwind(
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    target: usize,
) {
    let frame = &frames.data[target];
    stack.data.truncate(frame.base);
    storage.locals.data.truncate(frame.local_base);
    storage.iterations.data.truncate(frame.iteration_base);
    storage.addresses.data.truncate(frame.address_base);
    storage.bypasses.data.truncate(frame.bypass_base);
    frames.data.truncate(target);
}
