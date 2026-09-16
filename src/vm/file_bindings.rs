use super::*;

pub(super) fn environment(program: &Program) -> Option<&Arc<crate::objects::Instance>> {
    program
        .file
        .then_some(program.environment.as_ref())
        .flatten()
}

pub(super) fn get(program: &Program, ctx: &mut CallContext, name: &str) -> Result<Option<Value>> {
    match environment(program) {
        Some(environment) => crate::objects::field(ctx, environment, name),
        None => Ok(None),
    }
}

pub(super) fn set(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    name: &str,
    value: &Value,
) -> Result<()> {
    let environment = environment(program).unwrap();
    if let Some(field) = crate::objects::field_slot(ctx, environment, name)? {
        address::refresh(
            ctx,
            address::Root::Environment(environment.clone(), field),
            value,
            &mut storage.addresses.data,
            &[],
        )?;
    }
    crate::objects::set(ctx, environment, name, value)
}

pub(super) fn address(program: &Program, ctx: &mut CallContext, name: &str) -> Result<Address> {
    crate::objects::address(ctx, environment(program).unwrap(), name).map(Address::in_environment)
}

pub(super) fn declare(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    name: &str,
) -> Result<()> {
    if get(program, ctx, name)?.is_none()
        && !program.names.contains_key(name)
        && !program.declaration_names.contains_key(name)
        && !requires::root_bound(ctx, storage, name)?
    {
        set(program, ctx, storage, name, &Value::nil())?;
    }
    Ok(())
}

pub(super) fn local(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    current: usize,
    relative: usize,
    absolute: usize,
) -> Result<bool> {
    let Some(environment) = environment(program) else {
        return Ok(false);
    };
    let frame = &frames.data[current];
    let function = &program.functions[frame.function.unwrap()];
    let name = &function.local_names[relative];
    if name.starts_with('\0') || storage.locals.data[absolute].is_some() {
        return Ok(false);
    }
    for param in &function.params {
        ctx.charge(1)?;
        if param.slot == relative {
            return Ok(false);
        }
    }
    if frame.function == Some(0) {
        return Ok(true);
    }
    if function.name == "<block>" {
        let mut parent = frame.parent;
        while let Some(index) = parent {
            ctx.charge(1)?;
            let parent_frame = &frames.data[index];
            let parent_function = &parent_frame.program.functions[parent_frame.function.unwrap()];
            if parent_function.initializer {
                return Ok(false);
            }
            if parent_function.name != "<block>" {
                break;
            }
            parent = parent_frame.parent;
        }
    }
    Ok(
        crate::objects::field_slot(ctx, environment, name)?.is_some()
            || program.names.contains_key(name)
            || program.declaration_names.contains_key(name)
            || requires::root_bound(ctx, storage, name)?,
    )
}

pub(super) fn local_name(op: Op) -> Option<usize> {
    match op {
        Op::Load(slot)
        | Op::LoadOptional(slot, _)
        | Op::ReceiverBound(slot, _)
        | Op::Declare(slot)
        | Op::Store(slot)
        | Op::AddStore(slot)
        | Op::AddressLocal(slot)
        | Op::AddressBound(slot, _) => Some(slot),
        _ => None,
    }
}

pub(super) fn unshadowed(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    current: usize,
    name: &str,
) -> Result<bool> {
    let function = &program.functions[frames.data[current].function.unwrap()];
    for (relative, candidate) in function.local_names.iter().enumerate() {
        ctx.charge(1)?;
        if candidate == name {
            let absolute = resolve_slot(ctx, frames, storage, current, relative, false)?;
            return local(program, ctx, frames, storage, current, relative, absolute);
        }
    }
    if namespaces::constant(program, ctx, storage, function.namespace, name)?.is_some() {
        return Ok(false);
    }
    Ok(namespaces::ambient_slot(ctx, frames, storage, current, name)?.is_none())
}

pub(super) fn global_slot(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    index: usize,
) -> Result<usize> {
    let (global, original) = &program.globals[index];
    let root = &storage.programs.data[0].program;
    if let Some(index) = global_index(root, global.name()) {
        let slot = root.global_base + index;
        if storage.globals.data[slot].is_none() {
            storage.globals.data[slot] = Some(ctx.import(&root.globals[index].1)?);
        }
        return Ok(slot);
    }
    for (candidate, slot) in &storage.ambient_globals.data {
        ctx.charge(1)?;
        if candidate == global {
            return Ok(*slot);
        }
    }
    let value = ctx.import(original)?;
    let slot = storage.globals.data.len();
    storage
        .ambient_globals
        .ensure(ctx, storage.ambient_globals.data.len() + 1)?;
    storage.globals.push(ctx, Some(value))?;
    storage.ambient_globals.data.push((*global, slot));
    Ok(slot)
}

pub(super) fn global_address(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    index: usize,
    read: bool,
) -> Result<Address> {
    let name = program.globals[index].0.name();
    if environment(program).is_some() {
        if get(program, ctx, name)?.is_some() {
            return address(program, ctx, name);
        }
        let slot = global_slot(program, ctx, storage, index)?;
        let mut value = storage.globals.data[slot].as_ref().unwrap().clone();
        if read {
            if let (Kind::Builtin(current), Kind::Builtin(original)) =
                (&value.0, &program.globals[index].1.0)
            {
                if current == original {
                    value = current.read(ctx)?;
                    return Ok(Address::new(None, value));
                }
            }
        }
        return Ok(Address::global(slot, value));
    }
    if requires::get(ctx, storage, name)?.is_some() {
        return crate::objects::address(ctx, storage.bindings.as_ref().unwrap(), name)
            .map(Address::in_environment);
    }
    let mut value = global_value(program, ctx, storage, index)?;
    if read {
        if let (Kind::Builtin(current), Kind::Builtin(original)) =
            (&value.0, &program.globals[index].1.0)
        {
            if current == original {
                value = current.read(ctx)?;
                return Ok(Address::new(None, value));
            }
        }
    }
    Ok(Address::global(program.global_base + index, value))
}

pub(super) fn declaration_name(program: &Program, index: usize) -> &str {
    match &program.declarations[index].0 {
        Kind::Namespace(namespace) => &namespace.definition.name,
        Kind::Enum(enumeration) => &enumeration.definition.name,
        _ => unreachable!(),
    }
}

pub(super) enum RootBinding {
    Value(Value),
    Function(Arc<Program>, usize),
    Host(Arc<Program>, usize),
}

impl RootBinding {
    pub fn target(self) -> crate::arguments::Target {
        match self {
            Self::Value(value) => value_invocation(&value),
            Self::Function(owner, function) => crate::arguments::Target::Function(owner, function),
            Self::Host(owner, host) => crate::arguments::Target::Host(owner, host),
        }
    }
}

pub(super) fn root_binding(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    name: &str,
) -> Result<Option<RootBinding>> {
    if let Some(value) = requires::get(ctx, storage, name)? {
        return Ok(Some(RootBinding::Value(value)));
    }
    if program.file && program.index != 0 {
        let root = storage.programs.data[0].program.clone();
        if let Some(&index) = root.declaration_names.get(name) {
            return Ok(Some(RootBinding::Value(declaration_value(
                &root, ctx, storage, index,
            )?)));
        }
        if let Some(&function) = root.names.get(name) {
            return Ok(Some(RootBinding::Function(root, function)));
        }
        if let Some(host) = root.hosts.iter().position(|host| host == name) {
            return Ok(Some(RootBinding::Host(root, host)));
        }
    }
    Ok(None)
}

pub(super) fn read_root(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    binding: RootBinding,
) -> Result<()> {
    match binding {
        RootBinding::Value(Value(Kind::Function(function))) => requires::invoke(
            ctx,
            frames,
            storage,
            &function,
            Arguments::empty(),
            true,
            stack.data.len(),
        ),
        RootBinding::Value(value) => {
            let value = match value.0 {
                Kind::Host(method) => return Err(method.value_error()),
                Kind::Builtin(builtin) => builtin.read(ctx)?,
                Kind::Offset(offset) => return Err(offset.value_error()),
                _ => value,
            };
            stack.push(ctx, value)
        }
        RootBinding::Function(owner, function) => {
            enter_auto(&owner, ctx, frames, storage, function, stack.data.len())
        }
        RootBinding::Host(owner, host) => Err(callable_value_error(&owner.hosts[host], "method")),
    }
}
