use super::*;
use crate::{exports::Function, hash::Hash, loading::Loader};

pub(super) struct Module {
    pub code: Arc<crate::code::Code>,
    pub exports: Value,
    pub loading: bool,
    alias: Option<Value>,
}

pub(super) fn abandon(storage: &mut Storage, index: usize) {
    let module = &mut storage.modules.data[index];
    module.loading = false;
    module.exports = Value::nil();
    module.alias = None;
    while storage
        .modules
        .data
        .last()
        .is_some_and(|module| !module.loading && matches!(module.exports.0, Kind::Nil))
    {
        storage.modules.data.pop();
    }
}

pub(super) fn get(
    ctx: &mut CallContext,
    storage: &mut Storage,
    name: &str,
) -> Result<Option<Value>> {
    if let Some(value) = storage
        .bindings
        .as_ref()
        .map(|bindings| crate::objects::field(ctx, bindings, name))
        .transpose()
        .map(Option::flatten)?
    {
        return Ok(Some(value));
    }
    let Some(value) = globals::import(ctx, name)? else {
        return Ok(None);
    };
    programs::imported(ctx, storage, &value)?;
    set(ctx, storage, name, &value)?;
    Ok(Some(value))
}

pub(super) fn contains(ctx: &mut CallContext, storage: &Storage, name: &str) -> Result<bool> {
    if let Some(bindings) = &storage.bindings {
        if crate::objects::field_slot(ctx, bindings, name)?.is_some() {
            return Ok(true);
        }
    }
    globals::contains(ctx, name)
}

pub(super) fn set(
    ctx: &mut CallContext,
    storage: &mut Storage,
    name: &str,
    value: &Value,
) -> Result<()> {
    if storage.bindings.is_none() {
        storage.bindings = Some(crate::objects::environment(ctx)?);
    }
    let bindings = storage.bindings.as_ref().unwrap();
    if let Some(field) = crate::objects::field_slot(ctx, bindings, name)? {
        address::refresh(
            ctx,
            address::Root::Environment(bindings.clone(), field),
            value,
            &mut storage.addresses.data,
            &[],
        )?;
    }
    crate::objects::set(ctx, bindings, name, value)
}

pub(super) fn root_bound(ctx: &mut CallContext, storage: &Storage, name: &str) -> Result<bool> {
    let root = &storage.programs.data[0].program;
    Ok(root.names.contains_key(name)
        || root.declaration_names.contains_key(name)
        || root.hosts.iter().any(|n| n == name)
        || Global::parse(name).is_some()
        || contains(ctx, storage, name)?)
}

#[inline]
pub(super) fn local(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    current: usize,
    relative: usize,
    absolute: usize,
) -> Result<bool> {
    // Without root bindings or host globals no name can shadow a local.
    if (storage.bindings.is_none() && ctx.options.globals.is_empty())
        || frames.data[current].program.file
        || storage.locals.data[absolute].is_some()
    {
        return Ok(false);
    }
    unbound_local(ctx, &frames.data[current], storage, relative)
}

#[inline(never)]
fn unbound_local(
    ctx: &mut CallContext,
    frame: &Frame,
    storage: &Storage,
    relative: usize,
) -> Result<bool> {
    let function = &frame.program.functions[frame.function.unwrap()];
    for param in &function.params {
        ctx.charge(1)?;
        if param.slot == relative {
            return Ok(false);
        }
    }
    contains(ctx, storage, &function.local_names[relative])
}

pub(super) fn address(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    current: usize,
    name: &str,
) -> Result<Option<Address>> {
    if !contains(ctx, storage, name)? || file_bindings::get(program, ctx, name)?.is_some() {
        return Ok(None);
    }
    let function = &program.functions[frames.data[current].function.unwrap()];
    if namespaces::constant(program, ctx, storage, function.namespace, name)?.is_some()
        || namespaces::ambient_slot(ctx, frames, storage, current, name)?.is_some()
    {
        return Ok(None);
    }
    // A module export is executable code rather than data a write can reach, so
    // the name is read like any other callable instead.
    if matches!(get(ctx, storage, name)?, Some(Value(Kind::Function(_)))) {
        return Ok(None);
    }
    let bindings = storage.bindings.as_ref().unwrap();
    crate::objects::address(ctx, bindings, name)
        .map(Address::in_environment)
        .map(Some)
}

fn alias(ctx: &mut CallContext, args: &Arguments) -> Result<Option<Value>> {
    let mut result = None;
    for (name, value) in &args.keywords.buffer.data {
        ctx.charge(1)?;
        if name.as_bytes() != Some(b"as") {
            return Err(Error::new(
                ErrorKind::Argument,
                "require: unknown keyword argument",
            ));
        }
        if !matches!(value.0, Kind::Bytes(_) | Kind::Symbol(_)) {
            return Err(Error::new(
                ErrorKind::Argument,
                "require: alias must be a string or symbol",
            ));
        }
        let bytes = value.require_bytes()?;
        ctx.work_bytes(bytes.len())?;
        let name = std::str::from_utf8(bytes)
            .map_err(|_| Error::new(ErrorKind::Argument, "require: invalid alias"))?
            .trim();
        let mut chars = name.chars();
        if !chars
            .next()
            .is_some_and(|c| c == '_' || crate::syntax::unicode::letter(c))
            || !chars.all(|c| {
                matches!(c, '_' | '?' | '!')
                    || crate::syntax::unicode::letter(c)
                    || crate::syntax::unicode::digit(c)
            })
            || crate::syntax::keyword(name)
        {
            return Err(Error::new(ErrorKind::Argument, "require: invalid alias"));
        }
        result = Some(ctx.bytes(name.as_bytes())?);
    }
    Ok(result)
}

fn check_alias(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    alias: Option<&Value>,
    exports: &Value,
) -> Result<()> {
    let Some(alias) = alias else { return Ok(()) };
    let name = std::str::from_utf8(alias.as_bytes().unwrap()).unwrap();
    let same = |ctx: &mut CallContext, value: &Value| -> Result<bool> {
        let (Kind::Hash(a), Kind::Hash(b)) = (&value.0, &exports.0) else {
            return Ok(false);
        };
        if Arc::ptr_eq(a, b) {
            return Ok(true);
        }
        // Heap storage rewrites function environments while preserving their identity.
        if a.object && b.object {
            for (_, entry) in &b.buffer.data {
                ctx.charge(1)?;
                if matches!(entry.0, Kind::Function(_)) {
                    return crate::ops::equal(ctx, value, exports, 0);
                }
            }
        }
        Ok(false)
    };
    if let Some(value) = get(ctx, storage, name)? {
        if !same(ctx, &value)? {
            return Err(Error::new(
                ErrorKind::Argument,
                "require: alias already defined",
            ));
        }
    } else if root_bound(ctx, storage, name)? {
        return Err(Error::new(
            ErrorKind::Argument,
            "require: alias already defined",
        ));
    }
    if let Some(frame) = frames.data.last() {
        let function = &frame.program.functions[frame.function.unwrap()];
        if let Some(slot) = function.local_names.iter().position(|n| n == name) {
            let slot = resolve_slot(ctx, frames, storage, frames.data.len() - 1, slot, false)?;
            if let Some(value) = &storage.locals.data[slot] {
                if !same(ctx, value)? {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "require: alias already defined",
                    ));
                }
            }
        }
        if let Some(value) = file_bindings::get(&frame.program, ctx, name)? {
            if !same(ctx, &value)? {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "require: alias already defined",
                ));
            }
        }
        if let Some(value) =
            namespaces::constant(&frame.program, ctx, storage, function.namespace, name)?
        {
            if !same(ctx, &value)? {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "require: alias already defined",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn start(
    program: &Program,
    loader: &Arc<Loader>,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    args: Arguments,
) -> Result<()> {
    if ctx.strict_effects && !ctx.options.allow_require {
        ctx.checkpoint()?;
        return Err(Error::new(
            ErrorKind::Runtime,
            "strict effects: require is disabled without CallOptions.allow_require",
        ));
    }
    if args.positional.data.len() != 1 {
        return Err(Error::new(
            ErrorKind::Argument,
            "require expects a single module name argument",
        ));
    }
    if args.block.is_some() {
        return Err(Error::new(
            ErrorKind::Argument,
            "require does not accept blocks",
        ));
    }
    let alias = alias(ctx, &args)?;
    let input = &args.positional.data[0];
    if !matches!(input.0, Kind::Bytes(_) | Kind::Symbol(_)) {
        return Err(Error::new(
            ErrorKind::Argument,
            "require expects a string or symbol module name",
        ));
    }
    let root = storage.programs.data[0].program.code.clone();
    let code = loader
        .load(
            ctx,
            &mut storage.pins,
            input.require_bytes()?,
            program.code.origin.as_ref(),
            &root,
        )
        .map_err(Error::in_required_file)?;
    for (index, module) in storage.modules.data.iter().enumerate() {
        ctx.charge(1)?;
        if module.code.origin == code.origin {
            if module.loading {
                let mut message = Buffer::empty();
                message.extend(ctx, b"require: circular dependency detected: ")?;
                for module in &storage.modules.data[index..] {
                    ctx.charge(1)?;
                    if module.loading {
                        message.extend(ctx, module.code.origin.as_ref().unwrap().name())?;
                        message.extend(ctx, b" -> ")?;
                    }
                }
                message.extend(ctx, code.origin.as_ref().unwrap().name())?;
                return Err(Error::new(
                    ErrorKind::Runtime,
                    String::from_utf8_lossy(&message.data),
                ));
            }
            if !matches!(module.exports.0, Kind::Nil) {
                let exports = module.exports.clone();
                check_alias(ctx, frames, storage, alias.as_ref(), &exports)?;
                if let Some(alias) = alias {
                    set(
                        ctx,
                        storage,
                        std::str::from_utf8(alias.as_bytes().unwrap()).unwrap(),
                        &exports,
                    )?;
                }
                stack.push(ctx, exports)?;
                return Ok(());
            }
        }
    }
    let environment = crate::objects::environment(ctx)?;
    ctx.scoped_sources = true;
    let (owner, _) = programs::load(ctx, storage, &code, Some(&environment))?;
    let mut exports = Hash::empty();
    exports.object = true;
    for (name, target) in &code.exports {
        ctx.charge(1)?;
        let value = match *target {
            crate::code::Export::Enum(index) => declaration_value(&owner, ctx, storage, index)?,
            crate::code::Export::Function(index) => Value(Kind::Function(Function::new(
                ctx,
                code.clone(),
                environment.clone(),
                index,
            )?)),
        };
        let key = ctx.bytes(name.as_bytes())?;
        exports.insert(ctx, key, value)?;
    }
    let exports = Value::from_hash(ctx, exports)?;
    check_alias(ctx, frames, storage, alias.as_ref(), &exports)?;
    let index = storage.modules.data.len();
    storage.modules.ensure(ctx, index + 1)?;
    enter_arguments(
        &owner,
        ctx,
        frames,
        storage,
        0,
        Arguments::empty(),
        stack.data.len(),
    )?;
    storage.modules.data.push(Module {
        code,
        exports,
        loading: true,
        alias,
    });
    frames.data.last_mut().unwrap().return_to = ReturnTo::Require(index);
    programs::activate(ctx, storage, owner.index)?;
    Ok(())
}

pub(super) fn complete(
    ctx: &mut CallContext,
    _frames: &Buffer<Frame>,
    storage: &mut Storage,
    index: usize,
) -> Result<Value> {
    let module = &mut storage.modules.data[index];
    module.loading = false;
    let value = module.exports.clone();
    let alias = module.alias.take();
    let Kind::Hash(exports) = &value.0 else {
        unreachable!()
    };
    for (key, value) in &exports.buffer.data {
        ctx.charge(1)?;
        let name = std::str::from_utf8(key.as_bytes().unwrap()).unwrap();
        if !root_bound(ctx, storage, name)? {
            set(ctx, storage, name, value)?;
        }
    }
    if let Some(alias) = alias {
        set(
            ctx,
            storage,
            std::str::from_utf8(alias.as_bytes().unwrap()).unwrap(),
            &value,
        )?;
    }
    Ok(value)
}

pub(super) fn invoke(
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    function: &Arc<Function>,
    args: Arguments,
    auto: bool,
    base: usize,
) -> Result<()> {
    let (owner, _) = programs::load(ctx, storage, &function.code, Some(&function.environment))?;
    if auto {
        enter_auto(&owner, ctx, frames, storage, function.index, base)
    } else {
        enter_arguments(&owner, ctx, frames, storage, function.index, args, base)
    }
}
