use super::*;
use crate::{
    namespace::{Namespace, State},
    syntax::modules::Visibility,
};
use std::sync::Arc;

pub(super) struct Access {
    pub caller: Option<usize>,
    pub implicit: bool,
}

pub(super) enum Member {
    Value(Value),
    Function(usize),
    Helper(usize, crate::namespace::Helper),
    Missing,
}

pub(super) fn constant(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: Option<usize>,
    name: &str,
) -> Result<Option<Value>> {
    if let Some(module) = module {
        if name
            .chars()
            .next()
            .is_some_and(crate::syntax::unicode::upper)
        {
            return field(program, ctx, storage, module, name);
        }
    }
    Ok(None)
}

pub(super) fn implicit(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: Option<usize>,
    name: &str,
) -> Result<Member> {
    let Some(module) = module else {
        return Ok(Member::Missing);
    };
    let value = value(program, ctx, storage, module)?;
    let Kind::Namespace(namespace) = value.0 else {
        unreachable!()
    };
    let site = crate::bytecode::CallSite {
        name: 0,
        method: None,
        auto: true,
        scope: false,
    };
    member(
        program,
        ctx,
        storage,
        &namespace,
        site,
        name,
        Access {
            caller: Some(module),
            implicit: true,
        },
    )
}

pub(super) fn fallback(name: &str) -> Result<()> {
    if matches!(
        name,
        "nil?"
            | "itself"
            | "dup"
            | "tap"
            | "yield_self"
            | "eql?"
            | "equal?"
            | "respond_to?"
            | "is_a?"
            | "kind_of?"
            | "instance_of?"
    ) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Name,
            format!("unknown class member {name}"),
        ))
    }
}

pub(super) fn state(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
) -> Result<usize> {
    for (index, state) in storage.namespaces.data.iter().enumerate() {
        ctx.charge(1)?;
        if state.namespace.definition.index == module {
            return Ok(index);
        }
    }
    let definition = &program.namespaces[module];
    let namespace = Namespace::import(ctx, &Namespace::untracked(definition.clone()))?;
    let index = storage.namespaces.data.len();
    storage.namespaces.push(
        ctx,
        State {
            namespace,
            fields: Hash::empty(),
            initialized: definition.body.is_none(),
        },
    )?;
    for (name, nested) in &definition.nested {
        let nested = state(program, ctx, storage, *nested)?;
        let value = Value(Kind::Namespace(
            storage.namespaces.data[nested].namespace.clone(),
        ));
        let key = ctx.bytes(name.as_bytes())?;
        storage.namespaces.data[index]
            .fields
            .insert(ctx, key, value)?;
    }
    Ok(index)
}

pub(super) fn value(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
) -> Result<Value> {
    let index = state(program, ctx, storage, module)?;
    Ok(Value(Kind::Namespace(
        storage.namespaces.data[index].namespace.clone(),
    )))
}

pub(super) fn field(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
    name: &str,
) -> Result<Option<Value>> {
    let index = state(program, ctx, storage, module)?;
    let fields = &storage.namespaces.data[index].fields;
    Ok(fields
        .find(ctx, name.as_bytes())?
        .map(|i| fields.buffer.data[i].1.clone()))
}

pub(super) fn set(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
    name: &str,
    value: Value,
) -> Result<()> {
    let index = state(program, ctx, storage, module)?;
    let key = ctx.bytes(name.as_bytes())?;
    if let Some(field) = storage.namespaces.data[index]
        .fields
        .find(ctx, name.as_bytes())?
    {
        address::refresh(
            ctx,
            address::Root::Field(index, field),
            &value,
            &mut storage.addresses.data,
            &[],
        )?;
    }
    storage.namespaces.data[index]
        .fields
        .insert(ctx, key, value)
}

pub(super) fn address(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
    name: &str,
    optional: bool,
) -> Result<Address> {
    let index = state(program, ctx, storage, module)?;
    let field = if let Some(field) = storage.namespaces.data[index]
        .fields
        .find(ctx, name.as_bytes())?
    {
        field
    } else if optional {
        let key = ctx.bytes(name.as_bytes())?;
        let field = storage.namespaces.data[index].fields.buffer.data.len();
        storage.namespaces.data[index]
            .fields
            .insert(ctx, key, Value::nil())?;
        field
    } else {
        return Err(Error::new(ErrorKind::Name, "undefined class constant"));
    };
    let value = storage.namespaces.data[index].fields.buffer.data[field]
        .1
        .clone();
    Ok(Address::field(index, field, value))
}

pub(super) fn variable_name(module: Option<usize>, name: &str) -> Result<(usize, &str)> {
    if name.starts_with('@') && !name.starts_with("@@") {
        return Err(Error::new(ErrorKind::Name, "no instance context for ivar"));
    }
    let module =
        module.ok_or_else(|| Error::new(ErrorKind::Name, "no class context for class var"))?;
    Ok((module, name.strip_prefix("@@").unwrap_or(name)))
}

pub(super) fn member(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    namespace: &Arc<Namespace>,
    site: crate::bytecode::CallSite,
    name: &str,
    access: Access,
) -> Result<Member> {
    let Access { caller, implicit } = access;
    let definition = &namespace.definition;
    if !program
        .namespaces
        .get(definition.index)
        .is_some_and(|current| Arc::ptr_eq(current, definition))
    {
        return Err(Error::new(
            ErrorKind::Type,
            "module belongs to a different compiled script",
        ));
    }
    if site.scope {
        return field(program, ctx, storage, definition.index, name)?
            .map(Member::Value)
            .ok_or_else(|| Error::new(ErrorKind::Name, "unknown class constant"));
    }
    for method in &definition.methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name == name {
            let allowed = match method.visibility {
                Visibility::Public => true,
                Visibility::Private => implicit,
                Visibility::Protected => caller == Some(definition.index),
            };
            if !allowed {
                return Err(Error::new(
                    ErrorKind::Name,
                    "method is not accessible with this receiver",
                ));
            }
            return Ok(Member::Function(method.function));
        }
    }
    let helper = match name {
        "eql?" | "equal?" => Some(crate::namespace::Helper::Equality),
        "is_a?" | "kind_of?" | "instance_of?" => Some(crate::namespace::Helper::Class),
        "respond_to?" => Some(crate::namespace::Helper::Respond(implicit)),
        _ => None,
    };
    if let Some(helper) = helper {
        return Ok(Member::Helper(definition.index, helper));
    }
    if !matches!(name, "nil?" | "itself" | "dup") {
        if let Some(value) = field(program, ctx, storage, definition.index, name)? {
            return Ok(Member::Value(value));
        }
    }
    if name == "new" {
        return Err(Error::new(
            ErrorKind::Argument,
            "modules cannot be instantiated",
        ));
    }
    Ok(Member::Missing)
}

pub(super) fn ambient_slot(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    mut current: usize,
    name: &str,
) -> Result<Option<usize>> {
    if !frames.data[current]
        .function
        .is_some_and(|f| program.functions[f].namespace.is_some())
    {
        return Ok(None);
    }
    loop {
        ctx.charge(1)?;
        let frame = &frames.data[current];
        if frame
            .function
            .is_some_and(|f| program.functions[f].initializer)
        {
            let Some(parent) = frame.parent else {
                return Ok(None);
            };
            let parent = &frames.data[parent];
            let Some(function) = parent.function else {
                return Ok(None);
            };
            for (slot, candidate) in program.functions[function].local_names.iter().enumerate() {
                ctx.charge(1)?;
                ctx.work_bytes(name.len().max(candidate.len()))?;
                let slot = parent.local_base + slot;
                if name == candidate && storage.locals.data[slot].is_some() {
                    return Ok(Some(slot));
                }
            }
            return Ok(None);
        }
        let Some(parent) = frame.parent else {
            return Ok(None);
        };
        current = parent;
    }
}

pub(super) fn setter(
    program: &Program,
    ctx: &mut CallContext,
    receiver: &Namespace,
    name: &str,
    caller: Option<usize>,
) -> Result<Option<usize>> {
    if !program
        .namespaces
        .get(receiver.definition.index)
        .is_some_and(|current| Arc::ptr_eq(current, &receiver.definition))
    {
        return Err(Error::new(
            ErrorKind::Type,
            "module belongs to a different compiled script",
        ));
    }
    for method in &receiver.definition.methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name.strip_suffix('=') == Some(name) {
            if method.visibility == Visibility::Private
                || (method.visibility == Visibility::Protected
                    && caller != Some(receiver.definition.index))
            {
                return Err(Error::new(
                    ErrorKind::Name,
                    "setter is not accessible with this receiver",
                ));
            }
            return Ok(Some(method.function));
        }
    }
    Ok(None)
}

pub(super) fn call_helper(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
    helper: crate::namespace::Helper,
    args: &Arguments,
) -> Result<Value> {
    use crate::namespace::Helper;
    if !args.keywords.buffer.data.is_empty() || args.block.is_some() {
        return Err(Error::new(
            ErrorKind::Argument,
            "module predicate does not take keywords or a block",
        ));
    }
    let count = args.positional.data.len();
    if count != 1 && !(matches!(helper, Helper::Respond(_)) && count == 2) {
        return Err(Error::new(
            ErrorKind::Argument,
            "invalid module predicate argument count",
        ));
    }
    let value = &args.positional.data[0];
    let result = match helper {
        Helper::Equality => {
            matches!(&value.0, Kind::Namespace(other) if Arc::ptr_eq(&other.definition, &program.namespaces[module]))
        }
        Helper::Class => {
            if !matches!(value.0, Kind::Namespace(_)) {
                return Err(Error::new(
                    ErrorKind::Type,
                    "class predicate expects a class argument",
                ));
            }
            false
        }
        Helper::Respond(caller) => {
            let include_private = match args.positional.data.get(1) {
                None => false,
                Some(Value(Kind::Bool(value))) => *value,
                _ => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "respond_to? expects a boolean second argument",
                    ));
                }
            };
            let name = value.require_bytes()?;
            responds(
                program,
                ctx,
                storage,
                module,
                name,
                caller || include_private,
            )?
        }
    };
    Ok(Value::boolean(result))
}

fn responds(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
    name: &[u8],
    private: bool,
) -> Result<bool> {
    for method in &program.namespaces[module].methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name.as_bytes() == name {
            return Ok(private || method.visibility == Visibility::Public);
        }
    }
    if matches!(
        name,
        b"nil?"
            | b"itself"
            | b"dup"
            | b"eql?"
            | b"equal?"
            | b"respond_to?"
            | b"is_a?"
            | b"kind_of?"
            | b"instance_of?"
    ) {
        return Ok(true);
    }
    if matches!(name, b"tap" | b"yield_self") {
        return Ok(field(
            program,
            ctx,
            storage,
            module,
            std::str::from_utf8(name).unwrap(),
        )?
        .is_none());
    }
    Ok(false)
}
