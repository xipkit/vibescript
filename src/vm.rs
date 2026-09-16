use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    address::{self, Address},
    arguments::{Arguments, Binding, Block},
    budget::Buffer,
    builtin::Global,
    bytecode::{ArgumentOp, Invocation, Op, Selection},
    hash::Hash,
    iteration::{self, Iteration, Progress},
    members, ops,
    range::Range,
    value::Kind,
};
use std::sync::Arc;

mod call_targets;
mod dispatch;
mod file_bindings;
#[cfg(test)]
mod file_bindings_tests;
mod format;
mod handlers;
mod namespaces;
mod operators;
mod output;
mod programs;
mod scopes;
#[cfg(test)]
mod scopes_tests;
use handlers::{Control, Event};
pub(crate) use programs::Program;

#[derive(Default)]
enum ReturnTo {
    #[default]
    Stack,
    Address,
    Assigned(Value),
    Local(usize),
    Negate,
    Text(Value),
    Output,
    Format,
}

struct Frame {
    program: Arc<Program>,
    activation: bool,
    receiver: Option<Value>,
    constructor: bool,
    return_to: ReturnTo,
    function: Option<usize>,
    mutating: bool,
    ip: usize,
    iteration_base: usize,
    base: usize,
    local_base: usize,
    address_base: usize,
    bypass_base: usize,
    text_base: usize,
    loops: Buffer<LoopState>,
    arguments: Buffer<Arguments>,
    binding: Buffer<Binding>,
    parent: Option<usize>,
    home: Option<usize>,
    block: Option<Block>,
    block_args: Buffer<Value>,
}

struct Storage {
    programs: Buffer<programs::Entry>,
    activations: Buffer<programs::Activation>,
    discovered: usize,
    handlers: Buffer<handlers::Handler>,
    namespaces: Buffer<crate::namespace::State>,
    declarations: Buffer<((usize, usize), Value)>,
    texts: Buffer<Buffer<u8>>,
    iterations: Buffer<Iteration>,
    globals: Buffer<Option<Value>>,
    ambient_globals: Buffer<(Global, usize)>,
    locals: Buffer<Option<Value>>,
    addresses: Buffer<Address>,
    bypasses: Buffer<usize>,
}

struct LoopState {
    base: usize,
    address_base: usize,
    bypass_base: usize,
    argument_base: usize,
    text_base: usize,
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
    code: &Arc<crate::code::Code>,
    ctx: &mut CallContext,
    function: usize,
    args: &[Value],
    keywords: &[(String, Value)],
) -> Result<Value> {
    ctx.checkpoint()?;
    let mut stack = Buffer::empty();
    let mut frames = Buffer::empty();
    let mut storage = Storage {
        programs: Buffer::empty(),
        activations: Buffer::empty(),
        discovered: 0,
        handlers: Buffer::empty(),
        namespaces: Buffer::empty(),
        declarations: Buffer::empty(),
        texts: Buffer::empty(),
        iterations: Buffer::empty(),
        globals: Buffer::empty(),
        ambient_globals: Buffer::empty(),
        locals: Buffer::empty(),
        addresses: Buffer::empty(),
        bypasses: Buffer::empty(),
    };
    ctx.enum_rebind.definitions =
        (!code.program.file).then(|| code.program.enum_definitions.clone());
    ctx.enum_rebind.active = true;
    let result = (|| -> Result<Value> {
        let environment = code
            .program
            .file
            .then(|| crate::objects::environment(ctx))
            .transpose()?;
        let root = programs::load(ctx, &mut storage, code, environment.as_ref())?;
        let program = &*root;
        let mut active = root.clone();
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
        ctx.enum_rebind.active = false;
        programs::arguments(ctx, &mut storage, &input)?;
        let mut pending_entry = Some((function, input));
        let mut initializer = if function == 0 && !program.file {
            program.namespaces.len()
        } else {
            0
        };
        loop {
            let event = (|| -> Result<Event> {
                loop {
                    programs::advance(ctx, &mut frames, &mut storage, stack.data.len())?;
                    if frames.data.is_empty() {
                        while initializer < program.namespaces.len() {
                            let module = initializer;
                            initializer += 1;
                            if let Some(body) = program.namespaces[module].body {
                                let state = namespaces::state(program, ctx, &mut storage, module)?;
                                if !storage.namespaces.data[state].initialized {
                                    enter_arguments(
                                        program,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        body,
                                        Arguments::empty(),
                                        0,
                                    )?;
                                    break;
                                }
                            }
                        }
                        if frames.data.is_empty() {
                            let (function, input) = pending_entry.take().unwrap();
                            enter_arguments(
                                program,
                                ctx,
                                &mut frames,
                                &mut storage,
                                function,
                                input,
                                0,
                            )?;
                        }
                    }
                    let current = frames.data.len() - 1;
                    if !Arc::ptr_eq(&active, &frames.data[current].program) {
                        active = frames.data[current].program.clone();
                    }
                    let program = &*active;
                    let hosts = &program.code.hosts;
                    if frames.data[current].function.is_none() {
                        ctx.charge(1)?;
                        let iteration =
                            &mut storage.iterations.data[frames.data[current].iteration_base];
                        let returned = if iteration.waiting() {
                            Some(stack.data.pop().unwrap())
                        } else {
                            None
                        };
                        match iteration.advance(ctx, returned)? {
                            Progress::Yield(args, count) => {
                                let block = frames.data[current].block.unwrap();
                                enter_block(
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    block,
                                    &args[..count],
                                    stack.data.len(),
                                )?;
                            }
                            Progress::Call(receiver, operation, argument) => {
                                dispatch::reduce(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    &mut stack,
                                    [receiver, operation, argument],
                                )?;
                            }
                            Progress::Done(mut value) => {
                                let mutation = iteration.take_mutation();
                                if frames.data[current].mutating {
                                    let address = storage.addresses.data.pop().unwrap();
                                    if let Some(mutation) = mutation {
                                        let guard_program =
                                            programs::address(ctx, &mut storage, &address)?;
                                        let guard = address_guard(
                                            guard_program.as_deref(),
                                            ctx,
                                            &frames,
                                            &mut storage,
                                            &address,
                                        )?;
                                        value = address.apply(
                                            ctx,
                                            address::Bindings {
                                                recover: !storage.handlers.data.is_empty(),
                                                guard,
                                                locals: &mut storage.locals.data,
                                                globals: &mut storage.globals.data,
                                                namespaces: &mut storage.namespaces.data,
                                            },
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
                    ctx.charge(1)?;
                    let mut slot =
                        |slot, skip| resolve_slot(ctx, &frames, &storage, current, slot, skip);
                    let file_local = if program.file {
                        file_bindings::local_name(op)
                    } else {
                        None
                    };
                    let op = match op {
                        Op::Load(n) => Op::Load(slot(n, false)?),
                        Op::Bypass(n) => Op::Bypass(slot(n, false)?),
                        Op::LoadOptional(n, name) => Op::LoadOptional(slot(n, false)?, name),
                        Op::ReceiverBound(n, next) => Op::ReceiverBound(slot(n, false)?, next),
                        Op::Declare(n) => Op::Declare(slot(n, false)?),
                        Op::Store(n) => Op::Store(slot(n, false)?),
                        Op::AddStore(n) => Op::AddStore(slot(n, false)?),
                        Op::AddressLocal(n) => Op::AddressLocal(slot(n, false)?),
                        Op::AddressBound(n, next) => Op::AddressBound(slot(n, false)?, next),
                        Op::ResolveCall(n, name, parenthesized) => Op::ResolveCall(
                            if n == usize::MAX { n } else { slot(n, true)? },
                            name,
                            parenthesized,
                        ),
                        Op::CallName(n, name) => {
                            Op::CallName(if n == usize::MAX { n } else { slot(n, false)? }, name)
                        }
                        op => op,
                    };
                    let file_local = if let Some(relative) = file_local {
                        let absolute = file_bindings::local_name(op).unwrap();
                        file_bindings::local(
                            program, ctx, &frames, &storage, current, relative, absolute,
                        )?
                        .then_some(
                            program.functions[frames.data[current].function.unwrap()].local_names
                                [relative]
                                .as_str(),
                        )
                    } else {
                        None
                    };
                    let frame = &mut frames.data[current];
                    let namespace = frame
                        .function
                        .and_then(|index| program.functions[index].namespace);
                    let self_value = frame.receiver.clone();
                    let caller_instance = matches!(&self_value, Some(Value(Kind::Instance(_))));
                    match op {
                        Op::TryBegin(spec) => {
                            handlers::begin(ctx, &frames, &mut storage, &stack, spec)?
                        }
                        Op::TryBody | Op::TryEnd => handlers::normal(
                            ctx,
                            &mut frames,
                            &mut storage,
                            &mut stack,
                            matches!(op, Op::TryBody),
                        )?,
                        Op::EnsureEnd => {
                            if let Some(event) =
                                handlers::end_ensure(&mut frames, &mut storage, &mut stack, ctx)?
                            {
                                return Ok(event);
                            }
                        }
                        Op::Retry => {
                            return Ok(Event::Control(handlers::retry(&storage)?));
                        }
                        Op::RaiseStart(named, target) => {
                            let class = if let Some((name, slot)) = named {
                                let local = if let Some(slot) = slot {
                                    let slot =
                                        resolve_slot(ctx, &frames, &storage, current, slot, false)?;
                                    storage.locals.data[slot].is_some()
                                } else {
                                    false
                                };
                                let name = &program.members[name];
                                let bound = local
                                    || runtime_bound(
                                        program, ctx, &frames, &storage, current, name,
                                    )?
                                    || handlers::class_constant_bound(
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        current,
                                        name,
                                    )?;
                                if bound {
                                    None
                                } else {
                                    crate::ErrorClass::from_name(name)
                                }
                            } else {
                                None
                            };
                            let frame = &mut frames.data[current];
                            let mut args = Arguments::empty();
                            args.target =
                                Some(crate::arguments::Target::Raise(class, Value::nil()));
                            frame.arguments.push(ctx, args)?;
                            if class.is_some() {
                                frame.ip = target;
                            }
                        }
                        Op::RaiseValue => {
                            let args = frame.arguments.data.last_mut().unwrap();
                            args.target = Some(crate::arguments::Target::Raise(
                                None,
                                stack.data.pop().unwrap(),
                            ));
                        }
                        Op::Raise(count) => {
                            if count == 0 {
                                if let Some(error) = handlers::current_error(&storage) {
                                    return Ok(Event::Error(error));
                                }
                                return Err(Error::new(ErrorKind::Runtime, ""));
                            }
                            let message = stack.data.pop().unwrap();
                            if count == 1 {
                                return Err(handlers::raise(ctx, None, message, false)?);
                            }
                            let target = frame.arguments.data.pop().unwrap().target.unwrap();
                            let crate::arguments::Target::Raise(class, value) = target else {
                                unreachable!()
                            };
                            let class = class.or_else(|| handlers::class(&value));
                            return Err(handlers::raise(ctx, class, message, true)?);
                        }
                        Op::UnboundClass(name) => {
                            return Err(Error::new(
                                ErrorKind::Name,
                                format!("class {} is not bound", program.members[name]),
                            ));
                        }
                        Op::BindIvar(name, local) => {
                            let Some(Value(Kind::Instance(instance))) = frame.receiver.as_ref()
                            else {
                                return Err(Error::new(
                                    ErrorKind::Name,
                                    "no instance context for ivar parameter",
                                ));
                            };
                            let instance = instance.clone();
                            let slot = frame.local_base + local;
                            let value = storage.locals.data[slot].as_ref().unwrap().clone();
                            let value = normalize_ivar(
                                ctx,
                                &frames,
                                &mut storage,
                                &instance,
                                &program.members[name],
                                value,
                            )?;
                            set_ivar(ctx, &mut storage, &instance, &program.members[name], &value)?;
                            storage.locals.data[slot] = Some(value);
                        }
                        Op::InitNamespace(module) => {
                            let state = namespaces::state(program, ctx, &mut storage, module)?;
                            if !storage.namespaces.data[state].initialized {
                                let body = program.namespaces[module].body.unwrap();
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    body,
                                    Arguments::empty(),
                                    stack.data.len(),
                                )?;
                                frames.data.last_mut().unwrap().parent = Some(current);
                            }
                        }
                        Op::AmbientValue(name, next) | Op::AmbientAddress(name, next) => {
                            let name = &program.members[name];
                            let address = matches!(op, Op::AmbientAddress(..));
                            if !address
                                || !name
                                    .chars()
                                    .next()
                                    .is_some_and(crate::syntax::unicode::upper)
                            {
                                if let Some(slot) =
                                    namespaces::ambient_slot(ctx, &frames, &storage, current, name)?
                                {
                                    let value = storage.locals.data[slot].as_ref().unwrap().clone();
                                    if address {
                                        storage
                                            .addresses
                                            .push(ctx, Address::new(Some(slot), value))?;
                                    } else {
                                        stack.push(ctx, value)?;
                                    }
                                    frames.data[current].ip = next;
                                }
                            }
                        }
                        Op::FileValue(name, next) => {
                            if let Some(mut value) =
                                file_bindings::get(program, ctx, &program.members[name])?
                            {
                                if let Kind::Offset(offset) = &value.0 {
                                    return Err(offset.value_error());
                                }
                                if let Kind::Builtin(builtin) = value.0 {
                                    value = builtin.read(ctx)?;
                                }
                                stack.push(ctx, value)?;
                                frame.ip = next;
                            }
                        }
                        Op::FileAddress(name, next) => {
                            let name = &program.members[name];
                            if file_bindings::unshadowed(
                                program,
                                ctx,
                                &frames,
                                &mut storage,
                                current,
                                name,
                            )? && file_bindings::get(program, ctx, name)?.is_some()
                            {
                                let address = file_bindings::address(program, ctx, name)?;
                                storage.addresses.push(ctx, address)?;
                                frames.data[current].ip = next;
                            }
                        }
                        Op::NamespaceSelf(module) => {
                            let value = if let Some(value) = &self_value {
                                value.clone()
                            } else {
                                namespaces::value(program, ctx, &mut storage, module)?
                            };
                            stack.push(ctx, value)?;
                        }
                        Op::NamespaceConstant(name, next) => {
                            if let Some(value) = namespaces::field(
                                program,
                                ctx,
                                &mut storage,
                                namespace.unwrap(),
                                &program.members[name],
                            )? {
                                stack.push(ctx, value)?;
                                frames.data[current].ip = next;
                            }
                        }
                        Op::NamespaceVariable(name, optional) => {
                            let raw = &program.members[name];
                            if raw.starts_with('@') && !raw.starts_with("@@") {
                                let Some(Value(Kind::Instance(instance))) = &self_value else {
                                    return Err(Error::new(
                                        ErrorKind::Name,
                                        "no instance context for ivar",
                                    ));
                                };
                                let value = crate::objects::field(ctx, instance, &raw[1..])?
                                    .unwrap_or_default();
                                stack.push(ctx, value)?;
                                continue;
                            }
                            let (module, name) =
                                namespaces::variable_name(namespace, &program.members[name])?;
                            let value =
                                namespaces::field(program, ctx, &mut storage, module, name)?;
                            let value = if optional {
                                value.unwrap_or_default()
                            } else {
                                value.ok_or_else(|| {
                                    Error::new(ErrorKind::Name, "undefined class variable")
                                })?
                            };
                            stack.push(ctx, value)?;
                        }
                        Op::NamespaceAddress(name, optional) => {
                            let raw = &program.members[name];
                            if raw.starts_with('@') && !raw.starts_with("@@") {
                                let Some(Value(Kind::Instance(instance))) = &self_value else {
                                    return Err(Error::new(
                                        ErrorKind::Name,
                                        "no instance context for ivar",
                                    ));
                                };
                                let address = crate::objects::address(ctx, instance, &raw[1..])?;
                                storage.addresses.push(ctx, address)?;
                                continue;
                            }
                            let (module, name) =
                                namespaces::variable_name(namespace, &program.members[name])?;
                            let address = if !optional
                                && namespaces::field(program, ctx, &mut storage, module, name)?
                                    .is_none()
                            {
                                if let Some(slot) =
                                    namespaces::ambient_slot(ctx, &frames, &storage, current, name)?
                                {
                                    Address::new(
                                        Some(slot),
                                        storage.locals.data[slot].as_ref().unwrap().clone(),
                                    )
                                } else if let Some(&index) = program.declaration_names.get(name) {
                                    Address::new(
                                        None,
                                        declaration_value(program, ctx, &mut storage, index)?,
                                    )
                                } else if let Some(global) = global_index(program, name) {
                                    file_bindings::global_address(
                                        program,
                                        ctx,
                                        &mut storage,
                                        global,
                                        false,
                                    )?
                                } else {
                                    return Err(Error::new(
                                        ErrorKind::Name,
                                        "undefined class constant",
                                    ));
                                }
                            } else {
                                namespaces::address(
                                    program,
                                    ctx,
                                    &mut storage,
                                    module,
                                    name,
                                    optional,
                                )?
                            };
                            storage.addresses.push(ctx, address)?;
                        }
                        Op::NamespaceStore(name) => {
                            let raw = &program.members[name];
                            if raw.starts_with('@') && !raw.starts_with("@@") {
                                let Some(Value(Kind::Instance(instance))) = &self_value else {
                                    return Err(Error::new(
                                        ErrorKind::Name,
                                        "no instance context for ivar",
                                    ));
                                };
                                let value = normalize_ivar(
                                    ctx,
                                    &frames,
                                    &mut storage,
                                    instance,
                                    &raw[1..],
                                    stack.data.last().unwrap().clone(),
                                )?;
                                set_ivar(ctx, &mut storage, instance, &raw[1..], &value)?;
                                *stack.data.last_mut().unwrap() = value;
                                continue;
                            }
                            let (module, name) =
                                namespaces::variable_name(namespace, &program.members[name])?;
                            namespaces::set(
                                program,
                                ctx,
                                &mut storage,
                                module,
                                name,
                                stack.data.last().unwrap().clone(),
                            )?;
                        }
                        Op::StoreDeclaration(index) => {
                            if file_bindings::environment(program).is_some() {
                                file_bindings::set(
                                    program,
                                    ctx,
                                    &mut storage,
                                    file_bindings::declaration_name(program, index),
                                    stack.data.last().unwrap(),
                                )?;
                                continue;
                            }
                            let mut found = false;
                            for (key, value) in &mut storage.declarations.data {
                                ctx.charge(1)?;
                                if *key == (program.index, index) {
                                    *value = stack.data.last().unwrap().clone();
                                    found = true;
                                    break;
                                }
                            }
                            if !found {
                                storage.declarations.push(
                                    ctx,
                                    ((program.index, index), stack.data.last().unwrap().clone()),
                                )?;
                            }
                        }
                        Op::Integer(n, radix) => {
                            let text = program.constants[n].as_bytes().unwrap();
                            let value = crate::integer::parse(ctx, text, radix)?;
                            stack.push(ctx, value)?;
                        }
                        Op::Regex(n, flags) => {
                            let v = crate::regex::value::Regex::compile(
                                ctx,
                                program.constants[n].clone(),
                                flags,
                            )?;
                            stack.push(ctx, v)?;
                        }
                        Op::Constant(n) => {
                            let v = ctx.import(&program.constants[n])?;
                            stack.push(ctx, v)?;
                        }
                        Op::TypeShadowed(guard, next) => {
                            for name in &program.type_guards[guard] {
                                if runtime_bound(program, ctx, &frames, &storage, current, name)? {
                                    frames.data[current].ip = next;
                                    break;
                                }
                            }
                        }
                        Op::Nil => stack.push(ctx, Value::nil())?,
                        Op::TextStart => storage.texts.push(ctx, Buffer::empty())?,
                        Op::TextPart => {
                            let value = stack.data.pop().unwrap();
                            if let Some(call) = operators::string(ctx, &value)? {
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    call,
                                    Arguments::empty(),
                                    stack.data.len(),
                                )?;
                                frames.data.last_mut().unwrap().return_to = ReturnTo::Text(value);
                                continue;
                            }
                            crate::text::append(
                                ctx,
                                &value,
                                storage.texts.data.last_mut().unwrap(),
                            )?;
                        }
                        Op::TextEnd(symbol) => {
                            let text = storage.texts.data.pop().unwrap();
                            let mut value = Value::from_bytes(ctx, text)?;
                            if symbol {
                                let Kind::Bytes(bytes) = value.0 else {
                                    unreachable!()
                                };
                                value = Value(Kind::Symbol(bytes));
                            }
                            stack.push(ctx, value)?;
                        }
                        Op::Load(n) => {
                            let mut v = if let Some(name) = file_local {
                                file_bindings::get(program, ctx, name)?.unwrap_or_default()
                            } else {
                                storage.locals.data[n].clone().unwrap_or_default()
                            };
                            if let Kind::Offset(offset) = &v.0 {
                                return Err(offset.value_error());
                            }
                            if let Kind::Builtin(builtin) = v.0 {
                                v = builtin.read(ctx)?;
                            }
                            stack.push(ctx, v)?;
                        }
                        Op::LoadOptional(slot, name) => {
                            let scoped = if let Some(name) = file_local {
                                file_bindings::get(program, ctx, name)?
                            } else {
                                None
                            };
                            if let Some(value) =
                                scoped.as_ref().or(storage.locals.data[slot].as_ref())
                            {
                                if let Kind::Offset(offset) = &value.0 {
                                    return Err(offset.value_error());
                                }
                                let value = if let Kind::Builtin(builtin) = value.0 {
                                    builtin.read(ctx)?
                                } else {
                                    value.clone()
                                };
                                stack.push(ctx, value)?;
                            } else if let Some(value) = namespaces::constant(
                                program,
                                ctx,
                                &mut storage,
                                namespace,
                                &program.members[name],
                            )? {
                                stack.push(ctx, value)?;
                            } else if let Some(slot) = namespaces::ambient_slot(
                                ctx,
                                &frames,
                                &storage,
                                current,
                                &program.members[name],
                            )? {
                                stack.push(
                                    ctx,
                                    storage.locals.data[slot].as_ref().unwrap().clone(),
                                )?;
                            } else if let Some(&index) =
                                program.declaration_names.get(&program.members[name])
                            {
                                let value = declaration_value(program, ctx, &mut storage, index)?;
                                stack.push(ctx, value)?;
                            } else if let Some(&function) =
                                program.names.get(&program.members[name])
                            {
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
                            } else if let Some(global) =
                                global_index(program, &program.members[name])
                            {
                                let mut value = global_value(program, ctx, &mut storage, global)?;
                                if let Kind::Offset(offset) = &value.0 {
                                    return Err(offset.value_error());
                                }
                                if let Kind::Builtin(builtin) = value.0 {
                                    value = builtin.read(ctx)?;
                                }
                                stack.push(ctx, value)?;
                            } else {
                                return Err(Error::new(
                                    ErrorKind::Name,
                                    format!("undefined variable {}", program.members[name]),
                                ));
                            }
                        }
                        Op::ReceiverBound(slot, next) => {
                            let scoped = if let Some(name) = file_local {
                                file_bindings::get(program, ctx, name)?
                            } else {
                                None
                            };
                            if let Some(value) =
                                scoped.as_ref().or(storage.locals.data[slot].as_ref())
                            {
                                stack.push(ctx, value.clone())?;
                                frame.ip = next;
                            }
                        }
                        Op::Unbound(name) => {
                            if let Some(crate::arguments::Target::Function(owner, function)) =
                                file_bindings::root_target(
                                    program,
                                    &storage,
                                    &program.members[name],
                                )
                            {
                                enter_auto(
                                    &owner,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    function,
                                    stack.data.len(),
                                )?;
                                continue;
                            }
                            match namespaces::implicit(
                                program,
                                ctx,
                                &mut storage,
                                namespace,
                                self_value.as_ref(),
                                &program.members[name],
                            )? {
                                namespaces::Member::Function(function) => {
                                    enter_arguments(
                                        program,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        function,
                                        Arguments::empty(),
                                        stack.data.len(),
                                    )?;
                                }
                                namespaces::Member::Value(value) => stack.push(ctx, value)?,
                                namespaces::Member::Helper(module, helper) => {
                                    let value = dispatch::helper(
                                        program,
                                        ctx,
                                        &frames,
                                        &mut storage,
                                        (module, helper),
                                        &Arguments::empty(),
                                        true,
                                    )?;
                                    stack.push(ctx, value)?;
                                }
                                namespaces::Member::Missing => {
                                    namespaces::fallback(&program.members[name])?;
                                    let module = namespace.ok_or_else(|| {
                                        Error::new(ErrorKind::Name, "undefined variable")
                                    })?;
                                    let receiver = if let Some(value) = &self_value {
                                        value.clone()
                                    } else {
                                        namespaces::value(program, ctx, &mut storage, module)?
                                    };
                                    let site = crate::bytecode::CallSite {
                                        name,
                                        method: crate::bytecode::Method::parse(
                                            &program.members[name],
                                        ),
                                        auto: true,
                                        parenthesized: false,
                                        scope: false,
                                    };
                                    let (_, value) = members::call(
                                        ctx,
                                        site,
                                        &program.members[name],
                                        receiver,
                                        &[],
                                    )?;
                                    stack.push(ctx, value)?;
                                }
                            }
                        }
                        Op::Declaration(index) => {
                            let value = declaration_value(program, ctx, &mut storage, index)?;
                            stack.push(ctx, value)?;
                        }
                        Op::Global(index) => {
                            let mut value = global_value(program, ctx, &mut storage, index)?;
                            if let Kind::Offset(offset) = &value.0 {
                                return Err(offset.value_error());
                            }
                            if let Kind::Builtin(builtin) = value.0 {
                                value = builtin.read(ctx)?;
                            }
                            stack.push(ctx, value)?;
                        }
                        Op::GlobalReceiver(index, auto) => {
                            let mut value = global_value(program, ctx, &mut storage, index)?;
                            if let (Kind::Builtin(current), Kind::Builtin(original)) =
                                (&value.0, &program.globals[index].1.0)
                            {
                                if current == original && (auto || !current.auto()) {
                                    value = current.read(ctx)?;
                                }
                            }
                            stack.push(ctx, value)?;
                        }
                        Op::StoreGlobal(index) => {
                            if file_bindings::environment(program).is_some() {
                                file_bindings::set(
                                    program,
                                    ctx,
                                    &mut storage,
                                    program.globals[index].0.name(),
                                    stack.data.last().unwrap(),
                                )?;
                                continue;
                            }
                            let index = program.global_base + index;
                            let value = stack.data.last().unwrap();
                            address::refresh(
                                ctx,
                                address::Root::Global(index),
                                value,
                                &mut storage.addresses.data,
                                &[],
                            )?;
                            storage.globals.data[index] = Some(value.clone());
                        }
                        Op::ResolveGlobalCall(index) => {
                            let value = global_value(program, ctx, &mut storage, index)?;
                            let mut arguments = Arguments::empty();
                            arguments.target = Some(value_invocation(&value));
                            frame.arguments.push(ctx, arguments)?;
                        }
                        Op::AddressGlobal(index) => {
                            let address = file_bindings::global_address(
                                program,
                                ctx,
                                &mut storage,
                                index,
                                true,
                            )?;
                            storage.addresses.push(ctx, address)?;
                        }
                        Op::NonCallable => {
                            return Err(Error::new(
                                ErrorKind::Type,
                                "attempted to call non-callable value",
                            ));
                        }
                        Op::Bind(param, next) => {
                            if let Some(value) = frame.binding.data[0].value(ctx, param)? {
                                let param =
                                    &program.functions[frame.function.unwrap()].params[param];
                                let slot = frame.local_base + param.slot;
                                let ty = param.ty;
                                let value = if let Some(ty) = ty {
                                    normalize_type(
                                        program,
                                        ctx,
                                        &frames,
                                        &mut storage,
                                        current,
                                        (
                                            ty,
                                            crate::types::Context::Argument(param.name.as_bytes()),
                                        ),
                                        value,
                                    )?
                                } else {
                                    value
                                };
                                storage.locals.data[slot] = Some(value);
                                frames.data[current].ip = next;
                            }
                        }
                        Op::Normalize(ty, label) => {
                            let value = stack.data.pop().unwrap();
                            let value = normalize_type(
                                program,
                                ctx,
                                &frames,
                                &mut storage,
                                current,
                                (
                                    ty,
                                    crate::types::Context::Argument(
                                        program.constants[label].as_bytes().unwrap(),
                                    ),
                                ),
                                value,
                            )?;
                            stack.push(ctx, value)?;
                        }
                        Op::BindEnd => frame.binding = Buffer::empty(),
                        Op::Declare(slot) => {
                            if let Some(name) = file_local {
                                file_bindings::declare(program, ctx, &mut storage, name)?;
                            } else {
                                storage.locals.data[slot].get_or_insert_with(Value::nil);
                            }
                        }
                        Op::Bypass(slot) => storage.bypasses.push(ctx, slot)?,
                        Op::BypassEnd(n) => {
                            storage
                                .bypasses
                                .data
                                .truncate(storage.bypasses.data.len() - n);
                        }
                        Op::Shadow(slot) => {
                            storage.locals.data[frame.local_base + slot] = Some(Value::nil())
                        }
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
                                return Err(Error::local_jump("no block given"));
                            }
                        }
                        Op::Yield(n) => {
                            let block = frame.block.unwrap();
                            let base = stack.data.len() - n;
                            enter_block(
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
                            if let Some(name) = file_local {
                                file_bindings::set(program, ctx, &mut storage, name, value)?;
                            } else {
                                address::refresh(ctx, n, value, &mut storage.addresses.data, &[])?;
                                storage.locals.data[n] = Some(value.clone());
                            }
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
                            let result = ops::unary(ctx, op, value)?;
                            stack.push(ctx, result)?;
                        }
                        Op::Binary(op) => {
                            let b = stack.data.pop().unwrap();
                            let a = stack.data.pop().unwrap();
                            if let Some(resolved) = operators::resolve(
                                program,
                                ctx,
                                &a,
                                op,
                                (namespace, caller_instance),
                            )? {
                                let args = Arguments::from_values(ctx, &[b])?;
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    resolved.call,
                                    args,
                                    stack.data.len(),
                                )?;
                                if resolved.negate {
                                    frames.data.last_mut().unwrap().return_to = ReturnTo::Negate;
                                }
                                continue;
                            }
                            let value = ops::binary(ctx, op, a, b)?;
                            stack.push(ctx, value)?;
                        }
                        Op::AddStore(n) => {
                            let b = stack.data.pop().unwrap();
                            let a = stack.data.pop().unwrap();
                            if let Some(resolved) = operators::resolve(
                                program,
                                ctx,
                                &a,
                                "+",
                                (namespace, caller_instance),
                            )? {
                                let args = Arguments::from_values(ctx, &[b])?;
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    resolved.call,
                                    args,
                                    stack.data.len(),
                                )?;
                                frames.data.last_mut().unwrap().return_to = ReturnTo::Local(n);
                                continue;
                            }
                            storage.locals.data[n] = None;
                            let value = ops::binary(ctx, "+", a, b)?;
                            address::refresh(ctx, n, &value, &mut storage.addresses.data, &[])?;
                            storage.locals.data[n] = Some(value.clone());
                            stack.push(ctx, value)?;
                        }
                        Op::Shovel(site) => {
                            let value = stack.data.pop().unwrap();
                            let address = storage.addresses.data.pop().unwrap();
                            if matches!(address.value.0, Kind::Instance(_)) {
                                let resolved = operators::resolve(
                                    program,
                                    ctx,
                                    &address.value,
                                    "<<",
                                    (namespace, caller_instance),
                                )?
                                .ok_or_else(|| {
                                    Error::new(ErrorKind::Type, "unsupported append operands")
                                })?;
                                let args = Arguments::from_values(ctx, &[value])?;
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    resolved.call,
                                    args,
                                    stack.data.len(),
                                )?;
                                continue;
                            }
                            if !matches!(address.value.0, Kind::Array(_)) {
                                return Err(Error::new(
                                    ErrorKind::Type,
                                    "unsupported append operands",
                                ));
                            }
                            let guard_program = programs::address(ctx, &mut storage, &address)?;
                            let guard = address_guard(
                                guard_program.as_deref(),
                                ctx,
                                &frames,
                                &mut storage,
                                &address,
                            )?;
                            let result = address.apply(
                                ctx,
                                address::Bindings {
                                    recover: !storage.handlers.data.is_empty(),
                                    guard,
                                    locals: &mut storage.locals.data,
                                    globals: &mut storage.globals.data,
                                    namespaces: &mut storage.namespaces.data,
                                },
                                &mut storage.addresses.data,
                                |ctx, receiver| {
                                    members::call(ctx, site, "push", receiver, &[value])
                                },
                            )?;
                            stack.push(ctx, result)?;
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
                            if matches!(root.0, Kind::Instance(_)) {
                                let call = operators::index(
                                    program,
                                    ctx,
                                    root,
                                    "[]",
                                    (namespace, caller_instance),
                                )?;
                                let args = Arguments::from_values(ctx, args)?;
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    call,
                                    args,
                                    base,
                                )?;
                                stack.data.truncate(base);
                                continue;
                            }
                            let value = if n == 1 {
                                ops::index(ctx, root, &args[0])?
                            } else {
                                crate::sequence::slice(ctx, root, args, false)?
                            };
                            stack.data.truncate(base);
                            stack.push(ctx, value)?;
                        }
                        Op::AddressLocal(n) => {
                            let address = if let Some(name) = file_local {
                                file_bindings::address(program, ctx, name)?
                            } else {
                                Address::new(
                                    Some(n),
                                    storage.locals.data[n].clone().unwrap_or_default(),
                                )
                            };
                            storage.addresses.push(ctx, address)?;
                        }
                        Op::AddressBound(slot, next) => {
                            if let Some(name) =
                                file_local.filter(|_| file_bindings::environment(program).is_some())
                            {
                                if file_bindings::get(program, ctx, name)?.is_some() {
                                    let address = file_bindings::address(program, ctx, name)?;
                                    storage.addresses.push(ctx, address)?;
                                    frame.ip = next;
                                }
                            } else if let Some(value) = &storage.locals.data[slot] {
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
                            let root = &storage.addresses.data.last().unwrap().value;
                            if matches!(root.0, Kind::Instance(_)) {
                                let call = operators::index(
                                    program,
                                    ctx,
                                    root,
                                    "[]",
                                    (namespace, caller_instance),
                                )?;
                                let args = Arguments::from_values(ctx, &stack.data[base..])?;
                                storage.addresses.data.pop();
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    call,
                                    args,
                                    base,
                                )?;
                                frames.data.last_mut().unwrap().return_to = ReturnTo::Address;
                                stack.data.truncate(base);
                                continue;
                            }
                            storage
                                .addresses
                                .data
                                .last_mut()
                                .unwrap()
                                .index(ctx, &stack.data[base..])?;
                            stack.data.truncate(base);
                        }
                        Op::AddressNamespaceField(site) => {
                            let name = &program.members[site.name];
                            let address = storage.addresses.data.pop().unwrap();
                            if let Kind::Namespace(receiver) = &address.value.0 {
                                namespaces::member(
                                    ctx,
                                    &mut storage,
                                    &address.value,
                                    site,
                                    name,
                                    namespaces::Access {
                                        program: program.index,
                                        caller: namespace,
                                        implicit: false,
                                        instance: caller_instance,
                                    },
                                )?;
                                let owner = programs::namespace(ctx, &mut storage, receiver)?;
                                let address = namespaces::address(
                                    &owner,
                                    ctx,
                                    &mut storage,
                                    receiver.definition.index,
                                    name,
                                    false,
                                )?;
                                storage.addresses.push(ctx, address)?;
                            } else {
                                let (_, value) =
                                    members::call(ctx, site, name, address.value, &[])?;
                                storage.addresses.push(ctx, Address::new(None, value))?;
                            }
                        }
                        Op::AddressMember(site) => {
                            let name = &program.members[site.name];
                            if matches!(
                                storage.addresses.data.last().unwrap().value.0,
                                Kind::Namespace(_) | Kind::Instance(_)
                            ) {
                                let receiver = storage.addresses.data.last().unwrap().value.clone();
                                match namespaces::member(
                                    ctx,
                                    &mut storage,
                                    &receiver,
                                    site,
                                    name,
                                    namespaces::Access {
                                        program: program.index,
                                        caller: namespace,
                                        implicit: false,
                                        instance: caller_instance,
                                    },
                                )? {
                                    namespaces::Member::Function(function) => {
                                        storage.addresses.data.pop();
                                        enter_arguments(
                                            program,
                                            ctx,
                                            &mut frames,
                                            &mut storage,
                                            function,
                                            Arguments::empty(),
                                            stack.data.len(),
                                        )?;
                                        frames.data.last_mut().unwrap().return_to =
                                            ReturnTo::Address;
                                        continue;
                                    }
                                    namespaces::Member::Value(value) => {
                                        let value =
                                            members::field_call(ctx, site, value, &[], &[], false)?;
                                        storage.addresses.data.pop();
                                        storage.addresses.push(ctx, Address::new(None, value))?;
                                        continue;
                                    }
                                    namespaces::Member::Helper(module, helper) => {
                                        let value = dispatch::helper(
                                            program,
                                            ctx,
                                            &frames,
                                            &mut storage,
                                            (module, helper),
                                            &Arguments::empty(),
                                            true,
                                        )?;
                                        stack.push(ctx, value)?;
                                        continue;
                                    }
                                    namespaces::Member::Missing => namespaces::fallback(name)?,
                                }
                            }
                            let address = storage.addresses.data.last_mut().unwrap();
                            let key = ctx.bytes(name.as_bytes())?;
                            let data = if let Kind::Hash(hash) = &address.value.0 {
                                hash.find(ctx, name.as_bytes())?
                            } else {
                                None
                            };
                            if let Some(index) = data {
                                if !address.has_binding() {
                                    let Kind::Hash(hash) = &address.value.0 else {
                                        unreachable!()
                                    };
                                    let value = &hash.buffer.data[index].1;
                                    if members::introspection::callable(value) {
                                        let value = members::field_call(
                                            ctx,
                                            site,
                                            value.clone(),
                                            &[],
                                            &[],
                                            false,
                                        )?;
                                        storage.addresses.data.pop();
                                        storage.addresses.push(ctx, Address::new(None, value))?;
                                        continue;
                                    }
                                }
                                address.index(ctx, &[key])?;
                            } else {
                                let address = storage.addresses.data.pop().unwrap();
                                if !crate::bytecode::mutating_member(name) {
                                    let (_, value) =
                                        members::call(ctx, site, name, address.value, &[])?;
                                    storage.addresses.push(ctx, Address::new(None, value))?;
                                    continue;
                                }
                                let guard_program = programs::address(ctx, &mut storage, &address)?;
                                let guard = address_guard(
                                    guard_program.as_deref(),
                                    ctx,
                                    &frames,
                                    &mut storage,
                                    &address,
                                )?;
                                let value = address.apply(
                                    ctx,
                                    address::Bindings {
                                        recover: !storage.handlers.data.is_empty(),
                                        guard,
                                        locals: &mut storage.locals.data,
                                        globals: &mut storage.globals.data,
                                        namespaces: &mut storage.namespaces.data,
                                    },
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
                            address.member_target = true;
                            address.selectors.push(ctx, key)?;
                            if read {
                                if matches!(address.value.0, Kind::Namespace(_) | Kind::Instance(_))
                                {
                                    let receiver = address.value.clone();
                                    match namespaces::member(
                                        ctx,
                                        &mut storage,
                                        &receiver,
                                        site,
                                        name,
                                        namespaces::Access {
                                            program: program.index,
                                            caller: namespace,
                                            implicit: false,
                                            instance: caller_instance,
                                        },
                                    )? {
                                        namespaces::Member::Function(function) => {
                                            enter_arguments(
                                                program,
                                                ctx,
                                                &mut frames,
                                                &mut storage,
                                                function,
                                                Arguments::empty(),
                                                stack.data.len(),
                                            )?;
                                            continue;
                                        }
                                        namespaces::Member::Value(value) => {
                                            stack.push(ctx, value)?;
                                            continue;
                                        }
                                        namespaces::Member::Helper(module, helper) => {
                                            let value = dispatch::helper(
                                                program,
                                                ctx,
                                                &frames,
                                                &mut storage,
                                                (module, helper),
                                                &Arguments::empty(),
                                                true,
                                            )?;
                                            stack.push(ctx, value)?;
                                            continue;
                                        }
                                        namespaces::Member::Missing => namespaces::fallback(name)?,
                                    }
                                }
                                let address = storage.addresses.data.last().unwrap();
                                let (_, value) =
                                    members::call(ctx, site, name, address.value.clone(), &[])?;
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
                                if matches!(address.value.0, Kind::Instance(_)) {
                                    let call = operators::index(
                                        program,
                                        ctx,
                                        &address.value,
                                        "[]",
                                        (namespace, caller_instance),
                                    )?;
                                    let args =
                                        Arguments::from_values(ctx, &address.selectors.data)?;
                                    enter_arguments(
                                        program,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        call,
                                        args,
                                        stack.data.len(),
                                    )?;
                                    continue;
                                }
                                let value = address.read_target(ctx)?;
                                stack.push(ctx, value)?;
                            }
                        }
                        Op::AddressStore => {
                            let address = storage.addresses.data.pop().unwrap();
                            let value = stack.data.pop().unwrap();
                            if address.member_target
                                && matches!(address.value.0, Kind::Namespace(_) | Kind::Instance(_))
                            {
                                let receiver = &address.value;
                                let key = address.selectors.data[0].require_bytes()?;
                                let name = std::str::from_utf8(key).unwrap();
                                if let Some(function) = namespaces::setter(
                                    program,
                                    ctx,
                                    &mut storage,
                                    receiver,
                                    name,
                                    namespace,
                                    caller_instance,
                                )? {
                                    let args =
                                        Arguments::from_values(ctx, std::slice::from_ref(&value))?;
                                    enter_arguments(
                                        program,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        function,
                                        args,
                                        stack.data.len(),
                                    )?;
                                    frames.data.last_mut().unwrap().return_to =
                                        ReturnTo::Assigned(value);
                                    continue;
                                }
                                match &receiver.0 {
                                    Kind::Instance(instance) => {
                                        set_ivar(ctx, &mut storage, instance, name, &value)?
                                    }
                                    Kind::Namespace(namespace) => {
                                        let owner =
                                            programs::namespace(ctx, &mut storage, namespace)?;
                                        namespaces::set(
                                            &owner,
                                            ctx,
                                            &mut storage,
                                            namespace.definition.index,
                                            name,
                                            value.clone(),
                                        )?;
                                    }
                                    _ => unreachable!(),
                                }
                                stack.push(ctx, value)?;
                                continue;
                            }
                            if matches!(address.value.0, Kind::Instance(_)) {
                                let call = operators::index(
                                    program,
                                    ctx,
                                    &address.value,
                                    "[]=",
                                    (namespace, caller_instance),
                                )?;
                                let mut args = Arguments::empty();
                                args.positional =
                                    Buffer::with_capacity(ctx, address.selectors.data.len() + 1)?;
                                args.positional.extend(ctx, &address.selectors.data)?;
                                args.positional.push(ctx, value.clone())?;
                                enter_arguments(
                                    program,
                                    ctx,
                                    &mut frames,
                                    &mut storage,
                                    call,
                                    args,
                                    stack.data.len(),
                                )?;
                                frames.data.last_mut().unwrap().return_to =
                                    ReturnTo::Assigned(value);
                                continue;
                            }
                            let guard_program = programs::address(ctx, &mut storage, &address)?;
                            let guard = address_guard(
                                guard_program.as_deref(),
                                ctx,
                                &frames,
                                &mut storage,
                                &address,
                            )?;
                            let value = address.assign(
                                ctx,
                                address::Bindings {
                                    recover: !storage.handlers.data.is_empty(),
                                    guard,
                                    locals: &mut storage.locals.data,
                                    globals: &mut storage.globals.data,
                                    namespaces: &mut storage.namespaces.data,
                                },
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
                            if matches!(address.value.0, Kind::Namespace(_) | Kind::Instance(_)) {
                                let receiver = &address.value;
                                let name = &program.members[site.name];
                                match namespaces::member(
                                    ctx,
                                    &mut storage,
                                    receiver,
                                    site,
                                    name,
                                    namespaces::Access {
                                        program: program.index,
                                        caller: namespace,
                                        implicit: false,
                                        instance: caller_instance,
                                    },
                                )? {
                                    namespaces::Member::Function(function) => {
                                        let args =
                                            Arguments::from_values(ctx, &stack.data[base..])?;
                                        enter_arguments(
                                            program,
                                            ctx,
                                            &mut frames,
                                            &mut storage,
                                            function,
                                            args,
                                            base,
                                        )?;
                                        stack.data.truncate(base);
                                        continue;
                                    }
                                    namespaces::Member::Value(value) => {
                                        let value = members::field_call(
                                            ctx,
                                            site,
                                            value,
                                            &stack.data[base..],
                                            &[],
                                            false,
                                        )?;
                                        stack.data.truncate(base);
                                        stack.push(ctx, value)?;
                                        continue;
                                    }
                                    namespaces::Member::Helper(module, helper) => {
                                        let args =
                                            Arguments::from_values(ctx, &stack.data[base..])?;
                                        let value = dispatch::helper(
                                            program,
                                            ctx,
                                            &frames,
                                            &mut storage,
                                            (module, helper),
                                            &args,
                                            site.auto,
                                        )?;
                                        stack.data.truncate(base);
                                        stack.push(ctx, value)?;
                                        continue;
                                    }
                                    namespaces::Member::Missing => namespaces::fallback(name)?,
                                }
                            }
                            let guard_program = programs::address(ctx, &mut storage, &address)?;
                            let guard = address_guard(
                                guard_program.as_deref(),
                                ctx,
                                &frames,
                                &mut storage,
                                &address,
                            )?;
                            let value = address.apply(
                                ctx,
                                address::Bindings {
                                    recover: !storage.handlers.data.is_empty(),
                                    guard,
                                    locals: &mut storage.locals.data,
                                    globals: &mut storage.globals.data,
                                    namespaces: &mut storage.namespaces.data,
                                },
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
                            let matched =
                                ops::case_matches(ctx, target.as_ref(), &candidate, splat)?;
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
                                    _ => {
                                        return Err(Error::new(
                                            ErrorKind::Type,
                                            "cannot iterate this value",
                                        ));
                                    }
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
                                    text_base: storage.texts.data.len(),
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
                            storage.texts.data.truncate(state.text_base);
                            frame.arguments.data.truncate(state.argument_base);
                            frame.ip = state.next;
                        }
                        Op::LoopEnd => {
                            let state = frame.loops.data.pop().unwrap();
                            stack.data.truncate(state.base);
                            storage.addresses.data.truncate(state.address_base);
                            storage.bypasses.data.truncate(state.bypass_base);
                            storage.texts.data.truncate(state.text_base);
                            frame.arguments.data.truncate(state.argument_base);
                            stack.push(ctx, state.result())?;
                        }
                        Op::LoopGuard(breaking) => handlers::guard_loop(ctx, &frames, breaking)?,
                        Op::Break(has_value) | Op::Next(has_value) => {
                            let value = has_value.then(|| stack.data.pop().unwrap());
                            let control =
                                handlers::loop_control(&frames, matches!(op, Op::Break(_)), value)?;
                            return Ok(Event::Control(control));
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
                        Op::ForwardArguments => {
                            // Forwarded reads need the evaluated value; mutators keep the live address.
                            let mut args = Arguments::empty();
                            args.target = Some(crate::arguments::Target::Receiver(
                                storage.addresses.data.last().unwrap().value.clone(),
                            ));
                            frame.arguments.push(ctx, args)?;
                        }
                        Op::CallName(slot, name) => {
                            let target = call_targets::identifier(
                                program,
                                ctx,
                                &frames,
                                &mut storage,
                                current,
                                slot,
                                name,
                            )?;
                            frames.data[current]
                                .arguments
                                .data
                                .last_mut()
                                .unwrap()
                                .resolve(target, true);
                        }
                        Op::CallValue => {
                            let value = stack.data.pop().unwrap();
                            frame
                                .arguments
                                .data
                                .last_mut()
                                .unwrap()
                                .resolve(value_invocation(&value), true);
                        }
                        Op::CallMember(site) => {
                            let receiver = stack.data.pop().unwrap();
                            let target = call_targets::member(
                                program,
                                ctx,
                                &mut storage,
                                receiver,
                                site,
                                namespace,
                                self_value.is_some(),
                            )?;
                            frame
                                .arguments
                                .data
                                .last_mut()
                                .unwrap()
                                .resolve(target, site.parenthesized);
                        }
                        Op::ResolveCall(slot, name, parenthesized) => {
                            let name_index = name;
                            let name = &program.members[name];
                            let target = if let Some(Some(value)) = storage.locals.data.get(slot) {
                                value_invocation(value)
                            } else if let Some(value) = file_bindings::get(program, ctx, name)? {
                                value_invocation(&value)
                            } else if program.declaration_names.contains_key(name) {
                                crate::arguments::Target::Plain(Invocation::NonCallable)
                            } else if let Some(&function) = program.names.get(name) {
                                crate::arguments::Target::Plain(Invocation::Function(function))
                            } else if let Some(host) = program.hosts.iter().position(|h| h == name)
                            {
                                crate::arguments::Target::Plain(Invocation::Host(host))
                            } else if let Some(global) = global_index(program, name) {
                                value_invocation(&global_value(program, ctx, &mut storage, global)?)
                            } else if let Some(target) =
                                file_bindings::root_target(program, &storage, name)
                            {
                                target
                            } else {
                                match namespaces::implicit(
                                    program,
                                    ctx,
                                    &mut storage,
                                    namespace,
                                    self_value.as_ref(),
                                    name,
                                )? {
                                    namespaces::Member::Function(function) => {
                                        crate::arguments::Target::Method(function)
                                    }
                                    namespaces::Member::Value(value) => value_invocation(&value),
                                    namespaces::Member::Helper(receiver, helper) => {
                                        crate::arguments::Target::Helper(receiver, helper)
                                    }
                                    namespaces::Member::Missing => {
                                        namespaces::fallback(name)?;
                                        let module = namespace.ok_or_else(|| {
                                            Error::new(ErrorKind::Name, "undefined variable")
                                        })?;
                                        crate::arguments::Target::Plain(Invocation::ImplicitMember(
                                            module, name_index,
                                        ))
                                    }
                                }
                            };
                            let mut arguments = Arguments::empty();
                            arguments.resolve(target, parenthesized);
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
                            let mut args = frame.arguments.data.pop().unwrap();
                            let target = if matches!(target, Invocation::Resolved) {
                                args.target.take().unwrap()
                            } else {
                                crate::arguments::Target::Plain(target)
                            };
                            let target = match target {
                                crate::arguments::Target::Raise(..)
                                | crate::arguments::Target::Output(..)
                                | crate::arguments::Target::Format(..)
                                | crate::arguments::Target::Receiver(..) => unreachable!(),
                                crate::arguments::Target::Unbound(kind, name) => {
                                    let required = if kind == "hash" {
                                        "hash or object"
                                    } else {
                                        kind
                                    };
                                    return Err(Error::new(
                                        ErrorKind::Runtime,
                                        format!(
                                            "{kind}.{} requires a {required} receiver, got nil",
                                            program.members[name]
                                        ),
                                    ));
                                }
                                crate::arguments::Target::Member(receiver, name) => {
                                    stack.push(ctx, receiver)?;
                                    Invocation::Member(
                                        crate::bytecode::CallSite {
                                            name,
                                            method: crate::bytecode::Method::parse(
                                                &program.members[name],
                                            ),
                                            auto: false,
                                            parenthesized: false,
                                            scope: false,
                                        },
                                        false,
                                    )
                                }
                                crate::arguments::Target::Plain(target) => target,
                                crate::arguments::Target::Function(owner, function) => {
                                    enter_arguments(
                                        &owner,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        function,
                                        args,
                                        stack.data.len(),
                                    )?;
                                    continue;
                                }
                                crate::arguments::Target::Method(call) => {
                                    enter_arguments(
                                        program,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        call,
                                        args,
                                        stack.data.len(),
                                    )?;
                                    continue;
                                }
                                crate::arguments::Target::Helper(receiver, helper) => {
                                    let value = dispatch::helper(
                                        program,
                                        ctx,
                                        &frames,
                                        &mut storage,
                                        (receiver, helper),
                                        &args,
                                        false,
                                    )?;
                                    stack.push(ctx, value)?;
                                    continue;
                                }
                                crate::arguments::Target::Offset(offset) => {
                                    let value = offset.call(
                                        ctx,
                                        &args.positional.data,
                                        &args.keywords.buffer.data,
                                        args.block.is_some(),
                                    )?;
                                    stack.push(ctx, value)?;
                                    continue;
                                }
                            };
                            let target = if let Invocation::ImplicitMember(module, name) = target {
                                let receiver = if let Some(value) = &self_value {
                                    value.clone()
                                } else {
                                    namespaces::value(program, ctx, &mut storage, module)?
                                };
                                stack.push(ctx, receiver)?;
                                Invocation::Member(
                                    crate::bytecode::CallSite {
                                        name,
                                        method: crate::bytecode::Method::parse(
                                            &program.members[name],
                                        ),
                                        auto: false,
                                        parenthesized: false,
                                        scope: false,
                                    },
                                    false,
                                )
                            } else {
                                target
                            };
                            match target {
                                Invocation::Builtin(builtin) => {
                                    if let crate::builtin::Builtin::Output(kind) = builtin {
                                        output::start(
                                            program,
                                            ctx,
                                            &mut frames,
                                            &mut storage,
                                            &mut stack,
                                            kind,
                                            args,
                                        )?;
                                        continue;
                                    }
                                    if let crate::builtin::Builtin::Format(function) = builtin {
                                        format::start(
                                            program,
                                            ctx,
                                            &mut frames,
                                            &mut storage,
                                            &mut stack,
                                            function,
                                            args,
                                        )?;
                                        continue;
                                    }
                                    if builtin == crate::builtin::Builtin::Loop {
                                        let iteration = iteration::forever(
                                            ctx,
                                            &args.positional.data,
                                            &args.keywords.buffer.data,
                                            args.block.is_some(),
                                        )?;
                                        enter_iteration(
                                            program,
                                            ctx,
                                            &mut frames,
                                            &mut storage,
                                            stack.data.len(),
                                            args,
                                            iteration,
                                        )?;
                                        continue;
                                    }
                                    let value = builtin.call(
                                        ctx,
                                        &args.positional.data,
                                        &args.keywords.buffer.data,
                                        args.block.is_some(),
                                    )?;
                                    stack.push(ctx, value)?;
                                }
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
                                    let result = hosts[host](
                                        ctx,
                                        &args.positional.data,
                                        &args.keywords.buffer.data,
                                    );
                                    ctx.checkpoint()?;
                                    let value = ctx.import(&result?)?;
                                    programs::imported(ctx, &mut storage, &value)?;
                                    stack.push(ctx, value)?;
                                }
                                Invocation::Member(site, mutating) => {
                                    dispatch::member(
                                        program,
                                        ctx,
                                        &mut frames,
                                        &mut storage,
                                        &mut stack,
                                        dispatch::Call {
                                            site,
                                            name: &program.members[site.name],
                                            mutating,
                                            args,
                                            access: namespaces::Access {
                                                program: program.index,
                                                caller: namespace,
                                                implicit: false,
                                                instance: caller_instance,
                                            },
                                        },
                                    )?;
                                }
                                Invocation::NonCallable => {
                                    return Err(Error::new(
                                        ErrorKind::Type,
                                        "attempted to call non-callable value",
                                    ));
                                }
                                Invocation::Resolved | Invocation::ImplicitMember(..) => {
                                    unreachable!()
                                }
                            }
                        }
                        Op::Host(host, n) => {
                            let base = stack.data.len() - n;
                            ctx.checkpoint()?;
                            let result = hosts[host](ctx, &stack.data[base..], &[]);
                            ctx.checkpoint()?;
                            let result = result?;
                            let value = ctx.import(&result)?;
                            programs::imported(ctx, &mut storage, &value)?;
                            stack.data.truncate(base);
                            stack.push(ctx, value)?;
                        }
                        Op::Method(site, n) => {
                            let base = stack.data.len() - n - 1;
                            let root = std::mem::take(&mut stack.data[base]);
                            if matches!(root.0, Kind::Namespace(_) | Kind::Instance(_)) {
                                let receiver = &root;
                                let name = &program.members[site.name];
                                match namespaces::member(
                                    ctx,
                                    &mut storage,
                                    receiver,
                                    site,
                                    name,
                                    namespaces::Access {
                                        program: program.index,
                                        caller: namespace,
                                        implicit: false,
                                        instance: caller_instance,
                                    },
                                )? {
                                    namespaces::Member::Function(function) => {
                                        let args =
                                            Arguments::from_values(ctx, &stack.data[base + 1..])?;
                                        enter_arguments(
                                            program,
                                            ctx,
                                            &mut frames,
                                            &mut storage,
                                            function,
                                            args,
                                            base,
                                        )?;
                                        stack.data.truncate(base);
                                        continue;
                                    }
                                    namespaces::Member::Value(value) => {
                                        let value = members::field_call(
                                            ctx,
                                            site,
                                            value,
                                            &stack.data[base + 1..],
                                            &[],
                                            false,
                                        )?;
                                        stack.data.truncate(base);
                                        stack.push(ctx, value)?;
                                        continue;
                                    }
                                    namespaces::Member::Helper(module, helper) => {
                                        let args =
                                            Arguments::from_values(ctx, &stack.data[base + 1..])?;
                                        let value = dispatch::helper(
                                            program,
                                            ctx,
                                            &frames,
                                            &mut storage,
                                            (module, helper),
                                            &args,
                                            site.auto,
                                        )?;
                                        stack.data.truncate(base);
                                        stack.push(ctx, value)?;
                                        continue;
                                    }
                                    namespaces::Member::Missing => namespaces::fallback(name)?,
                                }
                            }
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
                        Op::JumpNil(target) => {
                            if matches!(stack.data.last().unwrap().0, Kind::Nil) {
                                frame.ip = target;
                            }
                        }
                        Op::AddressJumpNil(target, value_result) => {
                            if matches!(storage.addresses.data.last().unwrap().value.0, Kind::Nil) {
                                if value_result {
                                    storage.addresses.data.pop();
                                    stack.push(ctx, Value::nil())?;
                                } else {
                                    *storage.addresses.data.last_mut().unwrap() =
                                        Address::new(None, Value::nil());
                                }
                                frame.ip = target;
                            }
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
                            let target = if matches!(op, Op::Return)
                                && frame.parent.is_some()
                                && !program.functions[frame.function.unwrap()].initializer
                            {
                                let Some(home) = frame.home else {
                                    return Ok(Event::Control(Control::Invalid {
                                        frame: current,
                                        jump: handlers::Jump::Return,
                                        _value: Some(value),
                                    }));
                                };
                                home
                            } else {
                                current
                            };
                            return Ok(Event::Control(Control::Return {
                                target,
                                value,
                                normalize: true,
                            }));
                        }
                    }
                }
            })();
            let outcome = (|| -> Result<Option<Value>> {
                match event? {
                    Event::Error(error) => {
                        handlers::error(ctx, &mut frames, &mut storage, &mut stack, error)?;
                        Ok(None)
                    }
                    Event::Control(control) => {
                        if let Some(control) = handlers::intercept(
                            ctx,
                            &mut frames,
                            &mut storage,
                            &mut stack,
                            control,
                        )? {
                            handlers::apply_control(
                                ctx,
                                &mut frames,
                                &mut storage,
                                &mut stack,
                                pending_entry.is_some(),
                                control,
                            )
                        } else {
                            Ok(None)
                        }
                    }
                }
            })();
            match outcome {
                Ok(Some(value)) => return Ok(value),
                Ok(None) => (),
                Err(error) => {
                    if ctx.exhausted() || storage.handlers.data.is_empty() {
                        return Err(error);
                    }
                    ctx.checkpoint()?;
                    let error =
                        handlers::SavedError::new(program, ctx, &frames.data, function, error)?;
                    handlers::error(ctx, &mut frames, &mut storage, &mut stack, error)?;
                }
            }
        }
    })();
    ctx.enum_rebind = crate::enums::Rebind::default();
    result.map_err(|error| diagnose(&code.program, &frames.data, function, error))
}

fn frame_offset(frame: &Frame) -> Option<(&crate::bytecode::Program, u32)> {
    let program = &frame.program.code.program;
    let function = &program.functions[frame.function?];
    Some((
        program,
        function
            .locations
            .get(frame.ip.saturating_sub(1))
            .copied()
            .unwrap_or(function.offset),
    ))
}

fn diagnostic_site<'a>(
    mut program: &'a crate::bytecode::Program,
    mut frames: &'a [Frame],
    mut entry: usize,
) -> (&'a crate::bytecode::Program, &'a [Frame], u32) {
    while let Some((index, frame)) = frames
        .iter()
        .enumerate()
        .rev()
        .find(|(_, frame)| frame.function.is_some())
    {
        let function = frame.function.unwrap();
        let owner = &frame.program.code.program;
        let op = owner.functions[function]
            .code
            .get(frame.ip.saturating_sub(1));
        if frame.binding.data.is_empty()
            || !matches!(
                op,
                Some(Op::Bind(..) | Op::Normalize(..) | Op::BindIvar(..))
            )
        {
            break;
        }
        frames = &frames[..index];
        program = owner;
        entry = function;
    }
    let (program, offset) = frames
        .iter()
        .rev()
        .find_map(frame_offset)
        .unwrap_or((program, program.functions[entry].offset));
    (program, frames, offset)
}

fn trace_entries<'a>(
    program: &'a crate::bytecode::Program,
    frames: &'a [Frame],
    offset: u32,
) -> impl Iterator<Item = (Option<&'a Arc<str>>, &'a crate::source::Source, u32)> + 'a {
    let name = frames
        .iter()
        .rev()
        .filter(|frame| frame.binding.data.is_empty())
        .filter_map(|frame| frame.function.map(|index| &frame.program.functions[index]))
        .find(|function| function.name != "<block>" && !function.initializer)
        .filter(|function| function.name != "__main__")
        .map(|function| &function.trace_name);
    std::iter::once((name, &program.source, offset)).chain(
        frames
            .iter()
            .enumerate()
            .rev()
            .filter_map(move |(index, frame)| {
                let owner = &frame.program.code.program;
                let function = &owner.functions[frame.function?];
                if function.name == "<block>"
                    || function.name == "__main__"
                    || function.initializer
                    || !frame.binding.data.is_empty()
                {
                    return None;
                }
                let (caller, at) = frames[..index]
                    .iter()
                    .rev()
                    .find_map(frame_offset)
                    .unwrap_or((owner, function.offset));
                Some((Some(&function.trace_name), &caller.source, at))
            }),
    )
}

fn diagnose(
    program: &crate::bytecode::Program,
    frames: &[Frame],
    entry: usize,
    mut error: Error,
) -> Error {
    if error.diagnostic.is_some() {
        return error;
    }
    let (program, frames, offset) = diagnostic_site(program, frames, entry);
    let trace = trace_entries(program, frames, offset)
        .map(|(name, source, at)| crate::StackFrame {
            function: name.cloned().unwrap_or_else(|| "<script>".into()),
            position: source.position(at),
        })
        .collect();
    error.offset = Some(offset as usize);
    error.diagnostic = Some(std::sync::Arc::new(crate::Diagnostic {
        position: program.source.position(offset),
        code_frame: program.source.frame(offset),
        frames: trace,
    }));
    error
}

fn normalize_return(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    frame: usize,
    value: Value,
) -> Result<Value> {
    let program = &frames.data[frame].program;
    if frames.data[frame].constructor {
        return Ok(frames.data[frame].receiver.as_ref().unwrap().clone());
    }
    let Some(function) = frames.data[frame].function else {
        return Ok(value);
    };
    let Some(ty) = program.functions[function].return_type else {
        return Ok(value);
    };
    normalize_type(
        program,
        ctx,
        frames,
        storage,
        frame,
        (
            ty,
            crate::types::Context::Return(&program.functions[function].trace_name),
        ),
        value,
    )
    .map_err(|error| diagnose(program, &frames.data[..frame], function, error))
}

fn runtime_bound(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    current: usize,
    name: &str,
) -> Result<bool> {
    ctx.work_bytes(name.len())?;
    let mut scope = Some(current);
    while let Some(index) = scope {
        ctx.charge(1)?;
        let frame = &frames.data[index];
        if let Some(function) = frame.function {
            for (slot, candidate) in frame.program.functions[function]
                .local_names
                .iter()
                .enumerate()
            {
                ctx.charge(1)?;
                if storage.locals.data[frame.local_base + slot].is_some()
                    && crate::enums::compare_names(ctx, candidate.as_bytes(), name.as_bytes())?
                        == std::cmp::Ordering::Equal
                {
                    return Ok(true);
                }
            }
        }
        scope = frame.parent;
    }
    if crate::builtin::Global::parse(name).is_some()
        || program.names.contains_key(name)
        || program.declaration_names.contains_key(name)
    {
        return Ok(true);
    }
    if file_bindings::get(program, ctx, name)?.is_some() {
        return Ok(true);
    }
    for host in &program.hosts {
        ctx.charge(1)?;
        if crate::enums::compare_names(ctx, host.as_bytes(), name.as_bytes())?
            == std::cmp::Ordering::Equal
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn set_ivar(
    ctx: &mut CallContext,
    storage: &mut Storage,
    instance: &std::sync::Arc<crate::objects::Instance>,
    name: &str,
    value: &Value,
) -> Result<()> {
    if let Some(field) = crate::objects::field_slot(ctx, instance, name)? {
        address::refresh(
            ctx,
            address::Root::Object(instance.clone(), field),
            value,
            &mut storage.addresses.data,
            &[],
        )?;
    }
    crate::objects::set(ctx, instance, name, value)
}

fn property_type(
    program: &Program,
    ctx: &mut CallContext,
    instance: &crate::objects::Instance,
    name: &str,
) -> Result<Option<usize>> {
    let methods = &instance.class().definition.instance_methods;
    let mut getter = None;
    let mut setter = None;
    for method in methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name.strip_suffix('=') == Some(name) {
            setter = Some(method.function);
        }
        if method.name == name {
            getter = Some(method.function);
        }
    }
    let ty = if let Some(setter) = setter {
        let function = &program.functions[setter];
        if function
            .accessor
            .as_ref()
            .is_some_and(|(field, setter)| field == name && *setter)
        {
            function.params.first().and_then(|param| param.ty)
        } else {
            None
        }
    } else {
        getter.and_then(|getter| {
            let function = &program.functions[getter];
            function
                .accessor
                .as_ref()
                .filter(|(field, setter)| field == name && !setter)
                .and(function.return_type)
        })
    };
    Ok(ty)
}

fn normalize_ivar(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    instance: &crate::objects::Instance,
    name: &str,
    value: Value,
) -> Result<Value> {
    let owner = programs::namespace(ctx, storage, instance.class())?;
    let program = &*owner;
    let Some(ty) = property_type(program, ctx, instance, name)? else {
        return Ok(value);
    };
    crate::types::prepare(ctx, &program.types[ty], |ctx, name| {
        resolve_type(program, ctx, frames, storage, None, name, false)
    })?
    .normalize_with(ctx, value, crate::types::Context::Ivar(name.as_bytes()))
}

fn address_guard<'a>(
    program: Option<&'a Program>,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    address: &Address,
) -> Result<Option<crate::types::Prepared<'a>>> {
    let Some((instance, field)) = address.object_binding() else {
        return Ok(None);
    };
    let program = program.expect("object address has an owning program");
    let name = crate::objects::field_name(instance, field)?;
    let name = std::str::from_utf8(name.as_bytes().unwrap()).unwrap();
    let Some(ty) = property_type(program, ctx, instance, name)? else {
        return Ok(None);
    };
    crate::types::prepare(ctx, &program.types[ty], |ctx, name| {
        resolve_type(program, ctx, frames, storage, None, name, false)
    })
    .map(Some)
}

fn normalize_type(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    frame: usize,
    annotation: (usize, crate::types::Context<'_>),
    value: Value,
) -> Result<Value> {
    let lexical = frames.data[frame].parent;
    crate::types::prepare(ctx, &program.types[annotation.0], |ctx, name| {
        resolve_type(program, ctx, frames, storage, lexical, name, false)
    })?
    .normalize_with(ctx, value, annotation.1)
}

fn resolve_type(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    lexical: Option<usize>,
    name: &str,
    enum_only: bool,
) -> Result<Value> {
    ctx.work_bytes(name.len())?;
    let (binding, member) = name
        .split_once('.')
        .map_or((name, None), |(a, b)| (a, Some(b)));
    for fold in [false, true] {
        if member.is_some() && fold {
            break;
        }
        let mut scope = lexical;
        while let Some(index) = scope {
            ctx.charge(1)?;
            let frame = &frames.data[index];
            let function = &frame.program.functions[frame.function.unwrap()];
            let mut found = None;
            for (slot, candidate) in function.local_names.iter().enumerate() {
                ctx.charge(1)?;
                if let Some(value) = &storage.locals.data[frame.local_base + slot] {
                    if let Some(value) =
                        type_candidate(ctx, candidate, value, binding, member, fold, enum_only)?
                    {
                        merge_type(&mut found, value)?;
                    }
                }
            }
            if let Some(value) = found {
                return Ok(value);
            }
            scope = frame.parent;
        }
        let mut found = None;
        let mut declaration = None;
        if let Some(environment) = file_bindings::environment(program) {
            for (key, value) in crate::objects::bindings(ctx, environment)?.data {
                let candidate = std::str::from_utf8(key.as_bytes().unwrap()).unwrap();
                if let Some(value) =
                    type_candidate(ctx, candidate, &value, binding, member, fold, enum_only)?
                {
                    merge_type(&mut found, value)?;
                }
            }
            if let Some(value) = found {
                return Ok(value);
            }
        }
        for (index, (global, original)) in program.globals.iter().enumerate() {
            ctx.charge(1)?;
            let scoped = if file_bindings::environment(program).is_some() {
                Some(global_value(program, ctx, storage, index)?)
            } else {
                None
            };
            let value = scoped.as_ref().unwrap_or_else(|| {
                storage.globals.data[program.global_base + index]
                    .as_ref()
                    .unwrap_or(original)
            });
            if let Some(value) =
                type_candidate(ctx, global.name(), value, binding, member, fold, enum_only)?
            {
                merge_type(&mut found, value)?;
            }
        }
        for (index, value) in program.declarations.iter().enumerate() {
            ctx.charge(1)?;
            let name = match &value.0 {
                Kind::Enum(enumeration) => &enumeration.definition.name,
                Kind::Namespace(namespace) => &namespace.definition.name,
                _ => continue,
            };
            let scoped = file_bindings::get(program, ctx, name)?;
            let value = scoped.as_ref().unwrap_or_else(|| {
                storage
                    .declarations
                    .data
                    .iter()
                    .find(|(slot, _)| *slot == (program.index, index))
                    .map_or(value, |(_, value)| value)
            });
            if let Some(value) = type_candidate(ctx, name, value, binding, member, fold, enum_only)?
            {
                if found.is_none() {
                    declaration = Some(index);
                }
                merge_type(&mut found, value)?;
            }
        }
        if let Some(value) = found {
            return if let Some(index) = declaration {
                declaration_value(program, ctx, storage, index)
            } else {
                Ok(value)
            };
        }
    }
    Err(Error::new(ErrorKind::Type, "unknown named type"))
}

fn type_candidate(
    ctx: &mut CallContext,
    candidate: &str,
    value: &Value,
    binding: &str,
    member: Option<&str>,
    fold: bool,
    enum_only: bool,
) -> Result<Option<Value>> {
    let same = if fold {
        crate::text::case::equal(ctx, candidate.as_bytes(), binding.as_bytes())?
    } else {
        crate::enums::compare_names(ctx, candidate.as_bytes(), binding.as_bytes())?
            == std::cmp::Ordering::Equal
    };
    if !same {
        return Ok(None);
    }
    let Some(member) = member else {
        return Ok(matches!(value.0, Kind::Enum(_) | Kind::Namespace(_)).then(|| value.clone()));
    };
    let Kind::Hash(namespace) = &value.0 else {
        return Ok(None);
    };
    if !namespace.object {
        return Ok(None);
    }
    if let Some(index) = namespace.find(ctx, member.as_bytes())? {
        let value = &namespace.buffer.data[index].1;
        if (matches!(value.0, Kind::Enum(_))
            || (!enum_only && matches!(value.0, Kind::Namespace(_))))
        {
            return Ok(Some(value.clone()));
        }
    }
    let mut found = None;
    for (key, value) in &namespace.buffer.data {
        ctx.charge(1)?;
        if (matches!(value.0, Kind::Enum(_))
            || (!enum_only && matches!(value.0, Kind::Namespace(_))))
            && crate::text::case::equal(ctx, key.require_bytes()?, member.as_bytes())?
        {
            merge_type(&mut found, value.clone())?;
        }
    }
    Ok(found)
}

fn merge_type(found: &mut Option<Value>, value: Value) -> Result<()> {
    if let Some(previous) = found {
        let same = match (&previous.0, &value.0) {
            (Kind::Enum(a), Kind::Enum(b)) => std::sync::Arc::ptr_eq(&a.definition, &b.definition),
            (Kind::Namespace(a), Kind::Namespace(b)) => a.same_binding(b),
            _ => false,
        };
        if !same {
            return Err(Error::new(ErrorKind::Type, "ambiguous named type"));
        }
    } else {
        *found = Some(value);
    }
    Ok(())
}

fn value_invocation(value: &Value) -> crate::arguments::Target {
    match &value.0 {
        Kind::Builtin(builtin) => crate::arguments::Target::Plain(Invocation::Builtin(*builtin)),
        Kind::Offset(offset) => crate::arguments::Target::Offset(offset.clone()),
        _ => crate::arguments::Target::Plain(Invocation::NonCallable),
    }
}

fn global_index(program: &Program, name: &str) -> Option<usize> {
    if program.declaration_names.contains_key(name) {
        return None;
    }
    let namespace = Global::parse(name)?;
    program
        .globals
        .iter()
        .position(|(kind, _)| *kind == namespace)
}

fn declaration_value(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    index: usize,
) -> Result<Value> {
    let scoped = file_bindings::environment(program).is_some();
    if scoped {
        if let Some(value) = file_bindings::get(
            program,
            ctx,
            file_bindings::declaration_name(program, index),
        )? {
            return Ok(value);
        }
    }
    for (cached, value) in &storage.declarations.data {
        ctx.charge(1)?;
        if *cached == (program.index, index) {
            return Ok(value.clone());
        }
    }
    let value = if let Kind::Namespace(namespace) = &program.declarations[index].0 {
        namespaces::value(program, ctx, storage, namespace.definition.index)?
    } else if let Kind::Enum(enumeration) = &program.declarations[index].0 {
        ctx.charge(1)?;
        Value(Kind::Enum(if scoped {
            crate::enums::Enumeration::fresh(ctx, enumeration)?
        } else {
            crate::enums::Enumeration::instantiate(ctx, enumeration)?
        }))
    } else {
        ctx.import(&program.declarations[index])?
    };
    if scoped {
        file_bindings::set(
            program,
            ctx,
            storage,
            file_bindings::declaration_name(program, index),
            &value,
        )?;
    }
    storage
        .declarations
        .push(ctx, ((program.index, index), value.clone()))?;
    Ok(value)
}

fn global_value(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    index: usize,
) -> Result<Value> {
    if file_bindings::environment(program).is_some() {
        if let Some(value) = file_bindings::get(program, ctx, program.globals[index].0.name())? {
            return Ok(value);
        }
        let slot = file_bindings::global_slot(program, ctx, storage, index)?;
        return Ok(storage.globals.data[slot].as_ref().unwrap().clone());
    }
    let slot = program.global_base + index;
    if storage.globals.data[slot].is_none() {
        storage.globals.data[slot] = Some(ctx.import(&program.globals[index].1)?);
    }
    Ok(storage.globals.data[slot].as_ref().unwrap().clone())
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
        return ctx.guard(ErrorKind::Recursion, "recursion limit exceeded");
    }
    if args.len() != fun.params.len() {
        return Err(Error::argument(format!(
            "{} expects {} arguments, got {}",
            fun.name,
            fun.params.len(),
            args.len()
        )));
    }
    let mut frame = new_frame(ctx, program, storage, Some(function), base)?;
    frame.home = (function != 0 && !fun.initializer).then_some(frames.data.len());
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
    call: impl Into<crate::namespace::Call>,
    mut arguments: Arguments,
    base: usize,
) -> Result<()> {
    let mut call = call.into();
    let owner = call
        .receiver
        .as_ref()
        .map(|receiver| programs::receiver(ctx, storage, receiver))
        .transpose()?
        .flatten();
    let program = owner.as_deref().unwrap_or(program);
    let function = call.function;
    if call.constructor {
        let Some(Value(Kind::Namespace(class))) = &call.receiver else {
            unreachable!()
        };
        call.receiver = Some(Value(Kind::Instance(crate::objects::new(ctx, class)?)));
        if call.ignore_arguments {
            arguments = Arguments::empty();
        }
    }
    let fun = &program.functions[function];
    if fun.instance
        && !matches!(&call.receiver, Some(Value(Kind::Instance(instance))) if fun.namespace.is_some_and(|namespace| program.namespace_matches(namespace, instance.class())))
    {
        return Err(Error::new(
            ErrorKind::Type,
            "instance method requires its instance receiver",
        ));
    }
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
        let frame = frames.data.last_mut().unwrap();
        frame.block = arguments.block;
        frame.receiver = call.receiver;
        frame.constructor = call.constructor;
        return Ok(());
    }
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.guard(ErrorKind::Recursion, "recursion limit exceeded");
    }
    let block = arguments.block;
    let binding = Binding::new(ctx, &fun.params, arguments)?;
    let mut frame = new_frame(ctx, program, storage, Some(function), base)?;
    frame.home = (function != 0 && !fun.initializer).then_some(frames.data.len());
    frame.block = block;
    frame.receiver = call.receiver;
    frame.constructor = call.constructor;
    if fun.binds_parameters {
        frame.binding = Buffer::with_capacity(ctx, 1)?;
        frame.binding.data.push(binding);
    } else {
        for (i, param) in fun.params.iter().enumerate() {
            storage.locals.data[frame.local_base + param.slot] = binding.value(ctx, i)?;
        }
    }
    frames.push(ctx, frame)
}

fn enter_iteration(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    base: usize,
    args: Arguments,
    iteration: Iteration,
) -> Result<()> {
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.guard(ErrorKind::Recursion, "recursion limit exceeded");
    }
    let mut frame = new_frame(ctx, program, storage, None, base)?;
    frame.block = args.block;
    frame.arguments.push(ctx, args)?;
    storage.iterations.push(ctx, iteration)?;
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
        program: storage.programs.data[program.index].program.clone(),
        activation: false,
        receiver: None,
        constructor: false,
        return_to: ReturnTo::Stack,
        function,
        mutating: false,
        ip: 0,
        iteration_base: storage.iterations.data.len(),
        base,
        local_base,
        address_base: storage.addresses.data.len(),
        bypass_base: storage.bypasses.data.len(),
        text_base: storage.texts.data.len(),
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
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    mut frame: usize,
    mut slot: usize,
    skip: bool,
) -> Result<usize> {
    let own = frames.data[frame].local_base + slot;
    let function = &frames.data[frame].program.functions[frames.data[frame].function.unwrap()];
    if function.initializer && storage.locals.data[own].is_none() {
        if let Some(slot) =
            namespaces::ambient_slot(ctx, frames, storage, frame, &function.local_names[slot])?
        {
            return Ok(slot);
        }
    }
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
        let Some(capture) = frames.data[frame].program.functions
            [frames.data[frame].function.unwrap()]
        .captures
        .get(slot)
        .copied()
        .flatten() else {
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
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    block: Block,
    args: &[Value],
    base: usize,
) -> Result<()> {
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return ctx.guard(ErrorKind::Recursion, "recursion limit exceeded");
    }
    let program = frames.data[block.parent].program.clone();
    let mut frame = new_frame(ctx, &program, storage, Some(block.function), base)?;
    frame.receiver = frames.data[block.parent].receiver.clone();
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
    storage.texts.data.truncate(frame.text_base);
    frames.data.truncate(target);
}
