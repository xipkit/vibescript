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

#[cfg(feature = "tokio")]
pub(crate) mod asynchronous;
#[cfg(all(test, feature = "tokio"))]
mod asynchronous_tests;
mod call_targets;
mod capabilities;
mod dispatch;
mod execution;
mod file_bindings;
#[cfg(test)]
mod file_bindings_tests;
mod format;
mod globals;
mod handlers;
mod host_blocks;
mod host_signatures;
mod namespaces;
mod operators;
mod output;
mod programs;
mod publication;
mod requires;
mod scopes;
#[cfg(test)]
mod scopes_tests;
#[cfg(test)]
mod suspension_tests;
pub(crate) use execution::Execution;
use handlers::{Control, Event};
pub(crate) use programs::Program;

#[derive(Default)]
enum ReturnTo {
    Require(usize),
    #[default]
    Stack,
    Address,
    Assigned(Value),
    Local(usize),
    RootBinding(Value),
    Negate,
    Text(Value),
    Output,
    Format,
}

struct Frame {
    program: Arc<Program>,
    host: bool,
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
    pins: Buffer<crate::loading::Pin>,
    modules: Buffer<requires::Module>,
    bindings: Option<Arc<crate::objects::Instance>>,
    programs: Buffer<programs::Entry>,
    releasing: bool,
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

enum Exit {
    Value(Value),
    Control(Control),
}

/// Refuses a frame beyond the configured recursion limit, naming the limit as Go does.
pub(crate) fn recursion_exceeded<T>(ctx: &mut CallContext) -> Result<T> {
    let limit = ctx.options.limits.recursion;
    ctx.guard(
        ErrorKind::Recursion,
        &format!("recursion depth exceeded (limit {limit})"),
    )
}

/// Imports the host's entry arguments and the code they carry.
fn bind_entry(
    ctx: &mut CallContext,
    storage: &mut Storage,
    args: &[Value],
    keywords: &[(String, Value)],
) -> Result<Arguments> {
    let mut input = Arguments::empty();
    input.options_hash = false;
    input.positional = Buffer::with_capacity(ctx, args.len())?;
    for arg in args {
        let value = ctx.import(arg)?;
        crate::exports::check(ctx, &value)?;
        input.positional.data.push(value);
    }
    for (name, value) in keywords {
        let key = ctx.bytes(name.as_bytes())?;
        let value = ctx.import(value)?;
        crate::exports::check(ctx, &value)?;
        input.keywords.insert(ctx, key, value)?;
    }
    ctx.enum_rebind.active = false;
    programs::arguments(ctx, storage, &input)?;
    Ok(input)
}

/// Names the entry call's argument binding on a memory failure there, as Go's
/// host-visible message does.
fn entry_binding(mut error: Error) -> Error {
    if error.kind == ErrorKind::Memory {
        error
            .message
            .insert_str(0, "check memory after binding call env: ");
    }
    error
}

enum Step {
    Host,
    Complete(Exit),
}

struct Run {
    root: Arc<Program>,
    active: Arc<Program>,
    loader: Arc<crate::loading::Loader>,
    frames: Buffer<Frame>,
    storage: Storage,
    stack: Buffer<Value>,
    entry: usize,
    pending_entry: Option<(usize, Arguments)>,
    initializer: usize,
}

impl Run {
    fn new(
        code: &Arc<crate::code::Code>,
        loader: &Arc<crate::loading::Loader>,
        ctx: &mut CallContext,
        function: usize,
        args: &[Value],
        keywords: &[(String, Value)],
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let mut storage = Storage {
            pins: Buffer::empty(),
            modules: Buffer::empty(),
            bindings: None,
            programs: Buffer::empty(),
            releasing: false,
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
        globals::validate(ctx)?;
        let environment = code
            .program
            .file
            .then(|| crate::objects::environment(ctx))
            .transpose()?;
        let (root, _) = programs::load(ctx, &mut storage, code, environment.as_ref())?;
        let program = &*root;
        capabilities::bind(ctx, &mut storage)?;
        let input = bind_entry(ctx, &mut storage, args, keywords).map_err(entry_binding)?;
        let initializer = if function == 0 && !program.file {
            program.namespaces.len()
        } else {
            0
        };
        Ok(Self {
            active: root.clone(),
            root,
            loader: loader.clone(),
            frames: Buffer::empty(),
            storage,
            stack: Buffer::empty(),
            entry: function,
            pending_entry: Some((function, input)),
            initializer,
        })
    }
    fn run(&mut self, ctx: &mut CallContext) -> Result<Value> {
        match self.until(ctx, None)? {
            Exit::Value(value) => Ok(value),
            Exit::Control(_) => unreachable!(),
        }
    }

    fn until(&mut self, ctx: &mut CallContext, boundary: Option<usize>) -> Result<Exit> {
        let mut pending = None;
        loop {
            match self.resume(ctx, boundary, pending.take())? {
                Step::Host => pending = Some(self.host(ctx)),
                Step::Complete(exit) => return Ok(exit),
            }
        }
    }

    fn resume(
        &mut self,
        ctx: &mut CallContext,
        boundary: Option<usize>,
        mut pending: Option<Result<Event>>,
    ) -> Result<Step> {
        ctx.checkpoint()?;
        let floor = boundary.unwrap_or(0);
        loop {
            if boundary.is_some_and(|floor| self.frames.data.len() == floor) {
                ctx.checkpoint()?;
                return Ok(Step::Complete(Exit::Value(self.stack.data.pop().unwrap())));
            }
            let event = match pending.take() {
                Some(event) => event,
                None => match self.advance(ctx) {
                    Ok(Event::Host) => return Ok(Step::Host),
                    event => event,
                },
            };
            let program = &*self.root;
            let function = self.entry;
            let pending_entry = &self.pending_entry;
            let frames = &mut self.frames;
            let storage = &mut self.storage;
            let stack = &mut self.stack;
            let outcome = (|| -> Result<Option<Exit>> {
                match event? {
                    Event::Host => unreachable!(),
                    Event::Error(error) => {
                        handlers::error(ctx, frames, storage, stack, error, floor)?;
                        Ok(None)
                    }
                    Event::Control(control) => {
                        if let Some(control) =
                            handlers::intercept(ctx, frames, storage, stack, control, floor)?
                        {
                            if control.exits(storage, floor) {
                                unwind(frames, storage, stack, floor);
                                return Ok(Some(Exit::Control(control)));
                            }
                            handlers::apply_control(
                                ctx,
                                frames,
                                storage,
                                stack,
                                pending_entry.is_some(),
                                control,
                            )
                            .map(|value| value.map(Exit::Value))
                        } else {
                            Ok(None)
                        }
                    }
                }
            })();
            match outcome {
                Ok(Some(exit)) => return Ok(Step::Complete(exit)),
                Ok(None) => (),
                Err(error) => {
                    if ctx.exhausted() || storage.handlers.data.is_empty() {
                        return Err(error);
                    }
                    ctx.checkpoint()?;
                    let error =
                        handlers::SavedError::new(program, ctx, &frames.data, function, error)?;
                    handlers::error(ctx, frames, storage, stack, error, floor)?;
                }
            }
            if storage.releasing {
                programs::release(ctx, storage)?;
            }
        }
    }

    // Keep opcode temporaries off the Rust stack during host-driven reentry.
    #[inline(never)]
    fn advance(&mut self, ctx: &mut CallContext) -> Result<Event> {
        let program = &*self.root;
        let active = &mut self.active;
        let initializer = &mut self.initializer;
        let pending_entry = &mut self.pending_entry;
        let loader = &self.loader;
        let frames = &mut self.frames;
        let storage = &mut self.storage;
        let stack = &mut self.stack;
        loop {
            programs::advance(ctx, frames, storage, stack.data.len())?;
            if frames.data.is_empty() {
                while *initializer < program.namespaces.len() {
                    let module = *initializer;
                    *initializer += 1;
                    if let Some(body) = program.namespaces[module].body {
                        let state = namespaces::state(program, ctx, storage, module)?;
                        if !storage.namespaces.data[state].initialized {
                            enter_arguments(
                                program,
                                ctx,
                                frames,
                                storage,
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
                    enter_arguments(program, ctx, frames, storage, function, input, 0)
                        .map_err(entry_binding)?;
                }
            }
            let current = frames.data.len() - 1;
            if !Arc::ptr_eq(active, &frames.data[current].program) {
                *active = frames.data[current].program.clone();
                if storage.releasing {
                    programs::release(ctx, storage)?;
                }
            }
            let program = &**active;
            let hosts = &program.code.hosts;
            if frames.data[current].host {
                return Ok(Event::Host);
            }
            if frames.data[current].function.is_none() {
                ctx.charge(1)?;
                let iteration = &mut storage.iterations.data[frames.data[current].iteration_base];
                let returned = if iteration.waiting() {
                    Some(stack.data.pop().unwrap())
                } else {
                    None
                };
                match iteration.advance(ctx, returned)? {
                    Progress::Yield(args, count) => {
                        for value in &args[..count] {
                            crate::exports::check(ctx, value)?;
                        }
                        let block = frames.data[current].block.unwrap();
                        enter_block(
                            ctx,
                            frames,
                            storage,
                            block,
                            &args[..count],
                            stack.data.len(),
                        )?;
                    }
                    Progress::Call(receiver, operation, argument) => {
                        dispatch::reduce(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack,
                            [receiver, operation, argument],
                        )?;
                    }
                    Progress::Done(mut value) => {
                        let mutation = iteration.take_mutation();
                        if frames.data[current].mutating {
                            let address = storage.addresses.data.pop().unwrap();
                            if let Some(mutation) = mutation {
                                let guard_program = programs::address(ctx, storage, &address)?;
                                let guard = address_guard(
                                    guard_program.as_deref(),
                                    ctx,
                                    frames,
                                    storage,
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
                        unwind(frames, storage, stack, current);
                        crate::exports::check(ctx, &value)?;
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
            let mut slot = |slot, skip| resolve_slot(ctx, frames, storage, current, slot, skip);
            let binding_local = file_bindings::local_name(op);
            let file_local = if program.file {
                file_bindings::local_name(op)
            } else {
                None
            };
            let mut op = match op {
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
            if !ctx.options.globals.is_empty() || !ctx.capability_names.data.is_empty() {
                let count = match op {
                    Op::Call(_, count) | Op::Host(_, count) | Op::NonCallable(count) => Some(count),
                    _ => None,
                };
                if let Some(count) = count {
                    let target = frames.data[current].arguments.data.pop().unwrap().target;
                    if let Some(target) = target {
                        let base = stack.data.len() - count;
                        let mut args = Arguments::from_values(ctx, &stack.data[base..])?;
                        args.target = Some(target);
                        stack.data.truncate(base);
                        frames.data[current].arguments.push(ctx, args)?;
                        op = Op::Invoke(Invocation::Resolved);
                    }
                }
            }
            let file_local = if let Some(relative) = file_local {
                let absolute = file_bindings::local_name(op).unwrap();
                file_bindings::local(program, ctx, frames, storage, current, relative, absolute)?
                    .then_some(
                        program.functions[frames.data[current].function.unwrap()].local_names
                            [relative]
                            .as_str(),
                    )
            } else {
                None
            };
            let root_local = if let Some(relative) = binding_local {
                let absolute = file_bindings::local_name(op).unwrap();
                requires::local(ctx, frames, storage, current, relative, absolute)?.then_some(
                    program.functions[frames.data[current].function.unwrap()].local_names[relative]
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
                Op::TryBegin(spec) => handlers::begin(ctx, frames, storage, stack, spec)?,
                Op::TryBody | Op::TryEnd => {
                    handlers::normal(ctx, frames, storage, stack, matches!(op, Op::TryBody))?
                }
                Op::EnsureEnd => {
                    if let Some(event) = handlers::end_ensure(frames, storage, stack, ctx)? {
                        return Ok(event);
                    }
                }
                Op::Retry => {
                    return Ok(Event::Control(handlers::retry(storage)?));
                }
                Op::RaiseStart(named, target) => {
                    let class = if let Some((name, slot)) = named {
                        let local = if let Some(slot) = slot {
                            let slot = resolve_slot(ctx, frames, storage, current, slot, false)?;
                            storage.locals.data[slot].is_some()
                        } else {
                            false
                        };
                        let name = &program.members[name];
                        let bound = local
                            || runtime_bound(program, ctx, frames, storage, current, name)?
                            || handlers::class_constant_bound(ctx, frames, storage, current, name)?;
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
                    args.target = Some(crate::arguments::Target::Raise(class, Value::nil()));
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
                        if let Some(error) = handlers::current_error(storage) {
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
                    let Some(Value(Kind::Instance(instance))) = frame.receiver.as_ref() else {
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
                        frames,
                        storage,
                        &instance,
                        &program.members[name],
                        value,
                    )?;
                    set_ivar(ctx, storage, &instance, &program.members[name], &value)?;
                    storage.locals.data[slot] = Some(value);
                }
                Op::InitNamespace(module) => {
                    let state = namespaces::state(program, ctx, storage, module)?;
                    if !storage.namespaces.data[state].initialized {
                        let body = program.namespaces[module].body.unwrap();
                        enter_arguments(
                            program,
                            ctx,
                            frames,
                            storage,
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
                            namespaces::ambient_slot(ctx, frames, storage, current, name)?
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
                    let name = &program.members[name];
                    if let Some(mut value) = file_bindings::get(program, ctx, name)? {
                        if let Kind::Offset(offset) = &value.0 {
                            return Err(offset.value_error());
                        }
                        if let Kind::Builtin(builtin) = value.0 {
                            value = builtin.read(ctx)?;
                        }
                        stack.push(ctx, value)?;
                        frame.ip = next;
                    } else if !program.names.contains_key(name)
                        && !program.declaration_names.contains_key(name)
                        && !program.hosts.iter().any(|host| host == name)
                    {
                        if let Some(binding) =
                            file_bindings::root_binding(program, ctx, storage, name)?
                        {
                            frames.data[current].ip = next;
                            file_bindings::read_root(ctx, frames, storage, stack, binding)?;
                        }
                    }
                }
                Op::FileAddress(name, next) => {
                    let name = &program.members[name];
                    if file_bindings::unshadowed(program, ctx, frames, storage, current, name)? {
                        let address = if file_bindings::get(program, ctx, name)?.is_some() {
                            Some(file_bindings::address(program, ctx, name)?)
                        } else {
                            requires::address(program, ctx, frames, storage, current, name)?
                        };
                        if let Some(address) = address {
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next;
                        }
                    }
                }
                Op::RootAddress(name, next) => {
                    if let Some(address) = requires::address(
                        program,
                        ctx,
                        frames,
                        storage,
                        current,
                        &program.members[name],
                    )? {
                        storage.addresses.push(ctx, address)?;
                        frames.data[current].ip = next;
                    }
                }
                Op::PrepareMember(site, mutating) => {
                    let receiver = if mutating {
                        &storage.addresses.data.last().unwrap().value
                    } else {
                        stack.data.last().unwrap()
                    };
                    if matches!(receiver.0, Kind::Hash(_)) {
                        let field =
                            members::prepare(ctx, site, &program.members[site.name], receiver)?;
                        match field {
                            Some(Value(Kind::Host(method))) if mutating => {
                                let selected = crate::capability::SelectedMethod {
                                    method,
                                    receiver: receiver.clone(),
                                };
                                storage.addresses.data.last_mut().unwrap().capability =
                                    Some(selected);
                            }
                            // The receiver stays on the stack so the call can snapshot
                            // it; dispatch selects the same field from the same value.
                            Some(Value(Kind::Host(_))) => (),
                            Some(Value(Kind::Function(function))) if mutating => {
                                storage.addresses.data.last_mut().unwrap().exported =
                                    Some(function);
                            }
                            _ => (),
                        }
                    }
                }
                Op::NamespaceSelf(module) => {
                    let value = if let Some(value) = &self_value {
                        value.clone()
                    } else {
                        namespaces::value(program, ctx, storage, module)?
                    };
                    stack.push(ctx, value)?;
                }
                Op::NamespaceConstant(name, next) => {
                    if let Some(value) = namespaces::field(
                        program,
                        ctx,
                        storage,
                        namespace.unwrap(),
                        &program.members[name],
                    )? {
                        stack.push(ctx, value)?;
                        frames.data[current].ip = next;
                    }
                }
                Op::NamespaceConstantAddress(name, next) => {
                    let module = namespace.unwrap();
                    let name = &program.members[name];
                    if namespaces::field(program, ctx, storage, module, name)?.is_some() {
                        let address =
                            namespaces::address(program, ctx, storage, module, name, false)?;
                        storage.addresses.push(ctx, address)?;
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
                        let value =
                            crate::objects::field(ctx, instance, &raw[1..])?.unwrap_or_default();
                        stack.push(ctx, value)?;
                        continue;
                    }
                    let (module, name) =
                        namespaces::variable_name(namespace, &program.members[name])?;
                    let value = namespaces::field(program, ctx, storage, module, name)?;
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
                        && namespaces::field(program, ctx, storage, module, name)?.is_none()
                    {
                        if let Some(slot) =
                            namespaces::ambient_slot(ctx, frames, storage, current, name)?
                        {
                            Address::new(
                                Some(slot),
                                storage.locals.data[slot].as_ref().unwrap().clone(),
                            )
                        } else if let Some(&index) = program.declaration_names.get(name) {
                            Address::new(None, declaration_value(program, ctx, storage, index)?)
                        } else if let Some(global) = global_index(program, name) {
                            file_bindings::global_address(program, ctx, storage, global, false)?
                        } else {
                            return Err(Error::new(ErrorKind::Name, "undefined class constant"));
                        }
                    } else {
                        namespaces::address(program, ctx, storage, module, name, optional)?
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
                            frames,
                            storage,
                            instance,
                            &raw[1..],
                            stack.data.last().unwrap().clone(),
                        )?;
                        set_ivar(ctx, storage, instance, &raw[1..], &value)?;
                        *stack.data.last_mut().unwrap() = value;
                        continue;
                    }
                    let (module, name) =
                        namespaces::variable_name(namespace, &program.members[name])?;
                    namespaces::set(
                        program,
                        ctx,
                        storage,
                        module,
                        name,
                        stack.data.last().unwrap().clone(),
                    )?;
                }
                Op::StoreDeclaration(index) => {
                    if !program.file
                        && requires::contains(
                            ctx,
                            storage,
                            file_bindings::declaration_name(program, index),
                        )?
                    {
                        requires::set(
                            ctx,
                            storage,
                            file_bindings::declaration_name(program, index),
                            stack.data.last().unwrap(),
                        )?;
                        continue;
                    }
                    if file_bindings::environment(program).is_some() {
                        file_bindings::set(
                            program,
                            ctx,
                            storage,
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
                        // A method reachable through implicit self also turns
                        // the braced group back into a hash, as a bare call.
                        if runtime_bound(program, ctx, frames, storage, current, name)?
                            || matches!(
                                namespaces::implicit(
                                    program,
                                    ctx,
                                    storage,
                                    namespace,
                                    self_value.as_ref(),
                                    name,
                                )?,
                                namespaces::Member::Function(_)
                            )
                        {
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
                            frames,
                            storage,
                            call,
                            Arguments::empty(),
                            stack.data.len(),
                        )?;
                        frames.data.last_mut().unwrap().return_to = ReturnTo::Text(value);
                        continue;
                    }
                    crate::text::append(ctx, &value, storage.texts.data.last_mut().unwrap())?;
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
                    } else if let Some(name) = root_local {
                        requires::get(ctx, storage, name)?.unwrap()
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
                    } else if let Some(name) = root_local {
                        requires::get(ctx, storage, name)?
                    } else {
                        None
                    };
                    if let Some(value) = scoped.as_ref().or(storage.locals.data[slot].as_ref()) {
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
                        storage,
                        namespace,
                        &program.members[name],
                    )? {
                        stack.push(ctx, value)?;
                    } else if let Some(slot) = namespaces::ambient_slot(
                        ctx,
                        frames,
                        storage,
                        current,
                        &program.members[name],
                    )? {
                        stack.push(ctx, storage.locals.data[slot].as_ref().unwrap().clone())?;
                    } else if let Some(&index) =
                        program.declaration_names.get(&program.members[name])
                    {
                        let value = declaration_value(program, ctx, storage, index)?;
                        stack.push(ctx, value)?;
                    } else if let Some(&function) = program.names.get(&program.members[name]) {
                        enter_auto(program, ctx, frames, storage, function, stack.data.len())?;
                    } else if let Some(host) = program
                        .hosts
                        .iter()
                        .position(|h| h == &program.members[name])
                    {
                        return Err(callable_value_error(&program.hosts[host], "method"));
                    } else if let Some(binding) =
                        file_bindings::root_binding(program, ctx, storage, &program.members[name])?
                    {
                        file_bindings::read_root(ctx, frames, storage, stack, binding)?;
                    } else if let Some(global) = global_index(program, &program.members[name]) {
                        let mut value = global_value(program, ctx, storage, global)?;
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
                    } else if let Some(name) = root_local {
                        requires::get(ctx, storage, name)?
                    } else {
                        None
                    };
                    if let Some(value) = scoped.as_ref().or(storage.locals.data[slot].as_ref()) {
                        stack.push(ctx, value.clone())?;
                        frame.ip = next;
                    }
                }
                Op::Unbound(name) => {
                    if let Some(binding) =
                        file_bindings::root_binding(program, ctx, storage, &program.members[name])?
                    {
                        file_bindings::read_root(ctx, frames, storage, stack, binding)?;
                        continue;
                    }
                    match namespaces::implicit(
                        program,
                        ctx,
                        storage,
                        namespace,
                        self_value.as_ref(),
                        &program.members[name],
                    )? {
                        namespaces::Member::Function(function) => {
                            enter_arguments(
                                program,
                                ctx,
                                frames,
                                storage,
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
                                frames,
                                storage,
                                (module, helper),
                                &Arguments::empty(),
                                true,
                            )?;
                            stack.push(ctx, value)?;
                        }
                        namespaces::Member::Missing => {
                            namespaces::fallback(&program.members[name])?;
                            let module = namespace
                                .ok_or_else(|| Error::new(ErrorKind::Name, "undefined variable"))?;
                            let receiver = if let Some(value) = &self_value {
                                value.clone()
                            } else {
                                namespaces::value(program, ctx, storage, module)?
                            };
                            let site = crate::bytecode::CallSite {
                                name,
                                method: crate::bytecode::Method::parse(&program.members[name]),
                                auto: true,
                                parenthesized: false,
                                scope: false,
                            };
                            let (_, value) =
                                members::call(ctx, site, &program.members[name], receiver, &[])?;
                            stack.push(ctx, value)?;
                        }
                    }
                }
                Op::Declaration(index) => {
                    let value = declaration_value(program, ctx, storage, index)?;
                    stack.push(ctx, value)?;
                }
                Op::Global(index) => {
                    if program.file
                        && file_bindings::get(program, ctx, program.globals[index].0.name())?
                            .is_none()
                    {
                        if let Some(binding) = file_bindings::root_binding(
                            program,
                            ctx,
                            storage,
                            program.globals[index].0.name(),
                        )? {
                            file_bindings::read_root(ctx, frames, storage, stack, binding)?;
                            continue;
                        }
                    }
                    let mut value = global_value(program, ctx, storage, index)?;
                    if let Kind::Offset(offset) = &value.0 {
                        return Err(offset.value_error());
                    }
                    if let Kind::Builtin(builtin) = value.0 {
                        value = builtin.read(ctx)?;
                    }
                    stack.push(ctx, value)?;
                }
                Op::GlobalReceiver(index, auto) => {
                    if program.file
                        && file_bindings::get(program, ctx, program.globals[index].0.name())?
                            .is_none()
                    {
                        if let Some(binding) = file_bindings::root_binding(
                            program,
                            ctx,
                            storage,
                            program.globals[index].0.name(),
                        )? {
                            file_bindings::read_root(ctx, frames, storage, stack, binding)?;
                            continue;
                        }
                    }
                    let mut value = global_value(program, ctx, storage, index)?;
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
                    if !program.file
                        && requires::contains(ctx, storage, program.globals[index].0.name())?
                    {
                        requires::set(
                            ctx,
                            storage,
                            program.globals[index].0.name(),
                            stack.data.last().unwrap(),
                        )?;
                        continue;
                    }
                    if file_bindings::environment(program).is_some() {
                        file_bindings::set(
                            program,
                            ctx,
                            storage,
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
                    let value = global_value(program, ctx, storage, index)?;
                    let mut arguments = Arguments::empty();
                    arguments.target = Some(value_invocation(&value));
                    frame.arguments.push(ctx, arguments)?;
                }
                Op::AddressGlobal(index) => {
                    if program.file
                        && file_bindings::get(program, ctx, program.globals[index].0.name())?
                            .is_none()
                    {
                        if let Some(binding) = file_bindings::root_binding(
                            program,
                            ctx,
                            storage,
                            program.globals[index].0.name(),
                        )? {
                            match binding {
                                file_bindings::RootBinding::Value(value) => {
                                    storage.addresses.push(ctx, Address::new(None, value))?
                                }
                                file_bindings::RootBinding::Function(owner, function) => {
                                    enter_auto(
                                        &owner,
                                        ctx,
                                        frames,
                                        storage,
                                        function,
                                        stack.data.len(),
                                    )?;
                                    frames.data.last_mut().unwrap().return_to = ReturnTo::Address;
                                }
                                file_bindings::RootBinding::Host(owner, host) => {
                                    return Err(callable_value_error(&owner.hosts[host], "method"));
                                }
                            }
                            continue;
                        }
                    }
                    let address =
                        file_bindings::global_address(program, ctx, storage, index, true)?;
                    storage.addresses.push(ctx, address)?;
                }
                Op::NonCallable(_) => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "attempted to call non-callable value",
                    ));
                }
                Op::Bind(param, next) => {
                    if let Some(value) = frame.binding.data[0].value(ctx, param)? {
                        let param = &program.functions[frame.function.unwrap()].params[param];
                        let slot = frame.local_base + param.slot;
                        let ty = param.ty;
                        let value = if let Some(ty) = ty {
                            normalize_type(
                                program,
                                ctx,
                                frames,
                                storage,
                                current,
                                (ty, crate::types::Context::Argument(param.name.as_bytes())),
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
                        frames,
                        storage,
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
                        file_bindings::declare(program, ctx, storage, name)?;
                    } else if root_local.is_none() {
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
                    enter_block(ctx, frames, storage, block, &stack.data[base..], base)?;
                    stack.data.truncate(base);
                }
                Op::Store(n) => {
                    let value = stack.data.last().unwrap();
                    if let Some(name) = file_local {
                        file_bindings::set(program, ctx, storage, name, value)?;
                    } else if let Some(name) = root_local {
                        requires::set(ctx, storage, name, value)?;
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
                    if let Some(resolved) =
                        operators::resolve(program, ctx, &a, op, (namespace, caller_instance))?
                    {
                        let args = Arguments::from_values(ctx, &[b])?;
                        enter_arguments(
                            program,
                            ctx,
                            frames,
                            storage,
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
                    if let Some(resolved) =
                        operators::resolve(program, ctx, &a, "+", (namespace, caller_instance))?
                    {
                        let args = Arguments::from_values(ctx, &[b])?;
                        enter_arguments(
                            program,
                            ctx,
                            frames,
                            storage,
                            resolved.call,
                            args,
                            stack.data.len(),
                        )?;
                        frames.data.last_mut().unwrap().return_to = if let Some(name) = root_local {
                            ReturnTo::RootBinding(ctx.bytes(name.as_bytes())?)
                        } else {
                            ReturnTo::Local(n)
                        };
                        continue;
                    }
                    storage.locals.data[n] = None;
                    let value = ops::binary(ctx, "+", a, b)?;
                    if let Some(name) = root_local {
                        requires::set(ctx, storage, name, &value)?;
                    } else {
                        address::refresh(ctx, n, &value, &mut storage.addresses.data, &[])?;
                        storage.locals.data[n] = Some(value.clone());
                    }
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
                        .ok_or_else(|| ops::unsupported("<<"))?;
                        let args = Arguments::from_values(ctx, &[value])?;
                        enter_arguments(
                            program,
                            ctx,
                            frames,
                            storage,
                            resolved.call,
                            args,
                            stack.data.len(),
                        )?;
                        continue;
                    }
                    if !matches!(address.value.0, Kind::Array(_)) {
                        return Err(ops::unsupported("<<"));
                    }
                    let guard_program = programs::address(ctx, storage, &address)?;
                    let guard =
                        address_guard(guard_program.as_deref(), ctx, frames, storage, &address)?;
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
                        |ctx, receiver| members::call(ctx, site, "push", receiver, &[value]),
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
                Op::RangeStart => {
                    let start = stack.data.last_mut().unwrap();
                    *start = Value::int(crate::range::endpoint(start)?);
                }
                Op::Range(start, end, exclusive) => {
                    let end = end.then(|| stack.data.pop().unwrap());
                    let start = start.then(|| stack.data.pop().unwrap());
                    let start = start.as_ref().map(crate::range::endpoint).transpose()?;
                    let end = end.as_ref().map(crate::range::endpoint).transpose()?;
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
                        enter_arguments(program, ctx, frames, storage, call, args, base)?;
                        stack.data.truncate(base);
                        continue;
                    }
                    let value = if n == 1 {
                        ops::index(ctx, root, &args[0])?
                    } else {
                        crate::sequence::slice(ctx, root, args, false)?
                    };
                    if matches!(value.0, Kind::Host(_))
                        && matches!(root.0, Kind::Hash(_))
                        && matches!(
                            program.functions[frame.function.unwrap()]
                                .code
                                .get(frame.ip),
                            Some(Op::CallValue)
                        )
                    {
                        // `receiver[:name](...)` selects its callee here, before the
                        // arguments run; CallValue keeps the root only for host methods.
                        let receiver = root.clone();
                        if let Some(pending) = frame.arguments.data.last_mut() {
                            pending.receiver = Some(receiver);
                        }
                    }
                    stack.data.truncate(base);
                    stack.push(ctx, value)?;
                }
                Op::AddressLocal(n) => {
                    let address = if let Some(name) = file_local {
                        file_bindings::address(program, ctx, name)?
                    } else if let Some(name) = root_local {
                        requires::address(program, ctx, frames, storage, current, name)?.unwrap()
                    } else {
                        Address::new(Some(n), storage.locals.data[n].clone().unwrap_or_default())
                    };
                    storage.addresses.push(ctx, address)?;
                }
                Op::AddressBound(slot, next) => {
                    if let Some(name) = root_local {
                        if let Some(address) =
                            requires::address(program, ctx, frames, storage, current, name)?
                        {
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next;
                            continue;
                        }
                    }
                    if let Some(name) =
                        file_local.filter(|_| file_bindings::environment(program).is_some())
                    {
                        if file_bindings::get(program, ctx, name)?.is_some() {
                            let address = file_bindings::address(program, ctx, name)?;
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next;
                        }
                    } else if let Some(value) = &storage.locals.data[slot] {
                        storage
                            .addresses
                            .push(ctx, Address::new(Some(slot), value.clone()))?;
                        frames.data[current].ip = next;
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
                        enter_arguments(program, ctx, frames, storage, call, args, base)?;
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
                            storage,
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
                        let owner = programs::namespace(ctx, storage, receiver)?;
                        let address = namespaces::address(
                            &owner,
                            ctx,
                            storage,
                            receiver.definition.index,
                            name,
                            false,
                        )?;
                        storage.addresses.push(ctx, address)?;
                    } else {
                        let (_, value) = members::call(ctx, site, name, address.value, &[])?;
                        storage.addresses.push(ctx, Address::new(None, value))?;
                    }
                }
                Op::AddressMember(site) => {
                    let name = &program.members[site.name];
                    if let Some(function) = crate::exports::member(
                        ctx,
                        site,
                        name,
                        &storage.addresses.data.last().unwrap().value,
                    )? {
                        if site.auto && site.scope {
                            return Err(function.value_error());
                        }
                        storage.addresses.data.pop();
                        requires::invoke(
                            ctx,
                            frames,
                            storage,
                            &function,
                            Arguments::empty(),
                            site.auto,
                            stack.data.len(),
                        )?;
                        frames.data.last_mut().unwrap().return_to = ReturnTo::Address;
                        continue;
                    }
                    if matches!(
                        storage.addresses.data.last().unwrap().value.0,
                        Kind::Namespace(_) | Kind::Instance(_)
                    ) {
                        let receiver = storage.addresses.data.last().unwrap().value.clone();
                        match namespaces::member(
                            ctx,
                            storage,
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
                                    frames,
                                    storage,
                                    function,
                                    Arguments::empty(),
                                    stack.data.len(),
                                )?;
                                frames.data.last_mut().unwrap().return_to = ReturnTo::Address;
                                continue;
                            }
                            namespaces::Member::Value(value) => {
                                let value =
                                    capabilities::field(ctx, storage, site, value, &[], &[], None)?;
                                storage.addresses.data.pop();
                                value.finish(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
                                    stack,
                                    ReturnTo::Address,
                                )?;
                                continue;
                            }
                            namespaces::Member::Helper(module, helper) => {
                                let value = dispatch::helper(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
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
                                let value = value.clone();
                                let value =
                                    capabilities::field(ctx, storage, site, value, &[], &[], None)?;
                                storage.addresses.data.pop();
                                value.finish(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
                                    stack,
                                    ReturnTo::Address,
                                )?;
                                continue;
                            }
                        }
                        address.index(ctx, &[key])?;
                    } else {
                        let address = storage.addresses.data.pop().unwrap();
                        if !crate::bytecode::mutating_member(name) {
                            let (_, value) = members::call(ctx, site, name, address.value, &[])?;
                            storage.addresses.push(ctx, Address::new(None, value))?;
                            continue;
                        }
                        let guard_program = programs::address(ctx, storage, &address)?;
                        let guard = address_guard(
                            guard_program.as_deref(),
                            ctx,
                            frames,
                            storage,
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
                        if matches!(address.value.0, Kind::Namespace(_) | Kind::Instance(_)) {
                            let receiver = address.value.clone();
                            match namespaces::member(
                                ctx,
                                storage,
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
                                        frames,
                                        storage,
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
                                        frames,
                                        storage,
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
                            let args = Arguments::from_values(ctx, &address.selectors.data)?;
                            enter_arguments(
                                program,
                                ctx,
                                frames,
                                storage,
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
                            storage,
                            receiver,
                            name,
                            namespace,
                            caller_instance,
                        )? {
                            let args = Arguments::from_values(ctx, std::slice::from_ref(&value))?;
                            enter_arguments(
                                program,
                                ctx,
                                frames,
                                storage,
                                function,
                                args,
                                stack.data.len(),
                            )?;
                            frames.data.last_mut().unwrap().return_to = ReturnTo::Assigned(value);
                            continue;
                        }
                        match &receiver.0 {
                            Kind::Instance(instance) => {
                                set_ivar(ctx, storage, instance, name, &value)?
                            }
                            Kind::Namespace(namespace) => {
                                let owner = programs::namespace(ctx, storage, namespace)?;
                                namespaces::set(
                                    &owner,
                                    ctx,
                                    storage,
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
                            frames,
                            storage,
                            call,
                            args,
                            stack.data.len(),
                        )?;
                        frames.data.last_mut().unwrap().return_to = ReturnTo::Assigned(value);
                        continue;
                    }
                    let guard_program = programs::address(ctx, storage, &address)?;
                    let guard =
                        address_guard(guard_program.as_deref(), ctx, frames, storage, &address)?;
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
                    let address = storage.addresses.data.last().unwrap();
                    let method = if let Some(method) = &address.capability {
                        Some(method.clone())
                    } else {
                        capabilities::member(
                            ctx,
                            site,
                            &program.members[site.name],
                            &address.value,
                        )?
                        .map(|method| crate::capability::SelectedMethod {
                            method,
                            receiver: address.value.clone(),
                        })
                    };
                    if let Some(method) = method {
                        let base = stack.data.len() - n;
                        let value = capabilities::call_on(
                            ctx,
                            storage,
                            &method.method,
                            Some(&method.receiver),
                            &stack.data[base..],
                            &[],
                            None,
                            site.auto,
                        )?;
                        storage.addresses.data.pop();
                        stack.data.truncate(base);
                        value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                        continue;
                    }
                    let function = if let Some(function) = &address.exported {
                        Some(function.clone())
                    } else {
                        crate::exports::member(
                            ctx,
                            site,
                            &program.members[site.name],
                            &address.value,
                        )?
                    };
                    if let Some(function) = function {
                        if site.auto && site.scope {
                            return Err(function.value_error());
                        }
                        let base = stack.data.len() - n;
                        let args = Arguments::from_values(ctx, &stack.data[base..])?;
                        stack.data.truncate(base);
                        storage.addresses.data.pop();
                        requires::invoke(ctx, frames, storage, &function, args, site.auto, base)?;
                        continue;
                    }
                    let base = stack.data.len() - n;
                    let address = storage.addresses.data.pop().unwrap();
                    if matches!(address.value.0, Kind::Namespace(_) | Kind::Instance(_)) {
                        let receiver = &address.value;
                        let name = &program.members[site.name];
                        match namespaces::member(
                            ctx,
                            storage,
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
                                let args = Arguments::from_values(ctx, &stack.data[base..])?;
                                enter_arguments(
                                    program, ctx, frames, storage, function, args, base,
                                )?;
                                stack.data.truncate(base);
                                continue;
                            }
                            namespaces::Member::Value(value) => {
                                let value = capabilities::field_on(
                                    ctx,
                                    storage,
                                    site,
                                    Some(receiver),
                                    value,
                                    &stack.data[base..],
                                    &[],
                                    None,
                                )?;
                                stack.data.truncate(base);
                                value.finish(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
                                    stack,
                                    ReturnTo::Stack,
                                )?;
                                continue;
                            }
                            namespaces::Member::Helper(module, helper) => {
                                let args = Arguments::from_values(ctx, &stack.data[base..])?;
                                let value = dispatch::helper(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
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
                    let guard_program = programs::address(ctx, storage, &address)?;
                    let guard =
                        address_guard(guard_program.as_deref(), ctx, frames, storage, &address)?;
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
                        crate::exports::check(ctx, &value)?;
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
                Op::LoopGuard(breaking) => handlers::guard_loop(ctx, frames, breaking)?,
                Op::Break(has_value) | Op::Next(has_value) => {
                    let value = has_value.then(|| stack.data.pop().unwrap());
                    let control =
                        handlers::loop_control(frames, matches!(op, Op::Break(_)), value)?;
                    return Ok(Event::Control(control));
                }
                Op::Call(function, n) => {
                    let base = stack.data.len() - n;
                    enter(
                        program,
                        ctx,
                        frames,
                        storage,
                        function,
                        &stack.data[base..],
                        base,
                    )?;
                    stack.data.truncate(base);
                }
                Op::AutoCall(function) => {
                    if let Some(value) =
                        globals::get(ctx, storage, &program.functions[function].name)?
                    {
                        file_bindings::read_root(
                            ctx,
                            frames,
                            storage,
                            stack,
                            file_bindings::RootBinding::Value(value),
                        )?;
                        continue;
                    }
                    enter_auto(program, ctx, frames, storage, function, stack.data.len())?;
                }
                Op::HostValue(host) => {
                    if let Some(value) = globals::get(ctx, storage, &program.hosts[host])? {
                        file_bindings::read_root(
                            ctx,
                            frames,
                            storage,
                            stack,
                            file_bindings::RootBinding::Value(value),
                        )?;
                        continue;
                    }
                    return Err(callable_value_error(&program.hosts[host], "method"));
                }
                Op::RootCall(name, expanded) => {
                    if expanded
                        || !ctx.options.globals.is_empty()
                        || !ctx.capability_names.data.is_empty()
                    {
                        let mut args = Arguments::empty();
                        if let Some(value) = globals::get(ctx, storage, &program.members[name])? {
                            args.target = Some(value_invocation(&value));
                        }
                        frame.arguments.push(ctx, args)?;
                    }
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
                        program, ctx, frames, storage, current, slot, name,
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
                    let args = frame.arguments.data.last_mut().unwrap();
                    // An immediate `receiver[:name](...)` left its root here.
                    let pending = args.receiver.take();
                    args.resolve(value_invocation(&value), true);
                    args.keep_receiver(pending);
                }
                Op::CallMember(site) => {
                    let receiver = stack.data.pop().unwrap();
                    let selected = receiver.clone();
                    let target = call_targets::member(
                        program,
                        ctx,
                        storage,
                        receiver,
                        site,
                        namespace,
                        self_value.is_some(),
                    )?;
                    let args = frame.arguments.data.last_mut().unwrap();
                    args.resolve(target, site.parenthesized);
                    args.keep_receiver(Some(selected));
                }
                Op::ResolveCall(slot, name, parenthesized) => {
                    let name_index = name;
                    let name = &program.members[name];
                    let target = if let Some(Some(value)) = storage.locals.data.get(slot) {
                        value_invocation(value)
                    } else if let Some(value) = namespaces::call_constant(
                        program,
                        ctx,
                        storage,
                        namespace,
                        caller_instance,
                        name,
                    )? {
                        value_invocation(&value)
                    } else if let Some(slot) =
                        namespaces::ambient_slot(ctx, frames, storage, current, name)?
                    {
                        value_invocation(storage.locals.data[slot].as_ref().unwrap())
                    } else if let Some(value) = file_bindings::get(program, ctx, name)? {
                        value_invocation(&value)
                    } else if !program.file && globals::contains(ctx, name)? {
                        value_invocation(&requires::get(ctx, storage, name)?.unwrap())
                    } else if program.declaration_names.contains_key(name) {
                        crate::arguments::Target::Plain(Invocation::NonCallable)
                    } else if let Some(&function) = program.names.get(name) {
                        crate::arguments::Target::Plain(Invocation::Function(function))
                    } else if let Some(host) = program.hosts.iter().position(|h| h == name) {
                        crate::arguments::Target::Plain(Invocation::Host(host))
                    } else if let Some(binding) =
                        file_bindings::root_binding(program, ctx, storage, name)?
                    {
                        binding.target()
                    } else if let Some(global) = global_index(program, name) {
                        value_invocation(&global_value(program, ctx, storage, global)?)
                    } else {
                        match namespaces::implicit(
                            program,
                            ctx,
                            storage,
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
                    frames.data[current].arguments.push(ctx, arguments)?;
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
                Op::Invoke(target) | Op::InvokeRoot(target) => {
                    let mut args = frame.arguments.data.pop().unwrap();
                    let target = if matches!(op, Op::InvokeRoot(_))
                        || matches!(target, Invocation::Resolved)
                    {
                        args.target
                            .take()
                            .unwrap_or(crate::arguments::Target::Plain(target))
                    } else {
                        crate::arguments::Target::Plain(target)
                    };
                    let target = match target {
                        crate::arguments::Target::Capability(method) => {
                            let value = capabilities::call_on(
                                ctx,
                                storage,
                                &method,
                                args.receiver.as_ref(),
                                &args.positional.data,
                                &args.keywords.buffer.data,
                                args.block,
                                false,
                            )?;
                            value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                            continue;
                        }
                        crate::arguments::Target::Host(owner, host) => {
                            let value = capabilities::registered(
                                ctx,
                                storage,
                                &owner.code.hosts[host],
                                &args.positional.data,
                                &args.keywords.buffer.data,
                                args.block,
                            )?;
                            value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                            continue;
                        }
                        crate::arguments::Target::Export(function) => {
                            requires::invoke(
                                ctx,
                                frames,
                                storage,
                                &function,
                                args,
                                false,
                                stack.data.len(),
                            )?;
                            continue;
                        }
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
                                    method: crate::bytecode::Method::parse(&program.members[name]),
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
                                frames,
                                storage,
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
                                frames,
                                storage,
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
                                frames,
                                storage,
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
                            namespaces::value(program, ctx, storage, module)?
                        };
                        stack.push(ctx, receiver)?;
                        Invocation::Member(
                            crate::bytecode::CallSite {
                                name,
                                method: crate::bytecode::Method::parse(&program.members[name]),
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
                            if builtin == crate::builtin::Builtin::Require {
                                requires::start(
                                    program, loader, ctx, frames, storage, stack, args,
                                )?;
                                continue;
                            }
                            if let crate::builtin::Builtin::Output(kind) = builtin {
                                output::start(program, ctx, frames, storage, stack, kind, args)?;
                                continue;
                            }
                            if let crate::builtin::Builtin::Format(function) = builtin {
                                format::start(
                                    program, ctx, frames, storage, stack, function, args,
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
                                    frames,
                                    storage,
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
                                frames,
                                storage,
                                function,
                                args,
                                stack.data.len(),
                            )?;
                        }
                        Invocation::Host(host) => {
                            let value = capabilities::registered(
                                ctx,
                                storage,
                                &hosts[host],
                                &args.positional.data,
                                &args.keywords.buffer.data,
                                args.block,
                            )?;
                            value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                        }
                        Invocation::Member(site, mutating) => {
                            dispatch::member(
                                program,
                                ctx,
                                frames,
                                storage,
                                stack,
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
                    let value = capabilities::registered(
                        ctx,
                        storage,
                        &hosts[host],
                        &stack.data[base..],
                        &[],
                        None,
                    )?;
                    stack.data.truncate(base);
                    value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                }
                Op::Method(site, n) => {
                    let base = stack.data.len() - n - 1;
                    let root = std::mem::take(&mut stack.data[base]);
                    if let Some(method) =
                        capabilities::member(ctx, site, &program.members[site.name], &root)?
                    {
                        let value = capabilities::call_on(
                            ctx,
                            storage,
                            &method,
                            Some(&root),
                            &stack.data[base + 1..],
                            &[],
                            None,
                            site.auto,
                        )?;
                        stack.data.truncate(base);
                        value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                        continue;
                    }
                    if matches!(root.0, Kind::Hash(_)) {
                        if let Some(Value(Kind::Function(function))) =
                            members::field(ctx, site, &program.members[site.name], &root)?
                        {
                            if site.auto && site.scope {
                                return Err(function.value_error());
                            }
                            let args = Arguments::from_values(ctx, &stack.data[base + 1..])?;
                            stack.data.truncate(base);
                            requires::invoke(
                                ctx, frames, storage, &function, args, site.auto, base,
                            )?;
                            continue;
                        }
                    }
                    if matches!(root.0, Kind::Namespace(_) | Kind::Instance(_)) {
                        let receiver = &root;
                        let name = &program.members[site.name];
                        match namespaces::member(
                            ctx,
                            storage,
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
                                let args = Arguments::from_values(ctx, &stack.data[base + 1..])?;
                                enter_arguments(
                                    program, ctx, frames, storage, function, args, base,
                                )?;
                                stack.data.truncate(base);
                                continue;
                            }
                            namespaces::Member::Value(value) => {
                                let value = capabilities::field_on(
                                    ctx,
                                    storage,
                                    site,
                                    Some(receiver),
                                    value,
                                    &stack.data[base + 1..],
                                    &[],
                                    None,
                                )?;
                                stack.data.truncate(base);
                                value.finish(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
                                    stack,
                                    ReturnTo::Stack,
                                )?;
                                continue;
                            }
                            namespaces::Member::Helper(module, helper) => {
                                let args = Arguments::from_values(ctx, &stack.data[base + 1..])?;
                                let value = dispatch::helper(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
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
                    crate::exports::check(ctx, &value)?;
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
            if ctx.has_exports
                && matches!(
                    op,
                    Op::Index(_)
                        | Op::Method(..)
                        | Op::Mutate(..)
                        | Op::Invoke(_)
                        | Op::InvokeRoot(_)
                        | Op::Host(..)
                        | Op::Extract(_)
                        | Op::BlockArg(..)
                )
            {
                let frame = &frames.data[current];
                let target = matches!(
                    frame.program.functions[frame.function.unwrap()]
                        .code
                        .get(frame.ip),
                    Some(Op::CallValue)
                );
                if let Some(value) = stack.data.last() {
                    if !target || !matches!(value.0, Kind::Function(_) | Kind::Host(_)) {
                        crate::exports::check(ctx, value)?;
                    }
                }
            }
        }
    }
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
            filename: source.filename.clone(),
            position: source.position(at),
        })
        .collect();
    error.offset = Some(offset as usize);
    error.diagnostic = Some(std::sync::Arc::new(crate::Diagnostic {
        filename: program.source.filename.clone(),
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
    if file_bindings::get(program, ctx, name)?.is_some()
        || requires::root_bound(ctx, storage, name)?
    {
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
        globals::types(ctx, storage, binding, fold)?;
        if let Some(bindings) = &storage.bindings {
            for (key, value) in crate::objects::bindings(ctx, bindings)?.data {
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
            if !type_name_matches(ctx, global.name(), binding, fold)? {
                continue;
            }
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
            if !type_name_matches(ctx, name, binding, fold)? {
                continue;
            }
            let scoped = if program.file {
                file_bindings::get(program, ctx, name)?
            } else {
                requires::get(ctx, storage, name)?
            };
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
    if program.file && program.index != 0 {
        let root = storage.programs.data[0].program.clone();
        return resolve_type(&root, ctx, frames, storage, None, name, enum_only);
    }
    Err(Error::new(ErrorKind::Type, "unknown named type"))
}

fn type_name_matches(
    ctx: &mut CallContext,
    candidate: &str,
    binding: &str,
    fold: bool,
) -> Result<bool> {
    crate::types::binding_name_matches(ctx, candidate.as_bytes(), binding.as_bytes(), fold)
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
    if !type_name_matches(ctx, candidate, binding, fold)? {
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
        Kind::Host(method) => crate::arguments::Target::Capability(method.clone()),
        Kind::Function(function) => crate::arguments::Target::Export(function.clone()),
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
    if !scoped {
        if let Some(value) = globals::get(
            ctx,
            storage,
            file_bindings::declaration_name(program, index),
        )? {
            return Ok(value);
        }
    }
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
    if let Some(value) = requires::get(ctx, storage, program.globals[index].0.name())? {
        return Ok(value);
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
        return recursion_exceeded(ctx);
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
        return recursion_exceeded(ctx);
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
        return recursion_exceeded(ctx);
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
        program: programs::pin(ctx, storage, program.index)?,
        host: false,
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
        return recursion_exceeded(ctx);
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
    for frame in &frames.data[target..] {
        if let ReturnTo::Require(index) = frame.return_to {
            requires::abandon(storage, index);
        }
        programs::defer_release(storage, frame.program.index);
    }
    let frame = &frames.data[target];
    stack.data.truncate(frame.base);
    storage.locals.data.truncate(frame.local_base);
    storage.iterations.data.truncate(frame.iteration_base);
    storage.addresses.data.truncate(frame.address_base);
    storage.bypasses.data.truncate(frame.bypass_base);
    storage.texts.data.truncate(frame.text_base);
    frames.data.truncate(target);
}
