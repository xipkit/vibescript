use super::*;
use crate::{
    namespace::{Namespace, State},
    syntax::modules::Visibility,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct Access {
    pub program: usize,
    pub caller: Option<usize>,
    pub implicit: bool,
    pub instance: bool,
}

pub(super) enum Member {
    Value(Value),
    Function(crate::namespace::Call),
    Helper(Value, crate::namespace::Helper),
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
    receiver: Option<&Value>,
    name: &str,
) -> Result<Member> {
    let Some(module) = module else {
        return Ok(Member::Missing);
    };
    let value = if let Some(receiver) = receiver {
        receiver.clone()
    } else {
        value(program, ctx, storage, module)?
    };
    let site = crate::bytecode::CallSite {
        name: 0,
        method: None,
        auto: true,
        parenthesized: false,
        scope: false,
    };
    member(
        ctx,
        storage,
        &value,
        site,
        name,
        Access {
            program: program.index,
            caller: Some(module),
            implicit: true,
            instance: matches!(value.0, Kind::Instance(_)),
        },
    )
}

pub(super) fn fallback(name: &str) -> Result<()> {
    if matches!(
        name,
        "nil?"
            | "itself"
            | "dup"
            | "clone"
            | "freeze"
            | "frozen?"
            | "tap"
            | "yield_self"
            | "eql?"
            | "equal?"
            | "respond_to?"
            | "is_a?"
            | "kind_of?"
            | "instance_of?"
            | "is_type?"
            | "send"
            | "public_send"
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
    let definition = &program.namespaces[module];
    for (index, state) in storage.namespaces.data.iter().enumerate() {
        ctx.charge(1)?;
        if state.program == program.index && Arc::ptr_eq(&state.namespace.definition, definition) {
            return Ok(index);
        }
    }
    let namespace = Namespace::import(ctx, &Namespace::untracked(definition.clone()))?;
    let index = storage.namespaces.data.len();
    storage.namespaces.push(
        ctx,
        State {
            program: program.index,
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
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: &Value,
    site: crate::bytecode::CallSite,
    name: &str,
    access: Access,
) -> Result<Member> {
    let (namespace, instance) = match &receiver.0 {
        Kind::Namespace(namespace) => (namespace, None),
        Kind::Instance(instance) => (instance.class(), Some(instance)),
        _ => unreachable!(),
    };
    let definition = &namespace.definition;
    let owner = programs::namespace(ctx, storage, namespace)?;
    let program = &*owner;
    if site.scope {
        if instance.is_some() {
            return Err(Error::new(
                ErrorKind::Type,
                "scoped member access requires a namespace",
            ));
        }
        return field(program, ctx, storage, definition.index, name)?
            .map(Member::Value)
            .ok_or_else(|| Error::new(ErrorKind::Name, "unknown class constant"));
    }
    if instance.is_some() && name == "class" {
        return Ok(Member::Value(Value(Kind::Namespace(namespace.clone()))));
    }
    if instance.is_none() && name == "new" {
        if let Some((function, accepts_arguments)) = definition.constructor {
            return Ok(Member::Function(crate::namespace::Call {
                function,
                receiver: Some(receiver.clone()),
                constructor: true,
                ignore_arguments: !accepts_arguments,
            }));
        }
    }
    let methods = if instance.is_some() {
        &definition.instance_methods
    } else {
        &definition.methods
    };
    for method in methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name == name {
            let allowed = match method.visibility {
                Visibility::Public => true,
                Visibility::Private => access.implicit,
                Visibility::Protected => {
                    access.implicit
                        || (access.program == program.index
                            && access.caller == Some(definition.index)
                            && access.instance == instance.is_some())
                }
            };
            if !allowed {
                return Err(Error::new(
                    ErrorKind::Name,
                    "method is not accessible with this receiver",
                ));
            }
            return Ok(Member::Function(crate::namespace::Call {
                receiver: Some(receiver.clone()),
                ..method.function.into()
            }));
        }
    }
    let helper = match name {
        "eql?" | "equal?" => Some(crate::namespace::Helper::Equality(name == "eql?")),
        _ => crate::members::introspection::Predicate::parse(name)
            .map(|predicate| crate::namespace::Helper::Predicate(predicate, access.implicit)),
    };
    if let Some(helper) = helper {
        return Ok(Member::Helper(receiver.clone(), helper));
    }
    if !matches!(
        name,
        "nil?" | "itself" | "dup" | "clone" | "freeze" | "frozen?" | "send" | "public_send"
    ) {
        let value = if let Some(instance) = instance {
            crate::objects::field(ctx, instance, name)?
        } else {
            field(program, ctx, storage, definition.index, name)?
        };
        if let Some(value) = value {
            return Ok(Member::Value(value));
        }
    }
    if name == "new" && instance.is_none() {
        return Err(Error::new(
            ErrorKind::Argument,
            "modules cannot be instantiated",
        ));
    }
    Ok(Member::Missing)
}

pub(super) fn ambient_slot(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &Storage,
    mut current: usize,
    name: &str,
) -> Result<Option<usize>> {
    if !frames.data[current].function.is_some_and(|f| {
        frames.data[current].program.functions[f]
            .namespace
            .is_some()
    }) {
        return Ok(None);
    }
    loop {
        ctx.charge(1)?;
        let frame = &frames.data[current];
        if frame
            .function
            .is_some_and(|f| frame.program.functions[f].initializer)
        {
            let Some(parent) = frame.parent else {
                return Ok(None);
            };
            let parent = &frames.data[parent];
            if parent.program.index != frame.program.index {
                return Ok(None);
            }
            let Some(function) = parent.function else {
                return Ok(None);
            };
            for (slot, candidate) in parent.program.functions[function]
                .local_names
                .iter()
                .enumerate()
            {
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
    storage: &mut Storage,
    receiver: &Value,
    name: &str,
    caller: Option<usize>,
    caller_instance: bool,
) -> Result<Option<crate::namespace::Call>> {
    let (namespace, instance) = match &receiver.0 {
        Kind::Namespace(namespace) => (namespace, false),
        Kind::Instance(instance) => (instance.class(), true),
        _ => unreachable!(),
    };
    let _owner = programs::namespace(ctx, storage, namespace)?;
    let methods = if instance {
        &namespace.definition.instance_methods
    } else {
        &namespace.definition.methods
    };
    for method in methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name.strip_suffix('=') == Some(name) {
            if method.visibility == Visibility::Private
                || (method.visibility == Visibility::Protected
                    && (!caller
                        .and_then(|index| program.namespaces.get(index))
                        .is_some_and(|definition| Arc::ptr_eq(definition, &namespace.definition))
                        || caller_instance != instance))
            {
                return Err(Error::new(
                    ErrorKind::Name,
                    "setter is not accessible with this receiver",
                ));
            }
            return Ok(Some(crate::namespace::Call {
                receiver: Some(receiver.clone()),
                ..method.function.into()
            }));
        }
    }
    if instance && methods.iter().any(|method| method.name == name) {
        return Err(Error::new(
            ErrorKind::Argument,
            "cannot assign to read-only property",
        ));
    }
    Ok(None)
}

pub(super) fn call_helper(
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: Value,
    helper: crate::namespace::Helper,
    args: &Arguments,
    auto: bool,
) -> Result<Value> {
    use crate::namespace::Helper;
    if let Helper::Equality(strict) = helper {
        return crate::members::equality::invoke(
            ctx,
            auto,
            if strict { "eql?" } else { "equal?" },
            &receiver,
            &args.positional.data,
            !args.keywords.buffer.data.is_empty(),
            args.block.is_some(),
        );
    }
    let Helper::Predicate(predicate, caller) = helper else {
        unreachable!()
    };
    use crate::members::introspection::{self, Query};
    let query = predicate.validate(
        ctx,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )?;
    let result = match query {
        Query::Class(class) => introspection::belongs(&receiver, class),
        Query::Respond(name, private) => {
            responds(ctx, storage, &receiver, name, caller || private)?
        }
        Query::Type(_) => {
            return Err(Error::new(
                ErrorKind::Type,
                "type predicate requires an execution context",
            ));
        }
    };
    Ok(Value::boolean(result))
}

fn responds(
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: &Value,
    name: &[u8],
    private: bool,
) -> Result<bool> {
    let Some(text) = crate::members::introspection::method_name(ctx, name)? else {
        return Ok(false);
    };
    let (namespace, instance) = match &receiver.0 {
        Kind::Namespace(namespace) => (namespace, None),
        Kind::Instance(instance) => (instance.class(), Some(instance)),
        _ => unreachable!(),
    };
    let owner = programs::namespace(ctx, storage, namespace)?;
    let program = &*owner;
    let module = namespace.definition.index;
    if (instance.is_some() && name == b"class")
        || (instance.is_none() && namespace.definition.constructor.is_some() && name == b"new")
    {
        return Ok(true);
    }
    let methods = if instance.is_some() {
        &namespace.definition.instance_methods
    } else {
        &namespace.definition.methods
    };
    for method in methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name.as_bytes() == name {
            return Ok(private || method.visibility == Visibility::Public);
        }
    }
    let name = text;
    if !crate::members::names::universal(name) {
        return Ok(false);
    }
    if matches!(name, "tap" | "yield_self") {
        let value = if let Some(instance) = instance {
            crate::objects::field(ctx, instance, name)?
        } else {
            field(program, ctx, storage, module, name)?
        };
        return Ok(value
            .as_ref()
            .is_none_or(crate::members::introspection::callable));
    }
    Ok(true)
}
