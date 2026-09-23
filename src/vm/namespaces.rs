use super::*;
use crate::{
    members::{names::candidates::UNIVERSAL, suggest::Suggestion},
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

pub(super) fn call_constant(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: Option<usize>,
    instance: bool,
    name: &str,
) -> Result<Option<Value>> {
    // Explicit calls retain declared script-function dispatch, even when a
    // namespace constant has the same name as that function.
    if program.names.contains_key(name) {
        return Ok(None);
    }
    let value = constant(program, ctx, storage, module, name)?;
    if value.is_some() {
        let definition = &program.namespaces[module.unwrap()];
        let methods = if instance {
            &definition.instance_methods
        } else {
            &definition.methods
        };
        for method in methods {
            ctx.charge(1)?;
            ctx.work_bytes(name.len().max(method.name.len()))?;
            // Named calls keep method dispatch when a constant shares its name.
            if method.name == name {
                return Ok(None);
            }
        }
    }
    Ok(value)
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

/// Accepts a universal helper, which the generic member dispatch answers for
/// every receiver; any other name is missing from the class or instance.
pub(super) fn fallback(
    storage: &Storage,
    receiver: &Value,
    name: &str,
    implicit: bool,
) -> Result<()> {
    if crate::members::names::universal(name) {
        Ok(())
    } else {
        Err(missing(storage, receiver, name, implicit))
    }
}

/// Refuses a private or protected method by name, as Go does.
pub(super) fn hidden(visibility: Visibility, name: &str) -> Error {
    let visibility = if visibility == Visibility::Private {
        "private"
    } else {
        "protected"
    };
    Error::new(ErrorKind::Name, format!("{visibility} method {name}"))
}

/// Explains a name that used to construct a callable value, as Go does.
pub(super) fn removed(name: &str) -> Option<Error> {
    let constructor = match name {
        "proc" | "lambda" => name,
        "Proc" => "Proc.new",
        _ => return None,
    };
    Some(Error::new(
        ErrorKind::Name,
        format!(
            "{constructor} was removed; executable code is not a value. Define a named function and call it, or attach a block to the call that runs it"
        ),
    ))
}

/// Reports a name missing from a class or instance as the reference does.
/// Private and protected methods are suggested only to implicit calls, which
/// could reach them. Nothing here charges the call: it only renders.
pub(super) fn missing(storage: &Storage, receiver: &Value, name: &str, implicit: bool) -> Error {
    match &receiver.0 {
        Kind::Namespace(namespace) => class_missing(storage, &namespace.definition, name, implicit),
        Kind::Instance(instance) => instance_missing(instance, name, implicit),
        _ => Error::new(ErrorKind::Name, format!("unknown member {name}")),
    }
}

/// Reports a bare name missing from the current class context, whose receiver
/// is the running instance or class when one is bound. A removed callable
/// constructor keeps its teaching message.
pub(super) fn missing_implicit(
    storage: &Storage,
    receiver: Option<&Value>,
    definition: &Arc<crate::namespace::Definition>,
    name: &str,
) -> Error {
    if let Some(error) = removed(name) {
        return error;
    }
    match receiver {
        Some(receiver @ Value(Kind::Namespace(_) | Kind::Instance(_))) => {
            missing(storage, receiver, name, true)
        }
        _ => class_missing(storage, definition, name, true),
    }
}

/// Reports a scoped constant a class or module does not define, suggesting its
/// fields.
fn unknown_constant(
    storage: &Storage,
    definition: &Arc<crate::namespace::Definition>,
    name: &str,
) -> Error {
    let mut suggestion = Suggestion::new(name);
    let mut render = |fields: &mut dyn Iterator<Item = &[u8]>| {
        suggestion.offer(fields);
        format!("unknown constant {}::{name}{suggestion}", definition.name)
    };
    Error::new(
        ErrorKind::Name,
        with_class_fields(storage, definition, &mut render)
            .unwrap_or_else(|| format!("unknown constant {}::{name}", definition.name)),
    )
}

/// Renders with a class's field names without charging the call.
fn with_class_fields<R>(
    storage: &Storage,
    definition: &Arc<crate::namespace::Definition>,
    render: &mut dyn FnMut(&mut dyn Iterator<Item = &[u8]>) -> R,
) -> Option<R> {
    let state = storage
        .namespaces
        .data
        .iter()
        .find(|state| Arc::ptr_eq(&state.namespace.definition, definition));
    match state {
        Some(State {
            backing: Some(backing),
            ..
        }) => crate::objects::with_field_names(backing, render).ok(),
        Some(state) => Some(render(
            &mut state
                .fields
                .buffer
                .data
                .iter()
                .filter_map(|(key, _)| key.as_bytes()),
        )),
        None => Some(render(&mut std::iter::empty())),
    }
}

/// Method names a caller could reach: implicit calls also reach private and
/// protected methods.
fn accessible(methods: &[crate::namespace::Method], implicit: bool) -> impl Iterator<Item = &[u8]> {
    methods
        .iter()
        .filter(move |method| implicit || matches!(method.visibility, Visibility::Public))
        .map(|method| method.name.as_bytes())
}

/// An instance suggests `class`, its methods, fields and the universal helpers.
fn instance_missing(instance: &Arc<crate::objects::Instance>, name: &str, implicit: bool) -> Error {
    let class = instance.class();
    let mut suggestion = Suggestion::new(name);
    suggestion
        .offer([&b"class"[..]])
        .offer(accessible(&class.definition.instance_methods, implicit));
    let rendered = crate::objects::with_field_names(instance, |fields| {
        suggestion
            .offer(fields)
            .offer(UNIVERSAL.iter().map(|name| name.as_bytes()));
        format!("unknown member {name}{suggestion}")
    });
    Error::new(
        ErrorKind::Name,
        rendered.unwrap_or_else(|_| format!("unknown member {name}")),
    )
}

/// A class suggests `new`, its class methods, class fields and the universal
/// helpers, unless the name is a Ruby class macro spelled differently here.
fn class_missing(
    storage: &Storage,
    definition: &Arc<crate::namespace::Definition>,
    name: &str,
    implicit: bool,
) -> Error {
    let alternative = match name {
        "attr_accessor" => Some("use \"property x\" for a reader and writer"),
        "attr_reader" => Some("use \"getter x\""),
        "attr_writer" => Some("use \"setter x\""),
        _ => None,
    };
    if let Some(alternative) = alternative {
        return Error::new(
            ErrorKind::Name,
            format!("unknown class member {name} ({alternative}; the name is bare, not a symbol)"),
        );
    }
    let mut suggestion = Suggestion::new(name);
    suggestion
        .offer(definition.constructor.map(|_| &b"new"[..]))
        .offer(accessible(&definition.methods, implicit));
    let mut render = |fields: &mut dyn Iterator<Item = &[u8]>| {
        suggestion
            .offer(fields)
            .offer(UNIVERSAL.iter().map(|name| name.as_bytes()));
        format!("unknown class member {name}{suggestion}")
    };
    Error::new(
        ErrorKind::Name,
        with_class_fields(storage, definition, &mut render)
            .unwrap_or_else(|| format!("unknown class member {name}")),
    )
}

pub(super) fn state(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
) -> Result<usize> {
    let definition = &program.namespaces[module];
    let mut vacant = None;
    for (index, state) in storage.namespaces.data.iter().enumerate() {
        ctx.charge(1)?;
        if state.program == usize::MAX {
            vacant.get_or_insert(index);
        }
        if state.program == program.index && Arc::ptr_eq(&state.namespace.definition, definition) {
            return Ok(index);
        }
    }
    let mut namespace = Namespace::import(ctx, &Namespace::untracked(definition.clone()))?;
    let captured = if let Some(environment) = &program.environment {
        namespace = Namespace::with_environment(ctx, &namespace, environment.clone())?;
        Some(scopes::namespace(ctx, environment, module)?)
    } else {
        None
    };
    let fresh = captured.as_ref().is_none_or(|state| state.fresh);
    let initialized =
        definition.body.is_none() || captured.as_ref().is_some_and(|state| state.initialized);
    let value = State {
        program: program.index,
        namespace,
        fields: Hash::empty(),
        backing: captured.map(|state| state.fields),
        initialized,
    };
    let index = if let Some(index) = vacant {
        storage.namespaces.data[index] = value;
        index
    } else {
        let index = storage.namespaces.data.len();
        storage.namespaces.push(ctx, value)?;
        index
    };
    if fresh {
        for (name, nested) in &definition.nested {
            let nested = state(program, ctx, storage, *nested)?;
            let value = Value(Kind::Namespace(
                storage.namespaces.data[nested].namespace.clone(),
            ));
            if let Some(backing) = &storage.namespaces.data[index].backing {
                crate::objects::set(ctx, backing, name, &value)?;
            } else {
                let key = ctx.bytes(name.as_bytes())?;
                storage.namespaces.data[index]
                    .fields
                    .insert_field(ctx, key, value)?;
            }
        }
    }
    Ok(index)
}

pub(super) fn initialized(
    ctx: &mut CallContext,
    storage: &mut Storage,
    index: usize,
) -> Result<()> {
    let state = &mut storage.namespaces.data[index];
    if let Some(environment) = &state.namespace.environment {
        scopes::initialized(ctx, environment, state.namespace.definition.index)?;
    }
    state.initialized = true;
    Ok(())
}

pub(super) fn value(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
) -> Result<Value> {
    let index = state(program, ctx, storage, module)?;
    Namespace::import(ctx, &storage.namespaces.data[index].namespace)
        .map(|namespace| Value(Kind::Namespace(namespace)))
}

pub(super) fn field(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    module: usize,
    name: &str,
) -> Result<Option<Value>> {
    let index = state(program, ctx, storage, module)?;
    if let Some(backing) = &storage.namespaces.data[index].backing {
        return crate::objects::field(ctx, backing, name);
    }
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
    if let Some(backing) = &storage.namespaces.data[index].backing {
        if let Some(field) = crate::objects::field_slot(ctx, backing, name)? {
            address::refresh(
                ctx,
                address::Root::Environment(backing.clone(), field),
                &value,
                &mut storage.addresses.data,
                &[],
            )?;
        }
        return crate::objects::set(ctx, backing, name, &value);
    }
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
        .insert_field(ctx, key, value)
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
    if let Some(backing) = &storage.namespaces.data[index].backing {
        if !optional && crate::objects::field_slot(ctx, backing, name)?.is_none() {
            return Err(Error::new(ErrorKind::Name, "undefined class constant"));
        }
        return crate::objects::address(ctx, backing, name).map(Address::in_environment);
    }
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
            .insert_field(ctx, key, Value::nil())?;
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
            return Err(Error::new(ErrorKind::Type, crate::members::SCOPED_ONLY));
        }
        return match field(program, ctx, storage, definition.index, name)? {
            Some(value) => Ok(Member::Value(value)),
            None => Err(unknown_constant(storage, definition, name)),
        };
    }
    if instance.is_some() && name == "class" {
        return Ok(Member::Value(Value(Kind::Namespace(
            crate::namespace::Namespace::import(ctx, namespace)?,
        ))));
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
                return Err(hidden(method.visibility, name));
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
            format!("module {} cannot be instantiated", definition.name),
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
                    && (!caller.is_some_and(|index| program.namespace_matches(index, namespace))
                        || caller_instance != instance))
            {
                return Err(hidden(method.visibility, &method.name));
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
            format!("cannot assign to read-only property {name}"),
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
