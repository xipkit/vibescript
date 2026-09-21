use super::*;
use crate::{budget::Charge, code::Code};
use std::{ops::Deref, sync::Arc};

mod snapshots;

pub(super) fn snapshot(ctx: &mut CallContext, storage: &Storage, value: &Value) -> Result<Value> {
    snapshots::snapshot(ctx, storage, value)
}

pub(super) fn snapshot_values(
    ctx: &mut CallContext,
    storage: &Storage,
    values: &mut [Value],
) -> Result<()> {
    snapshots::snapshot_values(ctx, storage, values)
}

pub(super) fn snapshot_arguments(
    ctx: &mut CallContext,
    storage: &Storage,
    positional: &mut [Value],
    keywords: &mut [(Value, Value)],
) -> Result<()> {
    let count = positional
        .len()
        .saturating_add(keywords.len().saturating_mul(2));
    let mut values = Buffer::with_capacity(ctx, count)?;
    values.extend(ctx, positional)?;
    for (key, value) in keywords.iter() {
        values.push(ctx, key.clone())?;
        values.push(ctx, value.clone())?;
    }
    snapshot_values(ctx, storage, &mut values.data)?;
    let mut values = values.data.into_iter();
    for value in positional {
        *value = values.next().unwrap();
    }
    for (key, value) in keywords {
        *key = values.next().unwrap();
        *value = values.next().unwrap();
    }
    Ok(())
}

pub(crate) struct Program {
    pub code: Arc<Code>,
    pub environment: Option<Arc<crate::objects::Instance>>,
    pub index: usize,
    pub global_base: usize,
    global_len: usize,
    _charge: Option<Charge>,
}

pub(super) struct Entry {
    pub program: Arc<Program>,
    failed: bool,
    release: bool,
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

impl Program {
    fn matches(
        &self,
        code: &Arc<Code>,
        environment: Option<&Arc<crate::objects::Instance>>,
    ) -> bool {
        Arc::ptr_eq(&self.code, code)
            && crate::namespace::same_environment(self.environment.as_ref(), environment)
    }

    pub(super) fn namespace_matches(
        &self,
        index: usize,
        namespace: &crate::namespace::Namespace,
    ) -> bool {
        self.namespaces
            .get(index)
            .is_some_and(|definition| Arc::ptr_eq(definition, &namespace.definition))
            && crate::namespace::same_environment(
                self.environment.as_ref(),
                namespace.environment.as_ref(),
            )
    }
}

pub(super) fn load(
    ctx: &mut CallContext,
    storage: &mut Storage,
    code: &Arc<Code>,
    environment: Option<&Arc<crate::objects::Instance>>,
) -> Result<(Arc<Program>, bool)> {
    if let Some(index) = registered(ctx, storage, code, environment)? {
        return pin(ctx, storage, index).map(|program| (program, false));
    }
    let mut vacant = None;
    for entry in storage.programs.data.iter().skip(1) {
        ctx.charge(1)?;
        if Arc::strong_count(&entry.program) == 1
            && entry
                .program
                .environment
                .as_ref()
                .is_some_and(|environment| !environment.alive())
        {
            vacant = Some(entry.program.index);
            break;
        }
    }
    let previous = vacant.map(|index| &storage.programs.data[index].program);
    let (global_base, global_len) = previous
        .filter(|previous| previous.global_len >= code.program.globals.len())
        .map(|previous| (previous.global_base, previous.global_len))
        .unwrap_or((storage.globals.data.len(), code.program.globals.len()));
    let Some(end) = global_base.checked_add(global_len) else {
        return ctx.fail(ErrorKind::Memory, "allocation size overflow");
    };
    if vacant.is_none() {
        storage
            .programs
            .ensure(ctx, storage.programs.data.len() + 1)?;
    }
    storage.globals.ensure(ctx, end)?;
    let charge = ctx.reserve(size_of::<Program>() + 2 * size_of::<usize>())?;
    Code::retain(ctx, code)?;
    let environment = environment
        .map(|environment| crate::objects::import(ctx, environment))
        .transpose()?;
    let program = Arc::new(Program {
        code: code.clone(),
        environment,
        index: vacant.unwrap_or(storage.programs.data.len()),
        global_base,
        global_len,
        _charge: charge,
    });
    if let Some(index) = vacant {
        let previous = &storage.programs.data[index].program;
        ctx.charge(previous.global_len as u64)?;
        storage.globals.data[previous.global_base..previous.global_base + previous.global_len]
            .fill(None);
        // Reuse vacant slots without moving indices held by active frames and writes.
        for state in &mut storage.namespaces.data {
            ctx.charge(1)?;
            if state.program == index {
                state.program = usize::MAX;
                state.namespace = previous.environment.as_ref().unwrap().class().clone();
                state.backing = None;
            }
        }
    }
    storage
        .globals
        .data
        .resize(storage.globals.data.len().max(end), None);
    let release = program.index != 0 && program.file && program.environment.is_some();
    storage.releasing |= release;
    let entry = Entry {
        program: program.clone(),
        failed: false,
        release,
    };
    if let Some(index) = vacant {
        storage.programs.data[index] = entry;
    } else {
        storage.programs.data.push(entry);
    }
    Ok((program, true))
}

pub(super) fn pin(
    ctx: &mut CallContext,
    storage: &mut Storage,
    index: usize,
) -> Result<Arc<Program>> {
    let entry = &mut storage.programs.data[index];
    if let Some(environment) = &entry.program.environment {
        if !environment.rooted() {
            let environment = crate::objects::import(ctx, environment)?;
            if let Some(program) = Arc::get_mut(&mut entry.program) {
                program.environment = Some(environment);
            } else {
                let charge = ctx.reserve(size_of::<Program>() + 2 * size_of::<usize>())?;
                let previous = &entry.program;
                entry.program = Arc::new(Program {
                    code: previous.code.clone(),
                    environment: Some(environment),
                    index,
                    global_base: previous.global_base,
                    global_len: previous.global_len,
                    _charge: charge,
                });
            }
            entry.release = true;
            storage.releasing = true;
        }
    }
    Ok(entry.program.clone())
}

pub(super) fn defer_release(storage: &mut Storage, index: usize) {
    let entry = &mut storage.programs.data[index];
    if index != 0 && entry.program.file && entry.program.environment.is_some() {
        entry.release = true;
        storage.releasing = true;
    }
}

pub(super) fn release(ctx: &mut CallContext, storage: &mut Storage) -> Result<()> {
    storage.releasing = false;
    for entry in &mut storage.programs.data {
        ctx.charge(1)?;
        if !entry.release {
            continue;
        }
        let Some(program) = Arc::get_mut(&mut entry.program) else {
            storage.releasing = true;
            continue;
        };
        // File bindings live in the heap; these caches must not root abandoned scopes.
        let environment = program.environment.as_ref().unwrap();
        if environment.alive() {
            program.environment = Some(crate::objects::unroot(environment)?);
            for state in &mut storage.namespaces.data {
                ctx.charge(1)?;
                if state.program != program.index {
                    continue;
                }
                state.namespace = crate::namespace::Namespace::with_environment(
                    ctx,
                    &state.namespace,
                    program.environment.as_ref().unwrap().clone(),
                )?;
                state.backing = state
                    .backing
                    .as_ref()
                    .map(crate::objects::unroot)
                    .transpose()?;
            }
        }
        ctx.charge(storage.declarations.data.len() as u64)?;
        storage
            .declarations
            .data
            .retain(|((index, _), _)| *index != program.index);
        entry.release = false;
    }
    Ok(())
}

fn registered(
    ctx: &mut CallContext,
    storage: &Storage,
    code: &Arc<Code>,
    environment: Option<&Arc<crate::objects::Instance>>,
) -> Result<Option<usize>> {
    // The entry program has a fixed lookup covered by the executing operation.
    if let Some(entry) = storage.programs.data.first() {
        if entry.program.matches(code, environment) {
            return Ok(Some(0));
        }
    }
    for entry in storage.programs.data.iter().skip(1) {
        ctx.charge(1)?;
        let program = &entry.program;
        if program.matches(code, environment) {
            return Ok(Some(program.index));
        }
    }
    Ok(None)
}

pub(super) fn arguments(
    ctx: &mut CallContext,
    storage: &mut Storage,
    input: &Arguments,
) -> Result<()> {
    if ctx.scoped_sources {
        let mut values = Buffer::empty();
        for (_, value) in input.keywords.buffer.data.iter().rev() {
            values.push(ctx, value.clone())?;
        }
        for value in input.positional.data.iter().rev() {
            values.push(ctx, value.clone())?;
        }
        return admit(ctx, storage, values, 0);
    }
    let count = ctx.code_roots.as_ref().unwrap().data.len();
    // No callbacks have run yet; all retained sources belong to the accepted arguments.
    for index in (0..count).rev() {
        ctx.charge(1)?;
        let code = ctx.code_roots.as_ref().unwrap().data[index].clone();
        let (program, _) = load(ctx, storage, &code, None)?;
        if program.index != 0 {
            activate(ctx, storage, program.index)?;
        }
    }
    storage.discovered = count;
    Ok(())
}

pub(super) fn activate(ctx: &mut CallContext, storage: &mut Storage, program: usize) -> Result<()> {
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
        if registered(ctx, storage, &code, None)?.is_none() {
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
    let (program, _) = load(ctx, storage, owner, namespace.environment.as_ref())?;
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
    if !ctx.scoped_sources {
        discovered(ctx, storage)?;
    }
    ctx.charge(storage.activations.data.len() as u64)?;
    let pending = storage
        .activations
        .data
        .iter()
        .filter(|a| a.waiting.is_none())
        .count();
    if !ctx.scoped_sources
        && pending == 0
        && storage.discovered == ctx.code_roots.as_ref().unwrap().data.len()
    {
        return Ok(());
    }
    let mut values = Buffer::empty();
    values.push(ctx, value.clone())?;
    admit(ctx, storage, values, pending)
}

fn admit(
    ctx: &mut CallContext,
    storage: &mut Storage,
    mut values: Buffer<Value>,
    mut pending: usize,
) -> Result<()> {
    let mut instances: Buffer<Arc<crate::objects::Instance>> = Buffer::empty();
    let mut needed = Buffer::empty();
    while let Some(value) = values.data.pop() {
        ctx.charge(1)?;
        let source = match &value.0 {
            Kind::Function(function) => {
                values.push(ctx, Value(Kind::Instance(function.environment.clone())))?;
                Some((&function.code, Some(&function.environment)))
            }
            Kind::Namespace(namespace) => {
                if let Some(environment) = &namespace.environment {
                    values.push(ctx, Value(Kind::Instance(environment.clone())))?;
                }
                namespace
                    .owner
                    .as_ref()
                    .map(|owner| (owner, namespace.environment.as_ref()))
            }
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
                let namespace = instance.class();
                namespace
                    .owner
                    .as_ref()
                    .map(|owner| (owner, namespace.environment.as_ref()))
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
        let Some((owner, environment)) = source else {
            continue;
        };
        let (program, fresh) = load(ctx, storage, owner, environment)?;
        if fresh {
            activate(ctx, storage, program.index)?;
            pending += 1;
            if !ctx.scoped_sources {
                discovered(ctx, storage)?;
            }
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
        if !ctx.scoped_sources
            && needed.data.len() == pending
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

#[cfg(test)]
mod compilation_tests {
    use super::*;
    use crate::{CallOptions, ErrorClass, compilation::Meter, vm::handlers::SavedError};
    use std::cell::RefCell;

    #[test]
    fn compilation_errors_transfer_to_rescue_without_duplicate_storage_charges() {
        let program = Program {
            code: Code::compile("0", &Default::default()).unwrap(),
            environment: None,
            index: 0,
            global_base: 0,
            global_len: 0,
            _charge: None,
        };
        let message = "failure ".repeat(4096);
        let run = |ctx: &mut CallContext| {
            let work = Meter(RefCell::new(&mut *ctx));
            let error = Error::syntax(&work, 0, &message).in_required_file();
            let mut error = crate::source::parse_error("$", None, error, &work);
            for _ in 0..3 {
                let saved = SavedError::new(&program, ctx, &[], 0, error).unwrap();
                assert_eq!(saved.error.message, message);
                assert_eq!(saved.error.class(), Some(ErrorClass::Runtime));
                assert_eq!(
                    saved.error.diagnostic.as_ref().unwrap().code_frame,
                    "  --> line 1, column 1\n 1 | $\n   | ^"
                );
                error = saved.into_error(ctx).unwrap();
            }
            error
        };
        let mut context = CallContext::new(CallOptions::default());
        let error = run(&mut context);
        let peak = context.stats().peak_memory_bytes;
        assert!(peak < message.len() * 3 / 2);
        assert!(context.stats().retained_memory_bytes >= message.len());
        drop(error);
        assert_eq!(context.stats().retained_memory_bytes, 0);
        let mut options = CallOptions::default();
        options.limits.memory_bytes = Some(peak);
        let mut context = CallContext::new(options);
        drop(run(&mut context));
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }
}
