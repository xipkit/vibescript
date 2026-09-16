use super::*;
use crate::{ErrorClass, budget::Charge};
use std::{mem::size_of, sync::Arc};

pub(super) enum Event {
    Control(Control),
    Error(Arc<SavedError>),
}

#[derive(Clone, Copy)]
pub(super) enum Jump {
    Break,
    Next,
    Return,
}

impl Jump {
    fn error(self) -> Error {
        Error::local_jump(match self {
            Self::Break => "break cannot cross call boundary",
            Self::Next => "next cannot cross call boundary",
            Self::Return => "unexpected return",
        })
    }
}

pub(super) enum Control {
    Invalid {
        frame: usize,
        jump: Jump,
        _value: Option<Value>,
    },
    Return {
        target: usize,
        value: Value,
        normalize: bool,
    },
    Break {
        target: usize,
        loop_index: usize,
        value: Option<Value>,
    },
    Next {
        target: usize,
        loop_index: usize,
    },
    Retry {
        handler: usize,
    },
}

pub(super) struct SavedError {
    pub error: Error,
    _charge: Option<Charge>,
}

impl SavedError {
    pub fn new(
        program: &Program,
        ctx: &mut CallContext,
        frames: &[Frame],
        entry: usize,
        mut error: Error,
    ) -> Result<Arc<Self>> {
        ctx.work_bytes(error.message_bytes().len())?;
        let mut charge =
            ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>() + error.allocation_bytes())?;
        if let Some(diagnostic) = &error.diagnostic {
            Charge::merge(
                &mut charge,
                ctx.reserve(
                    size_of::<crate::Diagnostic>()
                        + 2 * size_of::<usize>()
                        + diagnostic.code_frame.capacity()
                        + diagnostic.frames.capacity() * size_of::<crate::StackFrame>(),
                )?,
            );
            for (index, frame) in diagnostic.frames.iter().enumerate() {
                ctx.charge(index as u64 + 1)?;
                if !diagnostic.frames[..index]
                    .iter()
                    .any(|prior| Arc::ptr_eq(&prior.function, &frame.function))
                {
                    ctx.work_bytes(frame.function.len())?;
                    Charge::merge(
                        &mut charge,
                        ctx.reserve(frame.function.len() + 2 * size_of::<usize>())?,
                    );
                }
            }
        } else {
            ctx.charge(frames.len() as u64)?;
            let (program, frames, offset) = diagnostic_site(program, frames, entry);
            let position = program.source.position_metered(ctx, offset)?;
            let (code_frame, snippet_charge) =
                program.source.frame_metered(ctx, offset, position)?;
            Charge::merge(&mut charge, snippet_charge);
            let mut trace: Buffer<crate::StackFrame> =
                Buffer::with_capacity(ctx, trace_entries(program, frames, offset).count())?;
            for (name, source, at) in trace_entries(program, frames, offset) {
                ctx.charge(trace.data.len() as u64 + 1)?;
                let seen = name.is_some_and(|name| {
                    trace
                        .data
                        .iter()
                        .any(|prior| Arc::ptr_eq(&prior.function, name))
                });
                if !seen {
                    let name = name.map_or("<script>", |name| &**name);
                    ctx.work_bytes(name.len())?;
                    Charge::merge(
                        &mut charge,
                        ctx.reserve(name.len() + 2 * size_of::<usize>())?,
                    );
                }
                trace.data.push(crate::StackFrame {
                    function: name.cloned().unwrap_or_else(|| "<script>".into()),
                    position: source.position_metered(ctx, at)?,
                });
            }
            let (trace, trace_charge) = trace.into_parts();
            Charge::merge(&mut charge, trace_charge);
            Charge::merge(
                &mut charge,
                ctx.reserve(size_of::<crate::Diagnostic>() + 2 * size_of::<usize>())?,
            );
            error.offset = Some(offset as usize);
            error.diagnostic = Some(Arc::new(crate::Diagnostic {
                position,
                code_frame,
                frames: trace,
            }));
        }
        Ok(Arc::new(Self {
            error,
            _charge: charge,
        }))
    }

    pub fn into_error(self: Arc<Self>) -> Error {
        match Arc::try_unwrap(self) {
            Ok(saved) => saved.error,
            Err(saved) => saved.error.clone(),
        }
    }

    fn value(&self, ctx: &mut CallContext) -> Result<Value> {
        let mut hash = Hash::empty();
        let class = ctx.bytes(
            self.error
                .class()
                .unwrap_or(ErrorClass::Runtime)
                .name()
                .as_bytes(),
        )?;
        let message = ctx.bytes(self.error.message_bytes())?;
        let code = ctx.bytes(
            self.error
                .diagnostic
                .as_ref()
                .map_or("", |d| d.code_frame.as_str())
                .as_bytes(),
        )?;
        let mut trace = Buffer::empty();
        if let Some(diagnostic) = &self.error.diagnostic {
            for frame in &diagnostic.frames {
                let (text, _charge) = crate::source::formatted(
                    ctx,
                    format_args!(
                        "{}:{}:in `{}`",
                        frame.position.line, frame.position.column, frame.function
                    ),
                )?;
                let value = ctx.bytes(text.as_bytes())?;
                trace.push(ctx, value)?;
            }
        }
        let trace = Value::from_array(ctx, trace)?;
        for (key, value) in [
            ("type", class.clone()),
            ("class", class),
            ("message", message.clone()),
            ("to_s", message),
            ("code_frame", code),
            ("backtrace", trace),
        ] {
            let key = ctx.bytes(key.as_bytes())?;
            hash.insert(ctx, key, value)?;
        }
        hash.object = true;
        hash.tag = crate::hash::Tag::Error;
        Ok(Value(Kind::Hash(hash.into_arc(ctx)?)))
    }
}

#[derive(Clone, Copy)]
struct Snapshot {
    stack: usize,
    addresses: usize,
    bypasses: usize,
    texts: usize,
    arguments: usize,
    bindings: usize,
    loops: usize,
    iterations: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Body,
    Rescue,
    Else,
    Ensure,
}

pub(super) enum Pending {
    Value(Value),
    Control(Control),
    Error(Arc<SavedError>),
}

pub(super) struct Handler {
    frame: usize,
    spec: usize,
    snapshot: Snapshot,
    phase: Phase,
    binding: Option<usize>,
    error: Option<Arc<SavedError>>,
    pending: Option<Pending>,
}

fn declare(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    frame: usize,
    slots: &[usize],
) -> Result<()> {
    for &slot in slots {
        ctx.charge(1)?;
        let resolved = resolve_slot(ctx, frames, storage, frame, slot, false)?;
        let owner = &frames.data[frame];
        let program = &owner.program;
        if file_bindings::local(program, ctx, frames, storage, frame, slot, resolved)? {
            let name = &program.functions[owner.function.unwrap()].local_names[slot];
            file_bindings::declare(program, ctx, storage, name)?;
        } else {
            storage.locals.data[resolved].get_or_insert_with(Value::nil);
        }
    }
    Ok(())
}

fn restore(
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    handler: usize,
) {
    let h = &storage.handlers.data[handler];
    let (owner, state) = (h.frame, h.snapshot);
    if frames.data.len() > owner + 1 {
        unwind(frames, storage, stack, owner + 1);
    }
    let frame = &mut frames.data[owner];
    stack.data.truncate(state.stack);
    storage.addresses.data.truncate(state.addresses);
    storage.bypasses.data.truncate(state.bypasses);
    storage.texts.data.truncate(state.texts);
    storage.iterations.data.truncate(state.iterations);
    frame.arguments.data.truncate(state.arguments);
    frame.binding.data.truncate(state.bindings);
    frame.loops.data.truncate(state.loops);
}

fn clear_binding(storage: &mut Storage, handler: usize) {
    if let Some(slot) = storage.handlers.data[handler].binding.take() {
        storage.locals.data[slot] = None;
    }
    storage.handlers.data[handler].error = None;
}

pub(super) fn begin(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    stack: &Buffer<Value>,
    spec: usize,
) -> Result<()> {
    let owner = frames.data.len() - 1;
    let frame = &frames.data[owner];
    let snapshot = Snapshot {
        stack: stack.data.len(),
        addresses: storage.addresses.data.len(),
        bypasses: storage.bypasses.data.len(),
        texts: storage.texts.data.len(),
        arguments: frame.arguments.data.len(),
        bindings: frame.binding.data.len(),
        loops: frame.loops.data.len(),
        iterations: storage.iterations.data.len(),
    };
    storage.handlers.push(
        ctx,
        Handler {
            frame: owner,
            spec,
            snapshot,
            phase: Phase::Body,
            binding: None,
            error: None,
            pending: None,
        },
    )
}

pub(super) fn current_error(storage: &Storage) -> Option<Arc<SavedError>> {
    storage.handlers.data.iter().rev().find_map(|h| {
        (h.phase == Phase::Rescue)
            .then(|| h.error.clone())
            .flatten()
    })
}

pub(super) fn retry(storage: &Storage) -> Result<Control> {
    let Some((handler, _)) = storage
        .handlers
        .data
        .iter()
        .enumerate()
        .rev()
        .find(|(_, h)| h.phase == Phase::Rescue)
    else {
        return Err(Error::new(
            ErrorKind::Runtime,
            "retry used outside of rescue",
        ));
    };
    Ok(Control::Retry { handler })
}

fn prepare_ensure(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    handler: usize,
    pending: Pending,
) -> Result<Option<Pending>> {
    let h = &storage.handlers.data[handler];
    let (owner, index) = (h.frame, h.spec);
    let program = frames.data[owner].program.clone();
    let spec = &program.handlers[index];
    declare(ctx, frames, storage, owner, &spec.body_locals)?;
    for clause in &spec.rescues {
        declare(ctx, frames, storage, owner, &clause.locals)?;
    }
    declare(ctx, frames, storage, owner, &spec.alternate_locals)?;
    clear_binding(storage, handler);
    restore(frames, storage, stack, handler);
    if let Some(ensure) = spec.ensure {
        let h = &mut storage.handlers.data[handler];
        h.phase = Phase::Ensure;
        h.pending = Some(pending);
        frames.data[owner].ip = ensure;
        Ok(None)
    } else {
        storage.handlers.data.pop();
        frames.data[owner].ip = spec.end;
        Ok(Some(pending))
    }
}

pub(super) fn normal(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    body: bool,
) -> Result<()> {
    let index = storage.handlers.data.len() - 1;
    let h = &storage.handlers.data[index];
    let program = frames.data[h.frame].program.clone();
    let (owner, spec) = (h.frame, &program.handlers[h.spec]);
    let value = stack.data.pop().unwrap();
    if body {
        declare(ctx, frames, storage, owner, &spec.body_locals)?;
        for clause in &spec.rescues {
            declare(ctx, frames, storage, owner, &clause.locals)?;
        }
        if let Some(alternate) = spec.alternate {
            storage.handlers.data[index].phase = Phase::Else;
            frames.data[owner].ip = alternate;
            return Ok(());
        }
    }
    if let Some(Pending::Value(value)) =
        prepare_ensure(ctx, frames, storage, stack, index, Pending::Value(value))?
    {
        stack.push(ctx, value)?;
    }
    Ok(())
}

pub(super) fn end_ensure(
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    ctx: &mut CallContext,
) -> Result<Option<Event>> {
    let mut h = storage.handlers.data.pop().unwrap();
    frames.data[h.frame].ip = frames.data[h.frame].program.handlers[h.spec].end;
    match h.pending.take().unwrap() {
        Pending::Value(value) => {
            stack.push(ctx, value)?;
            Ok(None)
        }
        Pending::Control(control) => Ok(Some(Event::Control(control))),
        Pending::Error(error) => Ok(Some(Event::Error(error))),
    }
}

pub(super) fn error(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    error: Arc<SavedError>,
) -> Result<()> {
    ctx.checkpoint()?;
    while let Some(index) = storage.handlers.data.len().checked_sub(1) {
        ctx.charge(1)?;
        let h = &storage.handlers.data[index];
        let program = frames.data[h.frame].program.clone();
        let (owner, spec, phase) = (h.frame, &program.handlers[h.spec], h.phase);
        if phase == Phase::Ensure {
            clear_binding(storage, index);
            storage.handlers.data.pop();
            continue;
        }
        restore(frames, storage, stack, index);
        declare(ctx, frames, storage, owner, &spec.body_locals)?;
        if phase == Phase::Body {
            for clause in &spec.rescues {
                ctx.charge(1)?;
                if error
                    .error
                    .class()
                    .is_some_and(|class| clause.classes.iter().any(|filter| filter.matches(class)))
                {
                    if clause.empty {
                        break;
                    }
                    let h = &mut storage.handlers.data[index];
                    h.phase = Phase::Rescue;
                    h.error = Some(error.clone());
                    frames.data[owner].ip = clause.start;
                    if let Some(slot) = clause.binding {
                        let slot = frames.data[owner].local_base + slot;
                        let value = error.value(ctx)?;
                        storage.locals.data[slot] = Some(value);
                        storage.handlers.data[index].binding = Some(slot);
                    }
                    return Ok(());
                }
                declare(ctx, frames, storage, owner, &clause.locals)?;
            }
        }
        if prepare_ensure(
            ctx,
            frames,
            storage,
            stack,
            index,
            Pending::Error(error.clone()),
        )?
        .is_none()
        {
            return Ok(());
        }
    }
    Err(error.into_error())
}

pub(super) fn intercept(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    mut control: Control,
) -> Result<Option<Control>> {
    let current = frames.data.len() - 1;
    let cross_call_retry = matches!(&control, Control::Retry { handler } if storage.handlers.data[*handler].frame != current);
    while let Some(index) = storage.handlers.data.len().checked_sub(1) {
        let h = &storage.handlers.data[index];
        if cross_call_retry && h.frame != current {
            break;
        }
        let exits = match &control {
            Control::Invalid { frame, .. } => h.frame >= *frame,
            Control::Return { target, .. } => h.frame >= *target,
            Control::Break {
                target, loop_index, ..
            }
            | Control::Next { target, loop_index } => {
                h.frame > *target || (h.frame == *target && h.snapshot.loops > *loop_index)
            }
            Control::Retry { handler } => index > *handler,
        };
        if !exits {
            break;
        }
        ctx.charge(1)?;
        if h.phase == Phase::Ensure {
            clear_binding(storage, index);
            storage.handlers.data.pop();
            continue;
        }
        match prepare_ensure(
            ctx,
            frames,
            storage,
            stack,
            index,
            Pending::Control(control),
        )? {
            None => return Ok(None),
            Some(Pending::Control(pending)) => control = pending,
            _ => unreachable!(),
        }
    }
    if cross_call_retry {
        unwind(frames, storage, stack, current);
        return Err(Error::local_jump("retry cannot cross call boundary"));
    }
    Ok(Some(control))
}

pub(super) fn restart(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    index: usize,
) -> Result<()> {
    ctx.charge(1)?;
    clear_binding(storage, index);
    restore(frames, storage, stack, index);
    let h = &mut storage.handlers.data[index];
    h.phase = Phase::Body;
    h.pending = None;
    frames.data[h.frame].ip = frames.data[h.frame].program.handlers[h.spec].body;
    Ok(())
}

pub(super) fn class(value: &Value) -> Option<ErrorClass> {
    let Kind::Namespace(namespace) = &value.0 else {
        return None;
    };
    ErrorClass::from_name(&namespace.definition.name)
}

pub(super) fn class_constant_bound(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    current: usize,
    name: &str,
) -> Result<bool> {
    let program = &frames.data[current].program;
    let module = frames.data[current]
        .function
        .and_then(|f| program.functions[f].namespace);
    if let Some(module) = module {
        return namespaces::field(program, ctx, storage, module, name).map(|value| value.is_some());
    }
    Ok(false)
}

pub(super) fn raise(
    ctx: &mut CallContext,
    class: Option<ErrorClass>,
    message: Value,
    typed: bool,
) -> Result<Error> {
    let Kind::Bytes(bytes) = &message.0 else {
        return Ok(Error::new(
            ErrorKind::Type,
            if typed {
                "exception message must be string"
            } else if matches!(message.0, Kind::Nil) {
                "exception object expected"
            } else {
                "exception class/object expected"
            },
        )
        .with_class(ErrorClass::Type));
    };
    if typed && class.is_none() {
        return Ok(
            Error::new(ErrorKind::Type, "exception class/object expected")
                .with_class(ErrorClass::Type),
        );
    }
    Ok(Error::from_bytes(ctx, &bytes.data)?.with_class(class.unwrap_or(ErrorClass::Runtime)))
}

pub(super) fn guard_loop(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    breaking: bool,
) -> Result<()> {
    for frame in frames.data.iter().rev() {
        ctx.charge(1)?;
        if !frame.loops.data.is_empty()
            || frame
                .function
                .is_none_or(|i| frame.program.functions[i].name == "<block>")
        {
            return Ok(());
        }
    }
    Err(Error::new(
        ErrorKind::Argument,
        if breaking {
            "break outside loop"
        } else {
            "next outside loop"
        },
    ))
}

fn invalid_loop_control(
    frames: &Buffer<Frame>,
    breaking: bool,
    value: Option<Value>,
) -> Result<Control> {
    let frame = frames.data.len() - 1;
    if frames.data[..frame].iter().any(|f| {
        !f.loops.data.is_empty()
            || f.function
                .is_none_or(|i| f.program.functions[i].name == "<block>")
    }) {
        Ok(Control::Invalid {
            frame,
            jump: if breaking { Jump::Break } else { Jump::Next },
            _value: value,
        })
    } else {
        Err(Error::new(
            ErrorKind::Argument,
            if breaking {
                "break outside loop"
            } else {
                "next outside loop"
            },
        ))
    }
}

pub(super) fn loop_control(
    frames: &Buffer<Frame>,
    breaking: bool,
    value: Option<Value>,
) -> Result<Control> {
    let current = frames.data.len() - 1;
    let frame = &frames.data[current];
    if let Some(loop_index) = frame.loops.data.len().checked_sub(1) {
        return Ok(if breaking {
            Control::Break {
                target: current,
                loop_index,
                value,
            }
        } else {
            Control::Next {
                target: current,
                loop_index,
            }
        });
    }
    if frame.parent.is_none() {
        return invalid_loop_control(frames, breaking, value);
    }
    if !breaking {
        return Ok(Control::Return {
            target: current,
            value: value.unwrap_or_default(),
            normalize: false,
        });
    }
    let target = (0..current)
        .rev()
        .find(|&i| !frames.data[i].loops.data.is_empty() || frames.data[i].parent.is_none())
        .unwrap();
    if let Some(loop_index) = frames.data[target].loops.data.len().checked_sub(1) {
        return Ok(Control::Break {
            target,
            loop_index,
            value,
        });
    }
    if frames.data[target].block.is_none() {
        return invalid_loop_control(frames, true, value);
    }
    Ok(Control::Return {
        target,
        value: value.unwrap_or_default(),
        normalize: true,
    })
}

pub(super) fn apply_control(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    pending_entry: bool,
    control: Control,
) -> Result<Option<Value>> {
    match control {
        Control::Invalid { frame, jump, .. } => {
            unwind(frames, storage, stack, frame);
            return Err(jump.error());
        }
        Control::Retry { handler } => restart(ctx, frames, storage, stack, handler)?,
        Control::Break {
            target,
            loop_index,
            value,
        } => {
            if frames.data.len() > target + 1 {
                unwind(frames, storage, stack, target + 1);
            }
            let frame = &mut frames.data[target];
            frame.loops.data.truncate(loop_index + 1);
            let state = &mut frame.loops.data[loop_index];
            state.broken = true;
            state.break_value = value;
            stack.data.truncate(state.base);
            storage.addresses.data.truncate(state.address_base);
            storage.bypasses.data.truncate(state.bypass_base);
            storage.texts.data.truncate(state.text_base);
            frame.arguments.data.truncate(state.argument_base);
            frame.ip = state.end;
        }
        Control::Next { target, loop_index } => {
            if frames.data.len() > target + 1 {
                unwind(frames, storage, stack, target + 1);
            }
            let frame = &mut frames.data[target];
            frame.loops.data.truncate(loop_index + 1);
            let state = &frame.loops.data[loop_index];
            stack.data.truncate(state.base);
            storage.addresses.data.truncate(state.address_base);
            storage.bypasses.data.truncate(state.bypass_base);
            storage.texts.data.truncate(state.text_base);
            frame.arguments.data.truncate(state.argument_base);
            frame.ip = state.next;
        }
        Control::Return {
            target,
            value,
            normalize,
        } => {
            let value = if normalize {
                normalize_return(ctx, frames, storage, target, value)?
            } else {
                value
            };
            let return_to = std::mem::take(&mut frames.data[target].return_to);
            let initialized = frames.data[target].function.and_then(|function| {
                frames.data[target].program.functions[function]
                    .initializer
                    .then(|| {
                        frames.data[target].program.functions[function]
                            .namespace
                            .unwrap()
                    })
            });
            if let Some(module) = initialized {
                let program = frames.data[target].program.clone();
                let state = namespaces::state(&program, ctx, storage, module)?;
                namespaces::initialized(ctx, storage, state)?;
            }
            unwind(frames, storage, stack, target);
            if frames.data.is_empty() {
                if pending_entry {
                    return Ok(None);
                }
                ctx.checkpoint()?;
                return Ok(Some(value));
            }
            if initialized.is_some() {
                return Ok(None);
            }
            match return_to {
                ReturnTo::Output => {
                    let program = frames.data.last().unwrap().program.clone();
                    output::resume(&program, ctx, frames, storage, stack, Some(value))?
                }
                ReturnTo::Format => {
                    let program = frames.data.last().unwrap().program.clone();
                    format::resume(&program, ctx, frames, storage, stack, Some(value))?
                }
                ReturnTo::Stack => stack.push(ctx, value)?,
                ReturnTo::Address => storage.addresses.push(ctx, Address::new(None, value))?,
                ReturnTo::Assigned(value) => stack.push(ctx, value)?,
                ReturnTo::Negate => stack.push(ctx, Value::boolean(!value.truthy()))?,
                ReturnTo::Local(slot) => {
                    address::refresh(ctx, slot, &value, &mut storage.addresses.data, &[])?;
                    storage.locals.data[slot] = Some(value.clone());
                    stack.push(ctx, value)?;
                }
                ReturnTo::Text(original) => {
                    let rendered = if matches!(value.0, Kind::Bytes(_)) {
                        value
                    } else {
                        original
                    };
                    crate::text::append(ctx, &rendered, storage.texts.data.last_mut().unwrap())?;
                }
            }
        }
    }
    Ok(None)
}
