use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    address::{self, Address},
    arguments::{Arguments, Binding, Block},
    budget::Buffer,
    builtin::Global,
    bytecode::{ArgumentOp, Invocation, NO_SLOT, Op, Operator, Receiving, Selection, narrow},
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
mod simple;
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

#[derive(Clone, Copy)]
enum FrameCode {
    Function(u32),
    Iteration,
    PooledIteration,
}

struct Frame {
    program: Arc<Program>,
    host: bool,
    /// Whether the frame's arguments came from the host and are being
    /// checked against the parameter types; the checker proves the
    /// arguments of every other call.
    checked: bool,
    /// For an iterating call's frame, whether the checker proved plain what
    /// it yields to its block, because its receiver and arguments are, and
    /// its result: neither then needs a scan for host methods.
    plain_yields: bool,
    plain_result: bool,
    activation: bool,
    receiver: Option<Value>,
    constructor: bool,
    return_to: ReturnTo,
    function: FrameCode,
    mutating: bool,
    ip: usize,
    iteration_base: u32,
    base: u32,
    local_base: u32,
    address_base: u32,
    bypass_base: u32,
    text_base: u32,
    /// Where the frame's loops start in [`Storage::loops`]; they end where
    /// the next frame's start, or at the end for the executing frame.
    loop_base: u32,
    /// Where the frame's pending calls' arguments start in
    /// [`Storage::arguments`].
    argument_base: u32,
    /// Whether the frame is binding its parameters, through the last of
    /// [`Storage::parameters`].
    binding: bool,
    parent: Option<u32>,
    home: Option<u32>,
    block: Option<Block>,
    /// How many arguments a block frame received; they sit on the operand
    /// stack from `base`, below the block's own values.
    block_args: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Frame>() == 136);

/// A binding that an assignment is filling, which same-name calls in its value skip.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bypass {
    /// A local slot of the assigning frame or one it captures.
    Local(usize),
    /// A required file's scope, which holds its top-level locals, functions and
    /// declarations. The program, function and slot name the assigned local.
    File(usize, usize, usize),
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
    iteration_pool: iteration::Pool,
    globals: Buffer<Option<Value>>,
    ambient_globals: Buffer<(Global, usize)>,
    locals: Buffer<Option<Value>>,
    addresses: Buffer<Address>,
    bypasses: Buffer<Bypass>,
    /// The entry frame's assigned local slots and values, captured as it
    /// returns when the host asked for the run's root bindings.
    root_locals: Option<Buffer<(usize, Value)>>,
    /// Every frame's active loops, innermost last.
    loops: Buffer<LoopState>,
    /// Every frame's pending calls' arguments, innermost last.
    arguments: Buffer<Arguments>,
    /// The parameter bindings of frames still binding their parameters.
    parameters: Buffer<Binding>,
    /// The shared literals ([`Op::Shared`]) this call imported, each
    /// program's from the base its entry records.
    shared: Buffer<Option<Value>>,
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
            iteration_pool: Buffer::empty(),
            globals: Buffer::empty(),
            ambient_globals: Buffer::empty(),
            locals: Buffer::empty(),
            addresses: Buffer::empty(),
            bypasses: Buffer::empty(),
            root_locals: None,
            loops: Buffer::empty(),
            arguments: Buffer::empty(),
            parameters: Buffer::empty(),
            shared: Buffer::empty(),
        };
        // Without enum declarations, arguments have nothing to rebind to.
        let definitions = &code.program.enum_definitions;
        ctx.enum_rebind.definitions =
            (!code.program.file && !definitions.is_empty()).then(|| definitions.clone());
        ctx.enum_rebind.active = true;
        globals::validate(ctx)?;
        crate::declared::check_globals(ctx, &code.declared)?;
        let environment = code
            .program
            .file
            .then(|| crate::objects::environment(ctx))
            .transpose()?;
        let (root, _) = programs::load(ctx, &mut storage, code, environment.as_ref())?;
        let program = &*root;
        capabilities::bind(ctx, &mut storage, &code.declared)?;
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
                None => match self.advance(ctx, floor) {
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
    fn advance(&mut self, ctx: &mut CallContext, floor: usize) -> Result<Event> {
        let program = &*self.root;
        let active = &mut self.active;
        let initializer = &mut self.initializer;
        let pending_entry = &mut self.pending_entry;
        let loader = &self.loader;
        let frames = &mut self.frames;
        let storage = &mut self.storage;
        let stack = &mut self.stack;
        // The executing frame is rechecked only after an instruction changes
        // the frame stack or queues a program activation.
        let mut settled = usize::MAX;
        loop {
            if frames.data.len() != settled || !storage.activations.data.is_empty() {
                settled = usize::MAX;
                if !storage.activations.data.is_empty() {
                    programs::advance(ctx, frames, storage, stack.data.len())?;
                }
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
                        enter_checked(program, ctx, frames, storage, function, input, 0, true)
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
                if frames.data[current].host {
                    return Ok(Event::Host);
                }
                if frames.data[current].function().is_none() {
                    ctx.charge(1)?;
                    let pooled = frames.data[current].pooled_iteration();
                    let waiting = if pooled {
                        storage.iteration_pool.data.last().unwrap().waiting
                    } else {
                        storage.iterations.data[frames.data[current].iteration_base()].waiting()
                    };
                    let returned = if waiting {
                        Some(stack.data.pop().unwrap())
                    } else {
                        None
                    };
                    let progress = if pooled {
                        storage
                            .iteration_pool
                            .data
                            .last_mut()
                            .unwrap()
                            .advance(ctx, returned)?
                    } else {
                        storage.iterations.data[frames.data[current].iteration_base()]
                            .advance(ctx, returned)?
                    };
                    match progress {
                        Progress::Yield(args, count) => {
                            if !frames.data[current].plain_yields {
                                for value in &args[..count] {
                                    crate::exports::check(ctx, value)?;
                                }
                            }
                            let block = frames.data[current].block.unwrap();
                            for value in args.into_iter().take(count) {
                                stack.push(ctx, value)?;
                            }
                            enter_block(ctx, frames, storage, stack, block, count)?;
                        }
                        Progress::Done(mut value) => {
                            let mutation = if pooled {
                                None
                            } else {
                                storage.iterations.data[frames.data[current].iteration_base()]
                                    .take_mutation()
                            };
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
                            let plain = frames.data[current].plain_result;
                            unwind(frames, storage, stack, current);
                            if !plain {
                                crate::exports::check(ctx, &value)?;
                            }
                            stack.push(ctx, value)?;
                        }
                    }
                    continue;
                }
                settled = frames.data.len();
            }
            let current = frames.data.len() - 1;
            let program = &**active;
            let hosts = &program.code.hosts;
            let (outer, rest) = frames.data.split_at_mut(current);
            let frame = &mut rest[0];
            let function = &program.functions[frame.function().unwrap()];
            simple::run(ctx, program, function, outer, frame, storage, stack)?;
            let op = function.code[frame.ip];
            let frame = &mut frames.data[current];
            frame.ip += 1;
            // Returns, and calls without host bindings, need nothing from the
            // prologue below.
            match op {
                Op::Call(callee, count)
                    if ctx.options.globals.is_empty() && ctx.capability_names.data.is_empty() =>
                {
                    ctx.charge(1)?;
                    let base = stack.data.len() - count as usize;
                    enter(
                        program,
                        ctx,
                        frames,
                        storage,
                        callee as usize,
                        &stack.data[base..],
                        base,
                    )?;
                    stack.data.truncate(base);
                    continue;
                }
                Op::CallBlock(callee, count, block)
                    if ctx.options.globals.is_empty() && ctx.capability_names.data.is_empty() =>
                {
                    ctx.charge(1)?;
                    call_block(
                        program,
                        ctx,
                        frames,
                        storage,
                        stack,
                        (callee as usize, count as usize, block as usize),
                    )?;
                    continue;
                }
                Op::Return | Op::Finish => {
                    ctx.charge(1)?;
                    let mut value = stack.data.pop().unwrap();
                    if function.returns_nil && matches!(op, Op::Finish) {
                        value = Value::nil();
                    }
                    if ctx.has_exports && !function.plain_values.contains(frame.ip - 1) {
                        crate::exports::check(ctx, &value)?;
                    }
                    let target = if matches!(op, Op::Return)
                        && frame.parent().is_some()
                        && !function.initializer
                    {
                        let Some(home) = frame.home() else {
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
                    // A plain call returning its value to the caller's stack
                    // meets no handler, return type, constructor, initializer,
                    // releasable program or run boundary, so it unwinds here.
                    if target == current
                        && current > floor
                        && matches!(frame.return_to, ReturnTo::Stack)
                        && !frame.constructor
                        && function.return_check.is_none()
                        && !function.initializer
                        && !handlers::guards(storage, current)
                        && !storage.releasing
                        && !(program.index != 0 && program.file && program.environment.is_some())
                    {
                        unwind(frames, storage, stack, current);
                        stack.push(ctx, value)?;
                        continue;
                    }
                    return Ok(Event::Control(Control::Return {
                        target,
                        value,
                        normalize: true,
                    }));
                }
                _ => (),
            }
            let local_base = frame.local_base();
            let namespace = function.namespace;
            let caller_instance = matches!(frame.receiver, Some(Value(Kind::Instance(_))));
            ctx.charge(1)?;
            let mut slot = |slot: usize, skip: bool| -> Result<usize> {
                // A bound local of the executing frame always resolves to itself.
                if !skip && storage.locals.data[local_base + slot].is_some() {
                    return Ok(local_base + slot);
                }
                resolve_slot(ctx, frames, storage, current, slot, skip)
            };
            // Local reads, writes and declarations remember the slot they name
            // and the one it resolved to, for the file and root binding checks.
            let mut local = None;
            let mut bound = |relative: u32| {
                let absolute = slot(relative as usize, false)?;
                local = Some((relative as usize, absolute));
                Ok::<_, Error>(narrow(absolute))
            };
            // A name an assignment skips resolves to no slot.
            let resolved = |absolute: usize| {
                if absolute == usize::MAX {
                    NO_SLOT
                } else {
                    narrow(absolute)
                }
            };
            let mut op = match op {
                Op::Load(n) => Op::Load(bound(n)?),
                Op::LoadOptional(n, name, receiving) => {
                    Op::LoadOptional(bound(n)?, name, receiving)
                }
                Op::ReceiverBound(n, next) => Op::ReceiverBound(bound(n)?, next),
                Op::Declare(n) => Op::Declare(bound(n)?),
                Op::Store(n) => Op::Store(bound(n)?),
                Op::AddStore(n) => Op::AddStore(bound(n)?),
                Op::AddressLocal(n) => Op::AddressLocal(bound(n)?),
                Op::AddressBound(n, next) => Op::AddressBound(bound(n)?, next),
                // A required file's own bindings live in its scope rather than in slots.
                Op::Bypass(n) if program.file => Op::Bypass(bound(n)?),
                Op::Bypass(n) => Op::Bypass(narrow(slot(n as usize, false)?)),
                Op::ResolveCall(n, name) if n != NO_SLOT => {
                    Op::ResolveCall(resolved(slot(n as usize, true)?), name)
                }
                Op::CallName(n, name) if n != NO_SLOT => {
                    Op::CallName(resolved(slot(n as usize, false)?), name)
                }
                op => op,
            };
            if let Op::Call(_, count)
            | Op::CallBlock(_, count, _)
            | Op::Host(_, count)
            | Op::NonCallable(count) = op
            {
                if !ctx.options.globals.is_empty() || !ctx.capability_names.data.is_empty() {
                    let target = storage.arguments.data.pop().unwrap().target;
                    if let Some(target) = target {
                        let base = stack.data.len() - count as usize;
                        let mut args = Arguments::from_values(ctx, &stack.data[base..])?;
                        args.target = Some(target);
                        if let Op::CallBlock(_, _, function) = op {
                            args.block = Some(Block::new(function as usize, current));
                        }
                        stack.data.truncate(base);
                        storage.arguments.push(ctx, args)?;
                        op = Op::Invoke(Invocation::Resolved);
                    }
                }
            }
            let (file_local, root_local) = match local {
                Some((relative, absolute)) if program.file => (
                    file_bindings::local(
                        program, ctx, frames, storage, current, relative, absolute,
                    )?
                    .then_some(function.local_names[relative].as_str()),
                    None,
                ),
                Some((relative, absolute)) => (
                    None,
                    requires::local(ctx, frames, storage, current, relative, absolute)?
                        .then_some(function.local_names[relative].as_str()),
                ),
                None => (None, None),
            };
            let frame = &mut frames.data[current];
            match op {
                Op::TryBegin(spec) => handlers::begin(ctx, frames, storage, stack, spec as usize)?,
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
                    let named = named.map(|index| program.raises[index as usize]);
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
                    storage.arguments.push(ctx, args)?;
                    if class.is_some() {
                        frame.ip = target as usize;
                    }
                }
                Op::RaiseValue => {
                    let args = storage.arguments.data.last_mut().unwrap();
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
                    let target = storage.arguments.data.pop().unwrap().target.unwrap();
                    let crate::arguments::Target::Raise(class, value) = target else {
                        unreachable!()
                    };
                    let class = class.or_else(|| handlers::class(&value));
                    return Err(handlers::raise(ctx, class, message, true)?);
                }
                Op::UnboundClass(name) => {
                    return Err(Error::new(
                        ErrorKind::Name,
                        format!("class {} is not bound", program.members[name as usize]),
                    ));
                }
                Op::Unsupported => {
                    return Err(Error::new(ErrorKind::Name, "unsupported statement"));
                }
                Op::BindIvar(name, local) => {
                    let Some(Value(Kind::Instance(instance))) = frame.receiver.as_ref() else {
                        return Err(Error::new(
                            ErrorKind::Name,
                            "no instance context for ivar parameter",
                        ));
                    };
                    let instance = instance.clone();
                    let slot = frame.local_base() + local as usize;
                    let value = storage.locals.data[slot].as_ref().unwrap().clone();
                    let value = if function.proven_ivars.contains(frame.ip - 1) {
                        value
                    } else {
                        normalize_ivar(
                            ctx,
                            frames,
                            storage,
                            &instance,
                            &program.members[name as usize],
                            value,
                        )?
                    };
                    set_ivar(
                        ctx,
                        storage,
                        &instance,
                        &program.members[name as usize],
                        &value,
                    )?;
                    storage.locals.data[slot] = Some(value);
                }
                Op::InstanceField(slot) => {
                    let Some(Value(Kind::Instance(instance))) = &frame.receiver else {
                        return Err(instance_context_error());
                    };
                    let value = crate::objects::get_slot(ctx, instance, slot as usize)?;
                    stack.push(ctx, value)?;
                }
                Op::InstanceAddress(slot) => {
                    let Some(Value(Kind::Instance(instance))) = &frame.receiver else {
                        return Err(instance_context_error());
                    };
                    let mut address = crate::objects::address_slot(ctx, instance, slot as usize)?;
                    address.proven = function.proven_ivars.contains(frame.ip - 1);
                    storage.addresses.push(ctx, address)?;
                }
                Op::InstanceStore(slot) | Op::BindField(slot, _) => {
                    let Some(Value(Kind::Instance(instance))) = frame.receiver.clone() else {
                        return Err(instance_context_error());
                    };
                    let local = if let Op::BindField(_, local) = op {
                        Some(frame.local_base() + local as usize)
                    } else {
                        None
                    };
                    let value = local
                        .map_or_else(
                            || stack.data.last().unwrap(),
                            |local| storage.locals.data[local].as_ref().unwrap(),
                        )
                        .clone();
                    let value = if function.proven_ivars.contains(frame.ip - 1) {
                        value
                    } else {
                        let name = &instance.class().field_layout().unwrap()[slot as usize];
                        normalize_ivar(ctx, frames, storage, &instance, name, value)?
                    };
                    address::refresh(
                        ctx,
                        address::Root::Object(instance.clone(), slot as usize),
                        &value,
                        &mut storage.addresses.data,
                        &[],
                    )?;
                    crate::objects::set_slot(ctx, &instance, slot as usize, value.clone())?;
                    if let Some(local) = local {
                        storage.locals.data[local] = Some(value);
                    } else {
                        *stack.data.last_mut().unwrap() = value;
                    }
                }
                Op::InitNamespace(module) => {
                    let state = namespaces::state(program, ctx, storage, module as usize)?;
                    if !storage.namespaces.data[state].initialized {
                        let body = program.namespaces[module as usize].body.unwrap();
                        enter_arguments(
                            program,
                            ctx,
                            frames,
                            storage,
                            body,
                            Arguments::empty(),
                            stack.data.len(),
                        )?;
                        frames.data.last_mut().unwrap().parent = Some(narrow(current));
                    }
                }
                Op::AmbientValue(name, next) | Op::AmbientAddress(name, next) => {
                    let name = &program.members[name as usize];
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
                            frames.data[current].ip = next as usize;
                        }
                    }
                }
                Op::ImplicitAddress(name, next) => {
                    let name = &program.members[name as usize];
                    if file_bindings::root_binding(program, ctx, storage, name)?.is_none()
                        && global_index(program, name).is_none()
                    {
                        let receiver = frame.receiver.clone();
                        if let Some(address) = namespaces::implicit_address(
                            program,
                            ctx,
                            storage,
                            namespace,
                            receiver.as_ref(),
                            name,
                        )? {
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next as usize;
                        }
                    }
                }
                Op::FileValue(name, next, receiving) => {
                    let name = &program.members[name as usize];
                    if let Some(mut value) = file_bindings::get(program, ctx, name)? {
                        if let Kind::Offset(offset) = &value.0 {
                            return Err(offset.value_error());
                        }
                        if let Kind::Builtin(builtin) = value.0 {
                            if receiving.runs_dynamic() {
                                value = builtin.read(ctx)?;
                            }
                        }
                        stack.push(ctx, value)?;
                        frame.ip = next as usize;
                    } else if !program.names.contains_key(name)
                        && !program.declaration_names.contains_key(name)
                        && !program.hosts.iter().any(|host| host == name)
                    {
                        if let Some(binding) =
                            file_bindings::root_binding(program, ctx, storage, name)?
                        {
                            frames.data[current].ip = next as usize;
                            file_bindings::receive_root(
                                program, ctx, frames, storage, stack, binding, receiving,
                            )?;
                        }
                    }
                }
                Op::FileAddress(name, next) => {
                    let name = &program.members[name as usize];
                    if file_bindings::unshadowed(program, ctx, frames, storage, current, name)? {
                        let address = if file_bindings::get(program, ctx, name)?.is_some() {
                            Some(file_bindings::address(program, ctx, name)?)
                        } else {
                            requires::address(program, ctx, frames, storage, current, name)?
                        };
                        if let Some(address) = address {
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next as usize;
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
                        &program.members[name as usize],
                    )? {
                        storage.addresses.push(ctx, address)?;
                        frames.data[current].ip = next as usize;
                    }
                }
                Op::PrepareMember(site, mutating) => {
                    if mutating {
                        storage.addresses.data.last().unwrap().check_present(ctx)?;
                    }
                    let receiver = if mutating {
                        &storage.addresses.data.last().unwrap().value
                    } else {
                        stack.data.last().unwrap()
                    };
                    if matches!(receiver.0, Kind::Hash(_)) {
                        let field = members::prepare(
                            ctx,
                            site,
                            &program.members[site.name as usize],
                            receiver,
                        )?;
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
                    let value = if let Some(value) = &frame.receiver {
                        value.clone()
                    } else {
                        namespaces::value(program, ctx, storage, module as usize)?
                    };
                    stack.push(ctx, value)?;
                }
                Op::NamespaceConstant(name, next) => {
                    if let Some(value) = namespaces::field(
                        program,
                        ctx,
                        storage,
                        namespace.unwrap(),
                        &program.members[name as usize],
                    )? {
                        stack.push(ctx, value)?;
                        frames.data[current].ip = next as usize;
                    }
                }
                Op::NamespaceConstantAddress(name, next) => {
                    let module = namespace.unwrap();
                    let name = &program.members[name as usize];
                    if namespaces::field(program, ctx, storage, module, name)?.is_some() {
                        let address =
                            namespaces::address(program, ctx, storage, module, name, false)?;
                        storage.addresses.push(ctx, address)?;
                        frames.data[current].ip = next as usize;
                    }
                }
                Op::NamespaceVariable(name, optional) => {
                    let raw = &program.members[name as usize];
                    if raw.starts_with('@') && !raw.starts_with("@@") {
                        let Some(Value(Kind::Instance(instance))) = &frame.receiver else {
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
                    let (module, name) = namespaces::variable_name(
                        namespace,
                        &program.members[name as usize],
                        false,
                    )?;
                    let value = namespaces::field(program, ctx, storage, module, name)?;
                    let value = value.ok_or_else(|| {
                        if optional {
                            Error::new(
                                ErrorKind::Runtime,
                                format!("class variable @@{name} is not initialized"),
                            )
                        } else {
                            Error::new(ErrorKind::Name, "undefined class variable")
                        }
                    })?;
                    stack.push(ctx, value)?;
                }
                Op::NamespaceAddress(name, optional) => {
                    let raw = &program.members[name as usize];
                    if raw.starts_with('@') && !raw.starts_with("@@") {
                        let Some(Value(Kind::Instance(instance))) = &frame.receiver else {
                            return Err(Error::new(
                                ErrorKind::Name,
                                "no instance context for ivar",
                            ));
                        };
                        let mut address = crate::objects::address(ctx, instance, &raw[1..])?;
                        address.proven = function.proven_ivars.contains(frame.ip - 1);
                        storage.addresses.push(ctx, address)?;
                        continue;
                    }
                    let (module, name) = namespaces::variable_name(
                        namespace,
                        &program.members[name as usize],
                        false,
                    )?;
                    if raw.starts_with("@@")
                        && namespaces::field(program, ctx, storage, module, name)?.is_none()
                    {
                        return Err(Error::new(
                            ErrorKind::Runtime,
                            format!("class variable @@{name} is not initialized"),
                        ));
                    }
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
                    let raw = &program.members[name as usize];
                    if raw.starts_with('@') && !raw.starts_with("@@") {
                        let self_value = frame.receiver.clone();
                        let Some(Value(Kind::Instance(instance))) = &self_value else {
                            return Err(Error::new(
                                ErrorKind::Name,
                                "no instance context for ivar",
                            ));
                        };
                        let value = stack.data.last().unwrap().clone();
                        let value = if function.proven_ivars.contains(frame.ip - 1) {
                            value
                        } else {
                            normalize_ivar(ctx, frames, storage, instance, &raw[1..], value)?
                        };
                        set_ivar(ctx, storage, instance, &raw[1..], &value)?;
                        *stack.data.last_mut().unwrap() = value;
                        continue;
                    }
                    let (module, name) = namespaces::variable_name(
                        namespace,
                        &program.members[name as usize],
                        true,
                    )?;
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
                            file_bindings::declaration_name(program, index as usize),
                        )?
                    {
                        requires::set(
                            ctx,
                            storage,
                            file_bindings::declaration_name(program, index as usize),
                            stack.data.last().unwrap(),
                        )?;
                        continue;
                    }
                    if file_bindings::environment(program).is_some() {
                        file_bindings::set(
                            program,
                            ctx,
                            storage,
                            file_bindings::declaration_name(program, index as usize),
                            stack.data.last().unwrap(),
                        )?;
                        continue;
                    }
                    let mut found = false;
                    for (key, value) in &mut storage.declarations.data {
                        ctx.charge(1)?;
                        if *key == (program.index, index as usize) {
                            *value = stack.data.last().unwrap().clone();
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        storage.declarations.push(
                            ctx,
                            (
                                (program.index, index as usize),
                                stack.data.last().unwrap().clone(),
                            ),
                        )?;
                    }
                }
                Op::Integer(n, radix) => {
                    let text = program.constants[n as usize].as_bytes().unwrap();
                    let value = crate::integer::parse(ctx, text, radix)?;
                    stack.push(ctx, value)?;
                }
                Op::Regex(n, flags) => {
                    let v = crate::regex::value::Regex::compile(
                        ctx,
                        program.constants[n as usize].clone(),
                        flags,
                        "regex literal",
                    )?;
                    stack.push(ctx, v)?;
                }
                Op::Constant(n) => {
                    let v = ctx.import(&program.constants[n as usize])?;
                    stack.push(ctx, v)?;
                }
                Op::Shared(slot) => {
                    let v = shared(ctx, program, storage, slot as usize)?;
                    stack.push(ctx, v)?;
                }
                Op::TypeShadowed(guard, next) => {
                    let self_value = frame.receiver.clone();
                    for name in &program.type_guards[guard as usize] {
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
                            frames.data[current].ip = next as usize;
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
                        storage.locals.data[n as usize].clone().unwrap_or_default()
                    };
                    if let Kind::Offset(offset) = &v.0 {
                        return Err(offset.value_error());
                    }
                    if let Kind::Builtin(builtin) = v.0 {
                        v = builtin.read(ctx)?;
                    }
                    stack.push(ctx, v)?;
                }
                Op::LoadOptional(slot, name, receiving) => {
                    let scoped = if let Some(name) = file_local {
                        file_bindings::get(program, ctx, name)?
                    } else if let Some(name) = root_local {
                        requires::get(ctx, storage, name)?
                    } else {
                        None
                    };
                    if let Some(value) = scoped
                        .as_ref()
                        .or(storage.locals.data[slot as usize].as_ref())
                    {
                        if let Kind::Offset(offset) = &value.0 {
                            return Err(offset.value_error());
                        }
                        let value = match value.0 {
                            Kind::Builtin(builtin) if receiving.runs_dynamic() => {
                                builtin.read(ctx)?
                            }
                            _ => value.clone(),
                        };
                        stack.push(ctx, value)?;
                    } else if let Some(value) = namespaces::constant(
                        program,
                        ctx,
                        storage,
                        namespace,
                        &program.members[name as usize],
                    )? {
                        stack.push(ctx, value)?;
                    } else if let Some(slot) = namespaces::ambient_slot(
                        ctx,
                        frames,
                        storage,
                        current,
                        &program.members[name as usize],
                    )? {
                        stack.push(ctx, storage.locals.data[slot].as_ref().unwrap().clone())?;
                    } else if let Some(&index) = program
                        .declaration_names
                        .get(&program.members[name as usize])
                    {
                        let value = declaration_value(program, ctx, storage, index)?;
                        stack.push(ctx, value)?;
                    } else if let Some(&function) =
                        program.names.get(&program.members[name as usize])
                    {
                        receive_function(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack.data.len(),
                            function,
                            receiving,
                        )?;
                    } else if let Some(host) = program
                        .hosts
                        .iter()
                        .position(|h| h == &program.members[name as usize])
                    {
                        if !receiving.runs_static(None) {
                            return Err(receive_host(program, &program.hosts[host], receiving));
                        }
                        let value =
                            capabilities::registered(ctx, storage, &hosts[host], &[], &[], None)?;
                        value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                    } else if let Some(binding) = file_bindings::root_binding(
                        program,
                        ctx,
                        storage,
                        &program.members[name as usize],
                    )? {
                        file_bindings::receive_root(
                            program, ctx, frames, storage, stack, binding, receiving,
                        )?;
                    } else if let Some(global) =
                        global_index(program, &program.members[name as usize])
                    {
                        let mut value = global_value(program, ctx, storage, global)?;
                        if let Kind::Offset(offset) = &value.0 {
                            return Err(offset.value_error());
                        }
                        if let Kind::Builtin(builtin) = value.0 {
                            if receiving.runs_static(None) {
                                value = builtin.read(ctx)?;
                            }
                        }
                        stack.push(ctx, value)?;
                    } else {
                        implicit_read(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack,
                            current,
                            namespace,
                            name as usize,
                            receiving,
                        )?;
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
                    if let Some(value) = scoped
                        .as_ref()
                        .or(storage.locals.data[slot as usize].as_ref())
                    {
                        stack.push(ctx, value.clone())?;
                        frame.ip = next as usize;
                    }
                }
                Op::Unbound(name, receiving) => {
                    if let Some(binding) = file_bindings::root_binding(
                        program,
                        ctx,
                        storage,
                        &program.members[name as usize],
                    )? {
                        file_bindings::receive_root(
                            program, ctx, frames, storage, stack, binding, receiving,
                        )?;
                        continue;
                    }
                    implicit_read(
                        program,
                        ctx,
                        frames,
                        storage,
                        stack,
                        current,
                        namespace,
                        name as usize,
                        receiving,
                    )?;
                }
                Op::Declaration(index) => {
                    let value = declaration_value(program, ctx, storage, index as usize)?;
                    stack.push(ctx, value)?;
                }
                Op::Global(index) => {
                    if program.file
                        && file_bindings::get(
                            program,
                            ctx,
                            program.globals[index as usize].0.name(),
                        )?
                        .is_none()
                    {
                        if let Some(binding) = file_bindings::root_binding(
                            program,
                            ctx,
                            storage,
                            program.globals[index as usize].0.name(),
                        )? {
                            file_bindings::read_root(ctx, frames, storage, stack, binding)?;
                            continue;
                        }
                    }
                    let mut value = global_value(program, ctx, storage, index as usize)?;
                    if let Kind::Offset(offset) = &value.0 {
                        return Err(offset.value_error());
                    }
                    if let Kind::Builtin(builtin) = value.0 {
                        value = builtin.read(ctx)?;
                    }
                    stack.push(ctx, value)?;
                }
                Op::GlobalReceiver(index, receiving) => {
                    if program.file
                        && file_bindings::get(
                            program,
                            ctx,
                            program.globals[index as usize].0.name(),
                        )?
                        .is_none()
                    {
                        if let Some(binding) = file_bindings::root_binding(
                            program,
                            ctx,
                            storage,
                            program.globals[index as usize].0.name(),
                        )? {
                            file_bindings::receive_root(
                                program, ctx, frames, storage, stack, binding, receiving,
                            )?;
                            continue;
                        }
                    }
                    let mut value = global_value(program, ctx, storage, index as usize)?;
                    if let (Kind::Builtin(current), Kind::Builtin(original)) =
                        (&value.0, &program.globals[index as usize].1.0)
                    {
                        if current == original && receiving.runs_static(None) {
                            value = current.read(ctx)?;
                        }
                    }
                    stack.push(ctx, value)?;
                }
                Op::StoreGlobal(index) => {
                    if !program.file
                        && requires::contains(
                            ctx,
                            storage,
                            program.globals[index as usize].0.name(),
                        )?
                    {
                        requires::set(
                            ctx,
                            storage,
                            program.globals[index as usize].0.name(),
                            stack.data.last().unwrap(),
                        )?;
                        continue;
                    }
                    if file_bindings::environment(program).is_some() {
                        file_bindings::set(
                            program,
                            ctx,
                            storage,
                            program.globals[index as usize].0.name(),
                            stack.data.last().unwrap(),
                        )?;
                        continue;
                    }
                    let index = program.global_base + index as usize;
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
                    let value = global_value(program, ctx, storage, index as usize)?;
                    let mut arguments = Arguments::empty();
                    arguments.target = Some(value_invocation(&value));
                    storage.arguments.push(ctx, arguments)?;
                }
                Op::AddressGlobal(index) => {
                    if program.file
                        && file_bindings::get(
                            program,
                            ctx,
                            program.globals[index as usize].0.name(),
                        )?
                        .is_none()
                    {
                        if let Some(binding) = file_bindings::root_binding(
                            program,
                            ctx,
                            storage,
                            program.globals[index as usize].0.name(),
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
                        file_bindings::global_address(program, ctx, storage, index as usize, true)?;
                    storage.addresses.push(ctx, address)?;
                }
                Op::NonCallable(_) => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "attempted to call non-callable value",
                    ));
                }
                Op::Bind(param, next) => {
                    if let Some(value) = storage
                        .parameters
                        .data
                        .last()
                        .unwrap()
                        .value(ctx, param as usize)?
                    {
                        let param =
                            &program.functions[frame.function().unwrap()].params[param as usize];
                        let slot = frame.local_base() + param.slot;
                        let ty = param
                            .ty
                            .filter(|&ty| frame.checked || program.types[ty].unproven());
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
                        frames.data[current].ip = next as usize;
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
                            ty as usize,
                            crate::types::Context::Argument(
                                program.constants[label as usize].as_bytes().unwrap(),
                            ),
                        ),
                        value,
                    )?;
                    stack.push(ctx, value)?;
                }
                Op::Check(ty, subject) => {
                    let value = stack.data.pop().unwrap();
                    let value = normalize_type(
                        program,
                        ctx,
                        frames,
                        storage,
                        current,
                        (
                            ty as usize,
                            crate::types::Context::Subject(
                                program.constants[subject as usize].as_bytes().unwrap(),
                            ),
                        ),
                        value,
                    )?;
                    stack.push(ctx, value)?;
                }
                Op::BindEnd => {
                    frame.binding = false;
                    storage.parameters.data.pop();
                }
                Op::Declare(slot) => {
                    if let Some(name) = file_local {
                        file_bindings::declare(program, ctx, storage, name)?;
                    } else if root_local.is_none() {
                        storage.locals.data[slot as usize].get_or_insert_with(Value::nil);
                    }
                }
                Op::Bypass(slot) => {
                    let bypass = match (file_local, local) {
                        (Some(_), Some((relative, _))) => {
                            Bypass::File(program.index, frame.function().unwrap(), relative)
                        }
                        _ => Bypass::Local(slot as usize),
                    };
                    storage.bypasses.push(ctx, bypass)?;
                }
                Op::BypassEnd(n) => {
                    storage
                        .bypasses
                        .data
                        .truncate(storage.bypasses.data.len() - n as usize);
                }
                Op::Shadow(slot) => {
                    storage.locals.data[frame.local_base() + slot as usize] = Some(Value::nil())
                }
                Op::BlockArg(index, autosplat) => {
                    let value = block_arg(frame, stack, index as usize, autosplat).cloned();
                    stack.push(ctx, value.unwrap_or_default())?;
                }
                Op::Attach(function) => {
                    storage.arguments.data.last_mut().unwrap().block =
                        Some(Block::new(function as usize, current));
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
                    enter_block(ctx, frames, storage, stack, block, n as usize)?;
                }
                Op::Store(n) => {
                    let value = stack.data.last().unwrap();
                    if let Some(name) = file_local {
                        file_bindings::set(program, ctx, storage, name, value)?;
                    } else if let Some(name) = root_local {
                        requires::set(ctx, storage, name, value)?;
                    } else {
                        address::refresh(ctx, n as usize, value, &mut storage.addresses.data, &[])?;
                        storage.locals.data[n as usize] = Some(value.clone());
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
                    let op = op.name();
                    let value = stack.data.pop().unwrap();
                    let result = ops::unary(ctx, op, value)?;
                    stack.push(ctx, result)?;
                }
                Op::Binary(op) => {
                    let b = stack.data.pop().unwrap();
                    let a = stack.data.pop().unwrap();
                    if let Some(value) = ops::immediate(ctx, op, &a, &b)? {
                        stack.push(ctx, value)?;
                        continue;
                    }
                    let op = op.name();
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
                    // An immediate sum cannot fail part way, so the slot is written once.
                    if root_local.is_none() {
                        if let Some(value) = ops::immediate(ctx, Operator::Add, &a, &b)? {
                            if !storage.addresses.data.is_empty() {
                                address::refresh(
                                    ctx,
                                    n as usize,
                                    &value,
                                    &mut storage.addresses.data,
                                    &[],
                                )?;
                            }
                            storage.locals.data[n as usize] = Some(value.clone());
                            stack.push(ctx, value)?;
                            continue;
                        }
                    }
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
                            ReturnTo::Local(n as usize)
                        };
                        continue;
                    }
                    storage.locals.data[n as usize] = None;
                    let value = ops::binary(ctx, "+", a, b)?;
                    if let Some(name) = root_local {
                        requires::set(ctx, storage, name, &value)?;
                    } else {
                        address::refresh(
                            ctx,
                            n as usize,
                            &value,
                            &mut storage.addresses.data,
                            &[],
                        )?;
                        storage.locals.data[n as usize] = Some(value.clone());
                    }
                    stack.push(ctx, value)?;
                }
                Op::Shovel(site) => {
                    let value = stack.data.pop().unwrap();
                    let address = storage.addresses.data.pop().unwrap();
                    address.check_present(ctx)?;
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
                        |ctx, receiver| {
                            let args = std::slice::from_ref(&value);
                            match members::direct::update(ctx, site.method, "push", receiver, args)
                            {
                                Ok(result) => result,
                                Err(receiver) => members::call(ctx, site, "push", receiver, args),
                            }
                        },
                    )?;
                    stack.push(ctx, result)?;
                }
                Op::Array(n) => {
                    let base = stack.data.len() - n as usize;
                    let mut values = Buffer::with_capacity(ctx, n as usize)?;
                    for value in stack.data.drain(base..) {
                        ctx.charge(1)?;
                        values.data.push(value);
                    }
                    let value = Value::from_array(ctx, values)?;
                    stack.push(ctx, value)?;
                }
                Op::Hash(n) => {
                    let base = stack.data.len() - (n * 2) as usize;
                    let mut values = Hash::empty();
                    values.buffer.ensure(ctx, n as usize)?;
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
                Op::Index(n) | Op::IndexLiteral(n) => {
                    let n = if matches!(op, Op::IndexLiteral(_)) {
                        let key = shared(ctx, program, storage, n as usize)?;
                        stack.push(ctx, key)?;
                        1
                    } else {
                        n
                    };
                    let base = stack.data.len() - n as usize - 1;
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
                        ops::index_many(ctx, root, args)?
                    };
                    if matches!(value.0, Kind::Host(_))
                        && matches!(root.0, Kind::Hash(_))
                        && matches!(
                            program.functions[frame.function().unwrap()]
                                .code
                                .get(frame.ip),
                            Some(Op::CallValue)
                        )
                    {
                        // `receiver[:name](...)` selects its callee here, before the
                        // arguments run; CallValue keeps the root only for host methods.
                        let receiver = root.clone();
                        if storage.arguments.data.len() > frame.argument_base() {
                            storage.arguments.data.last_mut().unwrap().receiver = Some(receiver);
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
                        Address::new(
                            Some(n as usize),
                            storage.locals.data[n as usize].clone().unwrap_or_default(),
                        )
                    };
                    storage.addresses.push(ctx, address)?;
                }
                Op::AddressBound(slot, next) => {
                    if let Some(name) = root_local {
                        if let Some(address) =
                            requires::address(program, ctx, frames, storage, current, name)?
                        {
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next as usize;
                            continue;
                        }
                    }
                    if let Some(name) =
                        file_local.filter(|_| file_bindings::environment(program).is_some())
                    {
                        if file_bindings::get(program, ctx, name)?.is_some() {
                            let address = file_bindings::address(program, ctx, name)?;
                            storage.addresses.push(ctx, address)?;
                            frames.data[current].ip = next as usize;
                        }
                    } else if let Some(value) = &storage.locals.data[slot as usize] {
                        storage
                            .addresses
                            .push(ctx, Address::new(Some(slot as usize), value.clone()))?;
                        frames.data[current].ip = next as usize;
                    }
                }
                Op::AddressValue => {
                    let value = stack.data.pop().unwrap();
                    storage.addresses.push(ctx, Address::new(None, value))?;
                }
                Op::AddressIndex(n) => {
                    let base = stack.data.len() - n as usize;
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
                    let name = &program.members[site.name as usize];
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
                    let name = &program.members[site.name as usize];
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
                            namespaces::Member::IsType(module) => {
                                let value = dispatch::type_predicate(
                                    program,
                                    ctx,
                                    frames,
                                    storage,
                                    &module,
                                    &Arguments::empty(),
                                )?;
                                stack.push(ctx, value)?;
                                continue;
                            }
                            namespaces::Member::Missing => {
                                namespaces::fallback(storage, &receiver, name, false)?
                            }
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
                            if members::callable(value) {
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
                    address.check_present(ctx)?;
                    let name = &program.members[site.name as usize];
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
                                namespaces::Member::IsType(module) => {
                                    let value = dispatch::type_predicate(
                                        program,
                                        ctx,
                                        frames,
                                        storage,
                                        &module,
                                        &Arguments::empty(),
                                    )?;
                                    stack.push(ctx, value)?;
                                    continue;
                                }
                                namespaces::Member::Missing => {
                                    namespaces::fallback(storage, &receiver, name, false)?
                                }
                            }
                        }
                        let address = storage.addresses.data.last().unwrap();
                        let (_, value) =
                            members::call(ctx, site, name, address.value.clone(), &[])?;
                        stack.push(ctx, value)?;
                    }
                }
                Op::AddressTarget(n, read) => {
                    let base = stack.data.len() - n as usize;
                    let address = storage.addresses.data.last_mut().unwrap();
                    address.check_present(ctx)?;
                    address.selectors.ensure(ctx, n as usize)?;
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
                    // Capability objects expose declared data through dot access;
                    // ordinary hashes still require an index.
                    if address.member_target
                        && !matches!(&address.value.0, Kind::Hash(hash) if hash.object)
                    {
                        return Err(Error::new(
                            ErrorKind::Type,
                            format!("cannot assign to {}", address.value.type_name()),
                        ));
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
                            &program.members[site.name as usize],
                            &address.value,
                        )?
                        .map(|method| crate::capability::SelectedMethod {
                            method,
                            receiver: address.value.clone(),
                        })
                    };
                    if let Some(method) = method {
                        let base = stack.data.len() - n as usize;
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
                            &program.members[site.name as usize],
                            &address.value,
                        )?
                    };
                    if let Some(function) = function {
                        if site.auto && site.scope {
                            return Err(function.value_error());
                        }
                        let base = stack.data.len() - n as usize;
                        let args = Arguments::from_values(ctx, &stack.data[base..])?;
                        stack.data.truncate(base);
                        storage.addresses.data.pop();
                        requires::invoke(ctx, frames, storage, &function, args, site.auto, base)?;
                        continue;
                    }
                    let base = stack.data.len() - n as usize;
                    let address = storage.addresses.data.pop().unwrap();
                    if matches!(address.value.0, Kind::Namespace(_) | Kind::Instance(_)) {
                        let receiver = &address.value;
                        let name = &program.members[site.name as usize];
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
                            namespaces::Member::IsType(module) => {
                                let args = Arguments::from_values(ctx, &stack.data[base..])?;
                                let value = dispatch::type_predicate(
                                    program, ctx, frames, storage, &module, &args,
                                )?;
                                stack.data.truncate(base);
                                stack.push(ctx, value)?;
                                continue;
                            }
                            namespaces::Member::Missing => {
                                namespaces::fallback(storage, receiver, name, false)?
                            }
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
                            let name = &program.members[site.name as usize];
                            let args = &stack.data[base..];
                            match members::direct::update(ctx, site.method, name, receiver, args) {
                                Ok(result) => result,
                                Err(receiver) => members::call(ctx, site, name, receiver, args),
                            }
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
                    let value = match program.selections[selection as usize] {
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
                                    format!("cannot iterate over {}", source.type_name()),
                                ));
                            }
                        }
                    } else {
                        0
                    };
                    storage.loops.push(
                        ctx,
                        LoopState {
                            base: stack.data.len(),
                            address_base: storage.addresses.data.len(),
                            bypass_base: storage.bypasses.data.len(),
                            argument_base: storage.arguments.data.len(),
                            text_base: storage.texts.data.len(),
                            next: next as usize,
                            end: end as usize,
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
                        frame.ip = storage.loops.data.last().unwrap().end;
                    }
                }
                Op::IterNext => {
                    let plain = !ctx.has_exports || function.plain_values.contains(frame.ip - 1);
                    let state = storage.loops.data.last_mut().unwrap();
                    if let Some(value) = state.next_value(ctx)? {
                        if !plain {
                            crate::exports::check(ctx, &value)?;
                        }
                        stack.push(ctx, value)?;
                    } else {
                        frame.ip = state.end;
                    }
                }
                Op::LoopBody => {
                    let state = storage.loops.data.last_mut().unwrap();
                    state.last = stack.data.pop().unwrap();
                    stack.data.truncate(state.base);
                    storage.addresses.data.truncate(state.address_base);
                    storage.bypasses.data.truncate(state.bypass_base);
                    storage.texts.data.truncate(state.text_base);
                    storage.arguments.data.truncate(state.argument_base);
                    frame.ip = state.next;
                }
                Op::LoopEnd => {
                    let state = storage.loops.data.pop().unwrap();
                    stack.data.truncate(state.base);
                    storage.addresses.data.truncate(state.address_base);
                    storage.bypasses.data.truncate(state.bypass_base);
                    storage.texts.data.truncate(state.text_base);
                    storage.arguments.data.truncate(state.argument_base);
                    stack.push(ctx, state.result())?;
                }
                Op::LoopGuard(breaking) => handlers::guard_loop(ctx, frames, storage, breaking)?,
                Op::Break(has_value) | Op::Next(has_value) => {
                    let value = has_value.then(|| stack.data.pop().unwrap());
                    let control =
                        handlers::loop_control(frames, storage, matches!(op, Op::Break(_)), value)?;
                    return Ok(Event::Control(control));
                }
                Op::Call(function, n) => {
                    let base = stack.data.len() - n as usize;
                    enter(
                        program,
                        ctx,
                        frames,
                        storage,
                        function as usize,
                        &stack.data[base..],
                        base,
                    )?;
                    stack.data.truncate(base);
                }
                Op::CallBlock(callee, count, block) => {
                    call_block(
                        program,
                        ctx,
                        frames,
                        storage,
                        stack,
                        (callee as usize, count as usize, block as usize),
                    )?;
                }
                Op::AutoCall(function, receiving) => {
                    if let Some(value) =
                        globals::get(ctx, storage, &program.functions[function as usize].name)?
                    {
                        file_bindings::receive_root(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack,
                            file_bindings::RootBinding::Value(value),
                            receiving,
                        )?;
                        continue;
                    }
                    receive_function(
                        program,
                        ctx,
                        frames,
                        storage,
                        stack.data.len(),
                        function as usize,
                        receiving,
                    )?;
                }
                Op::HostValue(host, receiving) => {
                    if let Some(value) = globals::get(ctx, storage, &program.hosts[host as usize])?
                    {
                        file_bindings::receive_root(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack,
                            file_bindings::RootBinding::Value(value),
                            receiving,
                        )?;
                        continue;
                    }
                    if !receiving.runs_static(None) {
                        return Err(receive_host(
                            program,
                            &program.hosts[host as usize],
                            receiving,
                        ));
                    }
                    let value = capabilities::registered(
                        ctx,
                        storage,
                        &hosts[host as usize],
                        &[],
                        &[],
                        None,
                    )?;
                    value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                }
                Op::RootCall(name, expanded) => {
                    if expanded
                        || !ctx.options.globals.is_empty()
                        || !ctx.capability_names.data.is_empty()
                    {
                        let mut args = Arguments::empty();
                        if let Some(value) =
                            globals::get(ctx, storage, &program.members[name as usize])?
                        {
                            args.target = Some(value_invocation(&value));
                        }
                        storage.arguments.push(ctx, args)?;
                    }
                }
                Op::Arguments => storage.arguments.push(ctx, Arguments::empty())?,
                Op::CallName(slot, name) => {
                    let target = call_targets::identifier(
                        program,
                        ctx,
                        frames,
                        storage,
                        current,
                        slot as usize,
                        name as usize,
                    )?;
                    storage.arguments.data.last_mut().unwrap().resolve(target);
                }
                Op::CallValue => {
                    let value = stack.data.pop().unwrap();
                    let args = storage.arguments.data.last_mut().unwrap();
                    // An immediate `receiver[:name](...)` left its root here.
                    let pending = args.receiver.take();
                    args.resolve(value_invocation(&value));
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
                        frame.receiver.is_some(),
                    )?;
                    let args = storage.arguments.data.last_mut().unwrap();
                    args.resolve(target);
                    args.keep_receiver(Some(selected));
                }
                Op::ResolveCall(slot, name) => {
                    let self_value = frame.receiver.clone();
                    let name_index = name;
                    let name = &program.members[name as usize];
                    let scoped = !file_bypassed(ctx, program, storage, name)?;
                    let target = if let Some(Some(value)) = storage.locals.data.get(slot as usize) {
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
                    } else if let Some(slot) = ambient_call(ctx, frames, storage, current, name)? {
                        value_invocation(storage.locals.data[slot].as_ref().unwrap())
                    } else if let Some(value) = scoped
                        .then(|| file_bindings::get(program, ctx, name))
                        .transpose()?
                        .flatten()
                    {
                        value_invocation(&value)
                    } else if !program.file && globals::contains(ctx, name)? {
                        value_invocation(&requires::get(ctx, storage, name)?.unwrap())
                    } else if scoped && program.declaration_names.contains_key(name) {
                        crate::arguments::Target::Plain(Invocation::NonCallable)
                    } else if let Some(&function) = program.names.get(name).filter(|_| scoped) {
                        crate::arguments::Target::Plain(Invocation::Function(narrow(function)))
                    } else if let Some(host) = program.hosts.iter().position(|h| h == name) {
                        crate::arguments::Target::Plain(Invocation::Host(narrow(host)))
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
                            namespaces::Member::IsType(receiver) => {
                                crate::arguments::Target::IsType(receiver)
                            }
                            namespaces::Member::Missing => {
                                let Some(module) = namespace else {
                                    return Err(undefined(program, frames, storage, current, name));
                                };
                                if !members::names::universal(name) {
                                    return Err(namespaces::missing_implicit(
                                        storage,
                                        self_value.as_ref(),
                                        &program.namespaces[module],
                                        name,
                                    ));
                                }
                                crate::arguments::Target::Plain(Invocation::ImplicitMember(
                                    narrow(module),
                                    name_index,
                                ))
                            }
                        }
                    };
                    let mut arguments = Arguments::empty();
                    arguments.resolve(target);
                    storage.arguments.push(ctx, arguments)?;
                }
                Op::Argument(op) => {
                    let value = stack.data.pop().unwrap();
                    let name = if let ArgumentOp::Keyword(name) = op {
                        &program.members[name as usize]
                    } else {
                        ""
                    };
                    let plain = !ctx.has_exports || function.plain_values.contains(frame.ip - 1);
                    storage
                        .arguments
                        .data
                        .last_mut()
                        .unwrap()
                        .push(ctx, op, name, value, plain)?;
                }
                Op::Invoke(target) | Op::InvokeRoot(target) => {
                    let mut args = storage.arguments.data.pop().unwrap();
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
                        | crate::arguments::Target::Format(..) => unreachable!(),
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
                                    name: narrow(name),
                                    method: crate::bytecode::Method::parse(&program.members[name]),
                                    auto: false,
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
                        crate::arguments::Target::IsType(receiver) => {
                            let value = dispatch::type_predicate(
                                program, ctx, frames, storage, &receiver, &args,
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
                        let receiver = if let Some(value) = &frames.data[current].receiver {
                            value.clone()
                        } else {
                            namespaces::value(program, ctx, storage, module as usize)?
                        };
                        stack.push(ctx, receiver)?;
                        Invocation::Member(
                            crate::bytecode::CallSite {
                                name,
                                method: crate::bytecode::Method::parse(
                                    &program.members[name as usize],
                                ),
                                auto: false,
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
                            if builtin == crate::builtin::Builtin::Format {
                                format::start(program, ctx, frames, storage, stack, args)?;
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
                                function as usize,
                                args,
                                stack.data.len(),
                            )?;
                        }
                        Invocation::Host(host) => {
                            let value = capabilities::registered(
                                ctx,
                                storage,
                                &hosts[host as usize],
                                &args.positional.data,
                                &args.keywords.buffer.data,
                                args.block,
                            )?;
                            value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                        }
                        Invocation::Member(site, mutating) => {
                            let ip = frames.data[current].ip - 1;
                            dispatch::member(
                                program,
                                ctx,
                                frames,
                                storage,
                                stack,
                                dispatch::Call {
                                    site,
                                    name: &program.members[site.name as usize],
                                    mutating,
                                    args,
                                    access: namespaces::Access {
                                        program: program.index,
                                        caller: namespace,
                                        implicit: false,
                                        instance: caller_instance,
                                    },
                                    plain_yields: function.plain_inputs.contains(ip),
                                    plain_result: function.plain_values.contains(ip),
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
                    let base = stack.data.len() - n as usize;
                    let value = capabilities::registered(
                        ctx,
                        storage,
                        &hosts[host as usize],
                        &stack.data[base..],
                        &[],
                        None,
                    )?;
                    stack.data.truncate(base);
                    value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                }
                Op::Method(site, n) => method(
                    program,
                    ctx,
                    frames,
                    storage,
                    stack,
                    site,
                    n as usize,
                    (namespace, caller_instance),
                )?,
                Op::MethodOf(function, class, n) => {
                    let base = stack.data.len() - n as usize - 1;
                    if let Kind::Instance(instance) = &stack.data[base].0 {
                        if program.namespace_matches(class as usize, instance.class()) {
                            let call = crate::namespace::Call {
                                function: function as usize,
                                receiver: Some(std::mem::take(&mut stack.data[base])),
                                constructor: false,
                                ignore_arguments: false,
                            };
                            // The dynamic call that follows is for other receivers.
                            frames.data[current].ip += 1;
                            let args = &stack.data[base + 1..];
                            enter_values(program, ctx, frames, storage, call, args, base)?;
                            stack.data.truncate(base);
                        }
                    }
                }
                Op::Direct(site, n) => {
                    let base = stack.data.len() - n as usize - 1;
                    let name = &program.members[site.name as usize];
                    let ip = frames.data[current].ip - 1;
                    // A member that can iterate took its arguments through a
                    // list, whose checks the direct call makes too unless
                    // the checker proved them plain.
                    let listed = iteration::method(name);
                    if listed && !function.plain_inputs.contains(ip) {
                        for arg in &stack.data[base + 1..] {
                            crate::exports::check(ctx, arg)?;
                        }
                    }
                    let direct = members::direct::call(
                        ctx,
                        site.method.unwrap(),
                        name,
                        &stack.data[base],
                        &stack.data[base + 1..],
                    )?;
                    if let Some(value) = direct {
                        stack.data.truncate(base);
                        stack.push(ctx, value)?;
                    } else if listed {
                        let args = Arguments::from_values(ctx, &stack.data[base + 1..])?;
                        stack.data.truncate(base + 1);
                        dispatch::member(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack,
                            dispatch::Call {
                                site,
                                name,
                                mutating: false,
                                args,
                                access: namespaces::Access {
                                    program: program.index,
                                    caller: namespace,
                                    implicit: false,
                                    instance: caller_instance,
                                },
                                plain_yields: false,
                                plain_result: function.plain_values.contains(ip),
                            },
                        )?;
                    } else {
                        method(
                            program,
                            ctx,
                            frames,
                            storage,
                            stack,
                            site,
                            n as usize,
                            (namespace, caller_instance),
                        )?;
                    }
                }
                Op::JumpNil(target) => {
                    if matches!(stack.data.last().unwrap().0, Kind::Nil) {
                        frame.ip = target as usize;
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
                        frame.ip = target as usize;
                    }
                }
                Op::Jump(target) => frame.ip = target as usize,
                Op::JumpFalse(target) => {
                    if !stack.data.pop().unwrap().truthy() {
                        frame.ip = target as usize;
                    }
                }
                Op::JumpTrue(target) => {
                    if stack.data.pop().unwrap().truthy() {
                        frame.ip = target as usize;
                    }
                }
                Op::Return | Op::Finish => unreachable!("returns skip the prologue"),
            }
            if ctx.has_exports
                && matches!(
                    op,
                    Op::Index(_)
                        | Op::IndexLiteral(_)
                        | Op::Method(..)
                        | Op::Direct(..)
                        | Op::Mutate(..)
                        | Op::Invoke(_)
                        | Op::InvokeRoot(_)
                        | Op::Host(..)
                        | Op::Extract(_)
                        | Op::BlockArg(..)
                )
                // A value the checker proved plain needs no scan. A call that
                // entered a frame left no value yet.
                && !(frames.data.len() == current + 1
                    && function.plain_values.contains(frames.data[current].ip - 1))
            {
                let frame = &frames.data[current];
                let target = matches!(
                    frame.program.functions[frame.function().unwrap()]
                        .code
                        .get(frame.ip),
                    Some(Op::CallValue)
                );
                if let Some(value) = visible_top(&frames.data, stack) {
                    if !target || !matches!(value.0, Kind::Function(_) | Kind::Host(_)) {
                        crate::exports::check(ctx, value)?;
                    }
                }
            }
        }
    }
}

/// Calls a member with the `count` arguments on top of the stack, above its
/// receiver, dispatching by name at runtime.
#[allow(clippy::too_many_arguments)]
fn method(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    site: crate::bytecode::CallSite,
    n: usize,
    (namespace, caller_instance): (Option<usize>, bool),
) -> Result<()> {
    let base = stack.data.len() - n - 1;
    let root = std::mem::take(&mut stack.data[base]);
    if let Some(method) =
        capabilities::member(ctx, site, &program.members[site.name as usize], &root)?
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
        return Ok(());
    }
    if dispatch::is_parse_as(ctx, site, &program.members[site.name as usize], &root)? {
        let value = dispatch::parse_as(program, ctx, frames, storage, &stack.data[base + 1..])?;
        stack.data.truncate(base);
        stack.push(ctx, value)?;
        return Ok(());
    }
    if matches!(root.0, Kind::Hash(_)) {
        if let Some(Value(Kind::Function(function))) =
            members::field(ctx, site, &program.members[site.name as usize], &root)?
        {
            if site.auto && site.scope {
                return Err(function.value_error());
            }
            let args = Arguments::from_values(ctx, &stack.data[base + 1..])?;
            stack.data.truncate(base);
            requires::invoke(ctx, frames, storage, &function, args, site.auto, base)?;
            return Ok(());
        }
    }
    if matches!(root.0, Kind::Namespace(_) | Kind::Instance(_)) {
        let receiver = &root;
        let name = &program.members[site.name as usize];
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
                let args = &stack.data[base + 1..];
                enter_values(program, ctx, frames, storage, function, args, base)?;
                stack.data.truncate(base);
                return Ok(());
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
                value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
                return Ok(());
            }
            namespaces::Member::IsType(module) => {
                let args = Arguments::from_values(ctx, &stack.data[base + 1..])?;
                let value =
                    dispatch::type_predicate(program, ctx, frames, storage, &module, &args)?;
                stack.data.truncate(base);
                stack.push(ctx, value)?;
                return Ok(());
            }
            namespaces::Member::Missing => namespaces::fallback(storage, receiver, name, false)?,
        }
    }
    let (_, value) = members::call(
        ctx,
        site,
        &program.members[site.name as usize],
        root,
        &stack.data[base + 1..],
    )?;
    stack.data.truncate(base);
    stack.push(ctx, value)?;
    Ok(())
}

fn frame_offset(frame: &Frame) -> Option<(&crate::bytecode::Program, u32)> {
    let program = &frame.program.code.program;
    let function = &program.functions[frame.function()?];
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
        .find(|(_, frame)| frame.function().is_some())
    {
        let function = frame.function().unwrap();
        let owner = &frame.program.code.program;
        let op = owner.functions[function]
            .code
            .get(frame.ip.saturating_sub(1));
        // A frame binding its parameters, or checking the host's arguments
        // to them, reports a mismatch where it was called.
        if (!frame.binding && !frame.checked)
            || !matches!(
                op,
                Some(Op::Bind(..) | Op::Normalize(..) | Op::BindIvar(..) | Op::BindField(..))
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
        .filter(|frame| !frame.binding)
        .filter_map(|frame| {
            frame
                .function()
                .map(|index| &frame.program.functions[index])
        })
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
                let function = &owner.functions[frame.function()?];
                if function.name == "<block>"
                    || function.name == "__main__"
                    || function.initializer
                    || frame.binding
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
    let Some(function) = frames.data[frame].function() else {
        return Ok(value);
    };
    let Some(ty) = program.functions[function].return_check else {
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
        if let Some(function) = frame.function() {
            for (slot, candidate) in frame.program.functions[function]
                .local_names
                .iter()
                .enumerate()
            {
                ctx.charge(1)?;
                if storage.locals.data[frame.local_base() + slot].is_some()
                    && crate::enums::compare_names(ctx, candidate.as_bytes(), name.as_bytes())?
                        == std::cmp::Ordering::Equal
                {
                    return Ok(true);
                }
            }
        }
        scope = frame.parent();
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
    let declared = program
        .ivars
        .get(&instance.class().definition.index)
        .and_then(|ivars| ivars.iter().find(|(field, _)| field == name));
    if let Some(&(_, ty)) = declared {
        return Ok(Some(ty));
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
    let context = crate::types::Context::Ivar(name.as_bytes());
    prepare_type(program, ctx, frames, storage, None, (ty, context))?
        .normalize_with(ctx, value, context)
}

/// Resolves an annotation's named types, explaining one that fails to resolve
/// in Go's words for `context`.
fn prepare_type<'a>(
    program: &'a Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    lexical: Option<usize>,
    (ty, context): (usize, crate::types::Context<'_>),
) -> Result<crate::types::Prepared<'a>> {
    let mut failed = None;
    let prepared = crate::types::prepare(ctx, &program.types[ty], |ctx, name| {
        resolve_type(program, ctx, frames, storage, lexical, name, false).inspect_err(|_| {
            failed = Some(name.to_owned());
        })
    });
    match (prepared, failed) {
        (Err(error), Some(name)) => Err(crate::types::host_resolution(ctx, context, &name, error)?),
        (prepared, _) => prepared,
    }
}

fn address_guard<'a>(
    program: Option<&'a Program>,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    address: &Address,
) -> Result<Option<crate::types::Prepared<'a>>> {
    let Some((instance, field)) = address.object_binding().filter(|_| !address.proven) else {
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
    let lexical = frames.data[frame].parent();
    prepare_type(program, ctx, frames, storage, lexical, annotation)?.normalize_with(
        ctx,
        value,
        annotation.1,
    )
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
    if let Some((outer, inner)) = name.split_once("::") {
        let outer = resolve_type(program, ctx, frames, storage, lexical, outer, false)?;
        return nested_type(program, ctx, storage, &outer, inner);
    }
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
            let function = &frame.program.functions[frame.function().unwrap()];
            let mut found = None;
            for (slot, candidate) in function.local_names.iter().enumerate() {
                ctx.charge(1)?;
                if let Some(value) = &storage.locals.data[frame.local_base() + slot] {
                    if let Some(value) =
                        type_candidate(ctx, candidate, value, binding, member, fold, enum_only)?
                    {
                        merge_type(&mut found, value, binding)?;
                    }
                }
            }
            if let Some(value) = found {
                return Ok(value);
            }
            scope = frame.parent();
        }
        let mut found = None;
        let mut declaration = None;
        if let Some(environment) = file_bindings::environment(program) {
            for (key, value) in crate::objects::bindings(ctx, environment)?.data {
                let candidate = std::str::from_utf8(key.as_bytes().unwrap()).unwrap();
                if let Some(value) =
                    type_candidate(ctx, candidate, &value, binding, member, fold, enum_only)?
                {
                    merge_type(&mut found, value, binding)?;
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
                    merge_type(&mut found, value, binding)?;
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
                merge_type(&mut found, value, binding)?;
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
                merge_type(&mut found, value, binding)?;
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

/// The class or module `path`, such as `Inner` or `Inner::Deeper`, that
/// the namespace `outer` nests, where the program declares both.
fn nested_type(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    outer: &Value,
    path: &str,
) -> Result<Value> {
    let unknown = || Error::new(ErrorKind::Type, "unknown named type");
    let (name, rest) = path
        .split_once("::")
        .map_or((path, None), |(a, b)| (a, Some(b)));
    let Kind::Namespace(namespace) = &outer.0 else {
        return Err(unknown());
    };
    let definition = &namespace.definition;
    let declared = program
        .namespaces
        .get(definition.index)
        .is_some_and(|candidate| std::sync::Arc::ptr_eq(candidate, definition));
    ctx.charge(definition.nested.len() as u64)?;
    let nested = definition
        .nested
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, index)| *index);
    let (true, Some(index)) = (declared, nested) else {
        return Err(unknown());
    };
    let value = namespaces::value(program, ctx, storage, index)?;
    match rest {
        Some(rest) => nested_type(program, ctx, storage, &value, rest),
        None => Ok(value),
    }
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
            merge_type(&mut found, value.clone(), member)?;
        }
    }
    Ok(found)
}

fn merge_type(found: &mut Option<Value>, value: Value, name: &str) -> Result<()> {
    if let Some(previous) = found {
        let same = match (&previous.0, &value.0) {
            (Kind::Enum(a), Kind::Enum(b)) => std::sync::Arc::ptr_eq(&a.definition, &b.definition),
            (Kind::Namespace(a), Kind::Namespace(b)) => a.same_binding(b),
            _ => false,
        };
        if !same {
            return Err(ambiguous_type(name, previous, &value));
        }
    } else {
        *found = Some(value);
    }
    Ok(())
}

/// Names both declarations a case-insensitive type name matched, as Go does.
fn ambiguous_type(name: &str, a: &Value, b: &Value) -> Error {
    fn declared(value: &Value) -> (&'static str, &str) {
        match &value.0 {
            Kind::Enum(enumeration) => ("enum", enumeration.definition.name.as_str()),
            Kind::Namespace(namespace) => ("class", namespace.definition.name.as_str()),
            _ => ("", ""),
        }
    }
    let ((a_kind, a_name), (b_kind, b_name)) = (declared(a), declared(b));
    let message = if a_kind == b_kind {
        let (first, second) = if a_name <= b_name {
            (a_name, b_name)
        } else {
            (b_name, a_name)
        };
        format!("ambiguous {a_kind} type {name} matches {first}, {second}")
    } else {
        let (enumeration, class) = if a_kind == "enum" {
            (a_name, b_name)
        } else {
            (b_name, a_name)
        };
        format!("ambiguous type {name} matches enum {enumeration}, class {class}")
    };
    Error::new(ErrorKind::Type, message)
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

/// Reads a bare name that no binding holds as a member of the running instance
/// or class, entering a method it selects unless `receiving` keeps the method as a
/// value, which has no members. Outside a class the name is undefined.
#[allow(clippy::too_many_arguments)]
fn implicit_read(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    current: usize,
    namespace: Option<usize>,
    name: usize,
    receiving: Receiving,
) -> Result<()> {
    let self_value = frames.data[current].receiver.clone();
    let text = &program.members[name];
    match namespaces::implicit(program, ctx, storage, namespace, self_value.as_ref(), text)? {
        namespaces::Member::Function(_) if !receiving.runs_static(None) => {
            let module = &program.namespaces[namespace.unwrap()];
            let separator = if matches!(self_value, Some(Value(Kind::Instance(_)))) {
                '#'
            } else {
                '.'
            };
            let member = &program.members[receiving.member().unwrap()];
            Err(callable_member_error(
                "method",
                &format!("{}{separator}{text}", module.name),
                member,
            ))
        }
        namespaces::Member::Function(function) => enter_arguments(
            program,
            ctx,
            frames,
            storage,
            function,
            Arguments::empty(),
            stack.data.len(),
        ),
        namespaces::Member::Value(value) => stack.push(ctx, value),
        namespaces::Member::IsType(module) => {
            let value = dispatch::type_predicate(
                program,
                ctx,
                frames,
                storage,
                &module,
                &Arguments::empty(),
            )?;
            stack.push(ctx, value)
        }
        namespaces::Member::Missing => {
            let Some(module) = namespace else {
                return Err(undefined(program, frames, storage, current, text));
            };
            if !members::names::universal(text) {
                return Err(namespaces::missing_implicit(
                    storage,
                    self_value.as_ref(),
                    &program.namespaces[module],
                    text,
                ));
            }
            let receiver = if let Some(value) = self_value {
                value
            } else {
                namespaces::value(program, ctx, storage, module)?
            };
            let site = crate::bytecode::CallSite {
                name: narrow(name),
                method: crate::bytecode::Method::parse(text),
                auto: true,
                scope: false,
            };
            let (_, value) = members::call(ctx, site, text, receiver, &[])?;
            stack.push(ctx, value)
        }
    }
}

/// Reports a bare name that no scope binds, suggesting the assigned locals, the
/// script's declarations and the builtin globals the reference exposes. A
/// removed callable constructor keeps its teaching message.
fn undefined(
    program: &Program,
    frames: &Buffer<Frame>,
    storage: &Storage,
    current: usize,
    name: &str,
) -> Error {
    if let Some(error) = namespaces::removed(name) {
        return error;
    }
    const BUILTINS: &[&str] = &[
        "assert",
        "format",
        "loop",
        "money",
        "money_cents",
        "p",
        "print",
        "puts",
        "require",
        "rand",
        "srand",
        "uuid",
        "warn",
        "random_id",
        "to_int",
        "to_float",
        "proc",
        "lambda",
        "Proc",
        "JSON",
        "Regex",
        "Math",
        "Duration",
        "Time",
    ];
    // A block also sees the assigned locals of the frames it is written in.
    let lexical = std::iter::successors(Some(current), |&index| {
        let frame = &frames.data[index];
        let block = frame
            .function()
            .is_some_and(|function| frame.program.functions[function].name == "<block>");
        block.then_some(frame.parent()).flatten()
    });
    let locals = lexical.flat_map(|index| {
        let frame = &frames.data[index];
        frame
            .function()
            .map(|function| frame.program.functions[function].local_names.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter(|(slot, _)| {
                storage
                    .locals
                    .data
                    .get(frame.local_base() + slot)
                    .is_some_and(Option::is_some)
            })
            .map(|(_, local)| local.as_bytes())
    });
    let candidates = locals
        .chain(program.names.keys().map(|name| name.as_bytes()))
        .chain(program.declaration_names.keys().map(|name| name.as_bytes()))
        .chain(program.hosts.iter().map(|name| name.as_bytes()))
        .chain(BUILTINS.iter().map(|name| name.as_bytes()));
    Error::new(
        ErrorKind::Name,
        format!(
            "undefined variable {name}{}",
            members::suggest::did_you_mean(name, candidates)
        ),
    )
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

/// Go's refusal of a member on executable code that a receiver kept as a value.
fn callable_member_error(kind: &str, name: &str, member: &str) -> Error {
    Error::new(
        ErrorKind::Type,
        format!("a {kind} has no member {member}; call {name}(...) directly"),
    )
}

/// Reads a function for `receiving`, calling it before selecting a result member.
fn receive_function(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    base: usize,
    function: usize,
    receiving: Receiving,
) -> Result<()> {
    let fun = &program.functions[function];
    let runs = receiving.runs_static(Some(fun.params.len()));
    if runs {
        return enter_auto(program, ctx, frames, storage, function, base);
    }
    let member = &program.members[receiving.member().unwrap()];
    Err(callable_member_error("function", &fun.name, member))
}

/// The error for reading a host method for `receiving`.
fn receive_host(program: &Program, host: &str, receiving: Receiving) -> Error {
    match receiving.member() {
        Some(member) if !receiving.runs_static(None) => {
            callable_member_error("method", host, &program.members[member])
        }
        _ => callable_value_error(host, "method"),
    }
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
    if fun.params.iter().any(|param| {
        !param.default
            && !matches!(
                param.kind,
                crate::syntax::ParamKind::Rest | crate::syntax::ParamKind::KeywordRest
            )
    }) {
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
    let start = match fun.proven {
        _ if fun.plain => 0,
        Some(start) => start,
        None => {
            let args = Arguments::from_values(ctx, args)?;
            return enter_arguments(program, ctx, frames, storage, function, args, base);
        }
    };
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return recursion_exceeded(ctx);
    }
    if args.len() != fun.params.len() {
        return Err(call_arity_error(fun, args.len()));
    }
    let local_base = open_locals(ctx, program, storage, Some(function))?;
    let pinned = programs::pin(ctx, storage, program.index)?;
    for (param, arg) in fun.params.iter().zip(args) {
        ctx.charge(1)?;
        storage.locals.data[local_base + param.slot] = Some(arg.clone());
    }
    // Built within the push, which spares the frame an intermediate copy.
    frames.push(
        ctx,
        Frame {
            home: (function != 0 && !fun.initializer).then_some(narrow(frames.data.len())),
            ip: start,
            ..Frame::new(pinned, storage, Some(function), base, local_base)
        },
    )
}

#[cold]
#[inline(never)]
fn instance_context_error() -> Error {
    Error::new(ErrorKind::Name, "no instance context for ivar")
}

#[cold]
#[inline(never)]
fn call_arity_error(function: &crate::bytecode::Function, arguments: usize) -> Error {
    // Go names the first parameter left without an argument.
    Error::argument(match function.params.get(arguments) {
        Some(param) => format!("missing argument {}", param.name),
        None => "unexpected positional arguments".to_owned(),
    })
}

fn enter_arguments(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    call: impl Into<crate::namespace::Call>,
    arguments: Arguments,
    base: usize,
) -> Result<()> {
    enter_checked(program, ctx, frames, storage, call, arguments, base, false)
}

/// Enters a call whose arguments the checker proved unless `checked`, when
/// they come from the host and the callee's prologue checks them.
#[allow(clippy::too_many_arguments)]
fn enter_checked(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    call: impl Into<crate::namespace::Call>,
    mut arguments: Arguments,
    base: usize,
    checked: bool,
) -> Result<()> {
    let mut call = call.into();
    let owner = callee(ctx, storage, &mut call)?;
    let program = owner.as_deref().unwrap_or(program);
    if call.constructor && call.ignore_arguments {
        arguments = Arguments::empty();
    }
    let fun = receiving(program, &call)?;
    if (fun.plain || fun.proven.is_some()) && arguments.keywords.buffer.data.is_empty() {
        enter(
            program,
            ctx,
            frames,
            storage,
            call.function,
            &arguments.positional.data,
            base,
        )?;
        let frame = frames.data.last_mut().unwrap();
        frame.block = arguments.block;
        frame.receiver = call.receiver;
        frame.constructor = call.constructor;
        if checked && !fun.plain {
            check_entry(program, ctx, frames, storage)?;
        }
        return Ok(());
    }
    bind(
        program, ctx, frames, storage, call, arguments, base, checked,
    )
}

/// Checks the host's arguments to the entered frame against its parameter
/// types, as its prologue's `Bind` instructions would, pointing a mismatch
/// at the parameter's instruction. The frame starts past the prologue, so
/// the call builds no argument binding.
fn check_entry(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
) -> Result<()> {
    let current = frames.data.len() - 1;
    let start = frames.data[current].ip;
    frames.data[current].checked = true;
    let function = &program.functions[frames.data[current].function().unwrap()];
    for (index, param) in function.params.iter().enumerate() {
        let Some(ty) = param.ty else {
            continue;
        };
        // The parameter's `Bind` is instruction `index` of the prologue.
        frames.data[current].ip = index + 1;
        let slot = frames.data[current].local_base() + param.slot;
        let value = storage.locals.data[slot].take().unwrap();
        let context = crate::types::Context::Argument(param.name.as_bytes());
        let value = normalize_type(program, ctx, frames, storage, current, (ty, context), value)?;
        storage.locals.data[slot] = Some(value);
    }
    frames.data[current].ip = start;
    frames.data[current].checked = false;
    Ok(())
}

/// Calls a script function with the arguments on top of the stack and a
/// block of the calling frame, as `(callee, count, block)`.
fn call_block(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    (callee, count, block): (usize, usize, usize),
) -> Result<()> {
    let parent = frames.data.len() - 1;
    let base = stack.data.len() - count;
    enter(
        program,
        ctx,
        frames,
        storage,
        callee,
        &stack.data[base..],
        base,
    )?;
    frames.data.last_mut().unwrap().block = Some(Block::new(block, parent));
    stack.data.truncate(base);
    Ok(())
}

/// Enters a call from script code with the positional `values` it reads
/// from the operand stack, building an argument list only for a callee
/// whose parameters bind through one.
fn enter_values(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    call: impl Into<crate::namespace::Call>,
    mut values: &[Value],
    base: usize,
) -> Result<()> {
    let mut call = call.into();
    let owner = callee(ctx, storage, &mut call)?;
    let program = owner.as_deref().unwrap_or(program);
    if call.constructor && call.ignore_arguments {
        values = &[];
    }
    let fun = receiving(program, &call)?;
    if fun.plain || fun.proven.is_some() {
        enter(program, ctx, frames, storage, call.function, values, base)?;
        let frame = frames.data.last_mut().unwrap();
        frame.receiver = call.receiver;
        frame.constructor = call.constructor;
        return Ok(());
    }
    let arguments = Arguments::from_values(ctx, values)?;
    bind(program, ctx, frames, storage, call, arguments, base, false)
}

/// Resolves the program that owns a call's receiver, and creates the
/// instance a constructor initializes.
fn callee(
    ctx: &mut CallContext,
    storage: &mut Storage,
    call: &mut crate::namespace::Call,
) -> Result<Option<Arc<Program>>> {
    let owner = call
        .receiver
        .as_ref()
        .map(|receiver| programs::receiver(ctx, storage, receiver))
        .transpose()?
        .flatten();
    if call.constructor {
        let Some(Value(Kind::Namespace(class))) = &call.receiver else {
            unreachable!()
        };
        call.receiver = Some(Value(Kind::Instance(crate::objects::new(ctx, class)?)));
    }
    Ok(owner)
}

/// The function a call enters, refusing an instance method called without
/// its instance.
fn receiving<'a>(
    program: &'a Program,
    call: &crate::namespace::Call,
) -> Result<&'a crate::bytecode::Function> {
    let fun = &program.functions[call.function];
    if fun.instance
        && !matches!(&call.receiver, Some(Value(Kind::Instance(instance))) if fun.namespace.is_some_and(|namespace| program.namespace_matches(namespace, instance.class())))
    {
        return Err(Error::new(
            ErrorKind::Type,
            "instance method requires its instance receiver",
        ));
    }
    Ok(fun)
}

/// Enters a call whose arguments bind through their list: a host entry, or
/// a callee with defaults, keywords or a rest parameter.
#[allow(clippy::too_many_arguments)]
fn bind(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    call: crate::namespace::Call,
    arguments: Arguments,
    base: usize,
    checked: bool,
) -> Result<()> {
    let function = call.function;
    let fun = &program.functions[function];
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return recursion_exceeded(ctx);
    }
    let block = arguments.block;
    let binding = Binding::new(ctx, &fun.params, arguments)?;
    let mut frame = new_frame(ctx, program, storage, Some(function), base)?;
    frame.home = (function != 0 && !fun.initializer).then_some(narrow(frames.data.len()));
    frame.checked = checked;
    frame.block = block;
    frame.receiver = call.receiver;
    frame.constructor = call.constructor;
    if fun.binds_parameters {
        // Bindings nest only through calls in default values, so grow the
        // stack one binding at a time.
        let parameters = &mut storage.parameters;
        parameters.ensure(ctx, parameters.data.len() + 1)?;
        parameters.data.push(binding);
        frame.binding = true;
    } else {
        for (i, param) in fun.params.iter().enumerate() {
            storage.locals.data[frame.local_base() + param.slot] = binding.value(ctx, i)?;
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
    let pooled = matches!(iteration, Iteration::Pooled);
    let result = (|| {
        ctx.charge(1)?;
        if frames.data.len() >= ctx.options.limits.recursion {
            return recursion_exceeded(ctx);
        }
        let mut frame = new_frame(ctx, program, storage, None, base)?;
        frame.block = args.block;
        storage.arguments.push(ctx, args)?;
        if pooled {
            frame.function = FrameCode::PooledIteration;
        } else {
            storage.iterations.push(ctx, iteration)?;
        }
        frames.push(ctx, frame)
    })();
    // Recursion guards can be rescued before the new frame exists to unwind.
    if pooled && result.is_err() {
        release_iteration_pool(storage, 1);
    }
    result
}

fn new_frame(
    ctx: &mut CallContext,
    program: &Program,
    storage: &mut Storage,
    function: Option<usize>,
    base: usize,
) -> Result<Frame> {
    let local_base = open_locals(ctx, program, storage, function)?;
    let pinned = programs::pin(ctx, storage, program.index)?;
    Ok(Frame::new(pinned, storage, function, base, local_base))
}

/// Adds a function's unbound locals to storage, returning where they start.
fn open_locals(
    ctx: &mut CallContext,
    program: &Program,
    storage: &mut Storage,
    function: Option<usize>,
) -> Result<usize> {
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
    Ok(local_base)
}

impl Frame {
    // The frame keeps its indexes in 32 bits to stay small; these widen them.

    fn function(&self) -> Option<usize> {
        match self.function {
            FrameCode::Function(index) => Some(index as usize),
            FrameCode::Iteration | FrameCode::PooledIteration => None,
        }
    }

    fn parent(&self) -> Option<usize> {
        self.parent.map(|index| index as usize)
    }

    fn home(&self) -> Option<usize> {
        self.home.map(|index| index as usize)
    }

    fn base(&self) -> usize {
        self.base as usize
    }

    fn local_base(&self) -> usize {
        self.local_base as usize
    }

    fn pooled_iteration(&self) -> bool {
        matches!(self.function, FrameCode::PooledIteration)
    }

    fn iteration_base(&self) -> usize {
        self.iteration_base as usize
    }

    fn address_base(&self) -> usize {
        self.address_base as usize
    }

    fn bypass_base(&self) -> usize {
        self.bypass_base as usize
    }

    fn text_base(&self) -> usize {
        self.text_base as usize
    }

    fn loop_base(&self) -> usize {
        self.loop_base as usize
    }

    fn argument_base(&self) -> usize {
        self.argument_base as usize
    }

    fn block_args(&self) -> usize {
        self.block_args as usize
    }

    /// A frame starting `function` over the current extent of storage.
    #[inline]
    fn new(
        program: Arc<Program>,
        storage: &Storage,
        function: Option<usize>,
        base: usize,
        local_base: usize,
    ) -> Self {
        Self {
            program,
            host: false,
            checked: false,
            plain_yields: false,
            plain_result: false,
            activation: false,
            receiver: None,
            constructor: false,
            return_to: ReturnTo::Stack,
            function: function.map_or(FrameCode::Iteration, |index| {
                FrameCode::Function(narrow(index))
            }),
            mutating: false,
            ip: 0,
            iteration_base: narrow(storage.iterations.data.len()),
            base: narrow(base),
            local_base: narrow(local_base),
            address_base: narrow(storage.addresses.data.len()),
            bypass_base: narrow(storage.bypasses.data.len()),
            text_base: narrow(storage.texts.data.len()),
            loop_base: narrow(storage.loops.data.len()),
            argument_base: narrow(storage.arguments.data.len()),
            binding: false,
            parent: None,
            home: None,
            block: None,
            block_args: 0,
        }
    }
}

/// Reports whether an assignment is evaluating the value that `slot` will receive,
/// which its calls of the same name skip.
fn bypassed(ctx: &mut CallContext, storage: &Storage, slot: usize) -> Result<bool> {
    ctx.charge(storage.bypasses.data.len() as u64)?;
    Ok(storage.bypasses.data.contains(&Bypass::Local(slot)))
}

/// Reports whether an assignment in a required file is filling its file-scope
/// binding of `name`. Go keeps that scope's locals, functions and declarations in
/// one environment, so a same-name call in the value skips all of them.
fn file_bypassed(
    ctx: &mut CallContext,
    program: &Program,
    storage: &Storage,
    name: &str,
) -> Result<bool> {
    if !program.file {
        return Ok(false);
    }
    ctx.charge(storage.bypasses.data.len() as u64)?;
    for bypass in &storage.bypasses.data {
        if let Bypass::File(owner, function, slot) = *bypass {
            if owner == program.index {
                let local = &program.functions[function].local_names[slot];
                ctx.work_bytes(local.len().max(name.len()))?;
                if local == name {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Finds the declaring frame's binding that a call in a namespace body reaches.
fn ambient_call(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    current: usize,
    name: &str,
) -> Result<Option<usize>> {
    match namespaces::ambient_slot(ctx, frames, storage, current, name)? {
        Some(slot) if !bypassed(ctx, storage, slot)? => Ok(Some(slot)),
        _ => Ok(None),
    }
}

fn resolve_slot(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    mut frame: usize,
    mut slot: usize,
    skip: bool,
) -> Result<usize> {
    let own = frames.data[frame].local_base() + slot;
    let function = &frames.data[frame].program.functions[frames.data[frame].function().unwrap()];
    if function.initializer && storage.locals.data[own].is_none() {
        if let Some(slot) =
            namespaces::ambient_slot(ctx, frames, storage, frame, &function.local_names[slot])?
        {
            return Ok(if skip && bypassed(ctx, storage, slot)? {
                usize::MAX
            } else {
                slot
            });
        }
    }
    loop {
        let local = frames.data[frame].local_base() + slot;
        if storage.locals.data[local].is_some() {
            if !skip {
                return Ok(local);
            }
            ctx.charge(storage.bypasses.data.len() as u64)?;
            if !storage.bypasses.data.contains(&Bypass::Local(local)) {
                return Ok(local);
            }
        }
        let Some(capture) = frames.data[frame].program.functions
            [frames.data[frame].function().unwrap()]
        .captures
        .get(slot)
        .copied()
        .flatten() else {
            return Ok(if skip { usize::MAX } else { own });
        };
        for _ in 0..=capture.depth {
            ctx.charge(1)?;
            frame = frames.data[frame].parent().unwrap();
        }
        slot = capture.slot;
    }
}

/// Enters a block with the `count` arguments on top of the operand stack,
/// which stay there for its prologue to read and leave with its frame.
fn enter_block(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &Buffer<Value>,
    block: Block,
    count: usize,
) -> Result<()> {
    ctx.charge(1)?;
    if frames.data.len() >= ctx.options.limits.recursion {
        return recursion_exceeded(ctx);
    }
    let base = stack.data.len() - count;
    let parent = &frames.data[block.parent()];
    let (receiver, home, outer) = (parent.receiver.clone(), parent.home, parent.block);
    let program = parent.program.clone();
    let mut frame = new_frame(ctx, &program, storage, Some(block.function()), base)?;
    frame.receiver = receiver;
    frame.parent = Some(narrow(block.parent()));
    frame.home = home;
    frame.block = outer;
    frame.block_args = narrow(count);
    // Charges the work of copying the arguments, as a separate list did.
    for chunk in stack.data[base..].chunks(crate::budget::CHUNK / std::mem::size_of::<Value>()) {
        ctx.work_bytes(std::mem::size_of_val(chunk))?;
    }
    frames.push(ctx, frame)
}

/// A program's shared literal by slot, imported on its first use in the
/// call and shared after that. Each use charges the step an import charges.
fn shared(
    ctx: &mut CallContext,
    program: &Program,
    storage: &mut Storage,
    slot: usize,
) -> Result<Value> {
    let base = storage.programs.data[program.index].shared;
    if let Some(Some(value)) = base.map(|base| &storage.shared.data[base + slot]) {
        ctx.charge(1)?;
        return Ok(value.clone());
    }
    let value = ctx.import(&program.constants[program.shared[slot]])?;
    let base = match base {
        Some(base) => base,
        None => {
            let base = storage.shared.data.len();
            storage.shared.ensure(ctx, base + program.shared.len())?;
            storage
                .shared
                .data
                .resize(base + program.shared.len(), None);
            storage.programs.data[program.index].shared = Some(base);
            base
        }
    };
    storage.shared.data[base + slot] = Some(value.clone());
    Ok(value)
}

/// The top of the operand stack below the arguments that block frames keep
/// there, which a frame's own values would otherwise sit directly on.
fn visible_top<'a>(frames: &[Frame], stack: &'a Buffer<Value>) -> Option<&'a Value> {
    let mut length = stack.data.len();
    for frame in frames.iter().rev() {
        if length != frame.base() + frame.block_args() {
            break;
        }
        length = frame.base();
    }
    length.checked_sub(1).map(|top| &stack.data[top])
}

/// The argument a block's prologue reads at `index`: from the one argument
/// when it is an array and the block names several.
fn block_arg<'a>(
    frame: &Frame,
    stack: &'a Buffer<Value>,
    index: usize,
    autosplat: bool,
) -> Option<&'a Value> {
    let args = &stack.data[frame.base()..frame.base() + frame.block_args()];
    let args = match args {
        [single] if autosplat => single.as_array().unwrap_or(args),
        _ => args,
    };
    args.get(index)
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
    let binding = frames.data[target..]
        .iter()
        .filter(|frame| frame.binding)
        .count();
    let parameters = storage.parameters.data.len() - binding;
    storage.parameters.data.truncate(parameters);
    let frame = &frames.data[target];
    storage.loops.data.truncate(frame.loop_base());
    storage.arguments.data.truncate(frame.argument_base());
    stack.data.truncate(frame.base());
    storage.locals.data.truncate(frame.local_base());
    storage.iterations.data.truncate(frame.iteration_base());
    let pooled = frames.data[target..]
        .iter()
        .filter(|frame| frame.pooled_iteration())
        .count();
    release_iteration_pool(storage, pooled);
    storage.addresses.data.truncate(frame.address_base());
    storage.bypasses.data.truncate(frame.bypass_base());
    storage.texts.data.truncate(frame.text_base());
    frames.data.truncate(target);
}

fn release_iteration_pool(storage: &mut Storage, pooled: usize) {
    if pooled != 0 {
        let remaining = storage.iteration_pool.data.len() - pooled;
        storage.iteration_pool.data.truncate(remaining);
        if remaining == 0 {
            storage.iteration_pool = Buffer::empty();
        }
    }
}
