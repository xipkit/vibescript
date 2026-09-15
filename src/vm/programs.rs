use super::*;
use crate::{budget::Charge, code::Code};
use std::{ops::Deref, sync::Arc};

pub(super) struct Program {
    pub code: Arc<Code>,
    pub index: usize,
    pub global_base: usize,
    _charge: Option<Charge>,
}

pub(super) struct Entry {
    pub program: Arc<Program>,
    failed: bool,
}

pub(super) struct Activation {
    program: usize,
    next: usize,
    waiting: Option<(usize, usize)>,
}

impl Deref for Program {
    type Target = crate::bytecode::Program;

    fn deref(&self) -> &Self::Target {
        &self.code.program
    }
}

pub(super) fn load(
    ctx: &mut CallContext,
    storage: &mut Storage,
    code: &Arc<Code>,
) -> Result<Arc<Program>> {
    if let Some(index) = registered(ctx, storage, code)? {
        return Ok(storage.programs.data[index].program.clone());
    }
    let global_base = storage.globals.data.len();
    let Some(end) = global_base.checked_add(code.program.globals.len()) else {
        return ctx.fail(ErrorKind::Memory, "allocation size overflow");
    };
    storage
        .programs
        .ensure(ctx, storage.programs.data.len() + 1)?;
    storage.globals.ensure(ctx, end)?;
    let charge = ctx.reserve(size_of::<Program>() + 2 * size_of::<usize>())?;
    Code::retain(ctx, code)?;
    let program = Arc::new(Program {
        code: code.clone(),
        index: storage.programs.data.len(),
        global_base,
        _charge: charge,
    });
    storage.globals.data.resize(end, None);
    storage.programs.data.push(Entry {
        program: program.clone(),
        failed: false,
    });
    Ok(program)
}

fn registered(ctx: &mut CallContext, storage: &Storage, code: &Arc<Code>) -> Result<Option<usize>> {
    // The entry program has a fixed lookup covered by the executing operation.
    if let Some(entry) = storage.programs.data.first() {
        if Arc::ptr_eq(&entry.program.code, code) {
            return Ok(Some(0));
        }
    }
    for entry in storage.programs.data.iter().skip(1) {
        ctx.charge(1)?;
        let program = &entry.program;
        if Arc::ptr_eq(&program.code, code) {
            return Ok(Some(program.index));
        }
    }
    Ok(None)
}

pub(super) fn arguments(ctx: &mut CallContext, storage: &mut Storage) -> Result<()> {
    let count = ctx.code_roots.as_ref().unwrap().data.len();
    // No callbacks have run yet; all retained sources belong to the accepted arguments.
    for index in (0..count).rev() {
        ctx.charge(1)?;
        let code = ctx.code_roots.as_ref().unwrap().data[index].clone();
        let program = load(ctx, storage, &code)?;
        if program.index != 0 {
            activate(ctx, storage, program.index)?;
        }
    }
    storage.discovered = count;
    Ok(())
}

fn activate(ctx: &mut CallContext, storage: &mut Storage, program: usize) -> Result<()> {
    storage.activations.push(
        ctx,
        Activation {
            program,
            next: 0,
            waiting: None,
        },
    )
}

fn discovered(ctx: &mut CallContext, storage: &mut Storage) -> Result<()> {
    while let Some(code) = ctx
        .code_roots
        .as_ref()
        .unwrap()
        .data
        .get(storage.discovered)
        .cloned()
    {
        if registered(ctx, storage, &code)?.is_none() {
            break;
        }
        storage.discovered += 1;
    }
    Ok(())
}

pub(super) fn namespace(
    ctx: &mut CallContext,
    storage: &mut Storage,
    namespace: &crate::namespace::Namespace,
) -> Result<Arc<Program>> {
    let owner = namespace
        .owner
        .as_ref()
        .ok_or_else(|| Error::new(ErrorKind::Type, "namespace has no executable source"))?;
    let program = load(ctx, storage, owner)?;
    if storage.programs.data[program.index].failed {
        return Err(Error::new(
            ErrorKind::Runtime,
            "source script initialization failed",
        ));
    }
    Ok(program)
}

pub(super) fn receiver(
    ctx: &mut CallContext,
    storage: &mut Storage,
    value: &Value,
) -> Result<Option<Arc<Program>>> {
    match &value.0 {
        Kind::Namespace(value) => namespace(ctx, storage, value).map(Some),
        Kind::Instance(value) => namespace(ctx, storage, value.class()).map(Some),
        _ => Ok(None),
    }
}

pub(super) fn address(
    ctx: &mut CallContext,
    storage: &mut Storage,
    address: &Address,
) -> Result<Option<Arc<Program>>> {
    address
        .object_binding()
        .map(|(instance, _)| namespace(ctx, storage, instance.class()))
        .transpose()
}

pub(super) fn imported(ctx: &mut CallContext, storage: &mut Storage, value: &Value) -> Result<()> {
    // Failed or discarded host imports retain code for cleanup without admitting it.
    discovered(ctx, storage)?;
    ctx.charge(storage.activations.data.len() as u64)?;
    let mut pending = storage
        .activations
        .data
        .iter()
        .filter(|a| a.waiting.is_none())
        .count();
    if pending == 0 && storage.discovered == ctx.code_roots.as_ref().unwrap().data.len() {
        return Ok(());
    }
    let mut values = Buffer::empty();
    let mut instances: Buffer<Arc<crate::objects::Instance>> = Buffer::empty();
    let mut needed = Buffer::empty();
    values.push(ctx, value.clone())?;
    while let Some(value) = values.data.pop() {
        ctx.charge(1)?;
        let namespace = match &value.0 {
            Kind::Namespace(namespace) => Some(namespace),
            Kind::Instance(instance) => {
                let mut seen = false;
                for previous in &instances.data {
                    ctx.charge(1)?;
                    if previous.same(instance) {
                        seen = true;
                        break;
                    }
                }
                if seen {
                    continue;
                }
                instances.push(ctx, instance.clone())?;
                crate::objects::children(ctx, instance, &mut values)?;
                Some(instance.class())
            }
            Kind::Array(array) => {
                for value in array.buffer.data.iter().rev() {
                    ctx.charge(1)?;
                    values.push(ctx, value.clone())?;
                }
                None
            }
            Kind::Hash(hash) => {
                for (key, value) in hash.buffer.data.iter().rev() {
                    ctx.charge(1)?;
                    values.push(ctx, value.clone())?;
                    values.push(ctx, key.clone())?;
                }
                None
            }
            _ => None,
        };
        let Some(owner) = namespace.and_then(|namespace| namespace.owner.as_ref()) else {
            continue;
        };
        let next_program = storage.programs.data.len();
        let program = load(ctx, storage, owner)?;
        if program.index == next_program {
            activate(ctx, storage, program.index)?;
            pending += 1;
            discovered(ctx, storage)?;
        }
        for activation in &storage.activations.data {
            ctx.charge(1)?;
            if activation.waiting.is_none() && activation.program == program.index {
                ctx.charge(needed.data.len() as u64)?;
                if !needed.data.contains(&activation.program) {
                    needed.push(ctx, activation.program)?;
                }
                break;
            }
        }
        if needed.data.len() == pending
            && storage.discovered == ctx.code_roots.as_ref().unwrap().data.len()
        {
            break;
        }
    }
    for program in needed.data.into_iter().rev() {
        ctx.charge(storage.activations.data.len() as u64 + 1)?;
        let index = storage
            .activations
            .data
            .iter()
            .position(|a| a.program == program)
            .unwrap();
        let activation = storage.activations.data.remove(index);
        storage.activations.data.push(activation);
    }
    Ok(())
}

pub(super) fn advance(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    base: usize,
) -> Result<()> {
    while let Some(activation) = storage.activations.data.last() {
        let program = storage.programs.data[activation.program].program.clone();
        if let Some((frame, state)) = activation.waiting {
            if !storage.namespaces.data[state].initialized {
                if frames
                    .data
                    .get(frame)
                    .is_some_and(|frame| frame.activation && frame.program.index == program.index)
                {
                    return Ok(());
                }
                storage.programs.data[program.index].failed = true;
                storage.activations.data.pop();
                continue;
            }
            storage.activations.data.last_mut().unwrap().waiting = None;
        }
        let next = storage.activations.data.last().unwrap().next;
        if next == program.namespaces.len() {
            storage.activations.data.pop();
            continue;
        }
        ctx.charge(1)?;
        storage.activations.data.last_mut().unwrap().next += 1;
        let Some(body) = program.namespaces[next].body else {
            continue;
        };
        let entered = (|| {
            let state = namespaces::state(&program, ctx, storage, next)?;
            if !storage.namespaces.data[state].initialized {
                let frame = frames.data.len();
                enter_arguments(
                    &program,
                    ctx,
                    frames,
                    storage,
                    body,
                    Arguments::empty(),
                    base,
                )?;
                frames.data.last_mut().unwrap().activation = true;
                storage.activations.data.last_mut().unwrap().waiting = Some((frame, state));
                return Ok(true);
            }
            Ok(false)
        })();
        match entered {
            Ok(true) => return Ok(()),
            Ok(false) => (),
            Err(error) => {
                storage.programs.data[program.index].failed = true;
                storage.activations.data.pop();
                return Err(error);
            }
        }
    }
    Ok(())
}
