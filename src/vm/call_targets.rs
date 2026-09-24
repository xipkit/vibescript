use super::*;
use crate::{arguments::Target, bytecode::CallSite};

// Shared primitive methods need a receiver from direct member-call syntax;
// bound methods and stored fields carry their own call target through rescue.
use crate::members::names;

pub(super) fn identifier(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    current: usize,
    slot: usize,
    index: usize,
) -> Result<Target> {
    let name = &program.members[index];
    let frame = &frames.data[current];
    let namespace = frame
        .function
        .and_then(|f| frame.program.functions[f].namespace);
    if let Some(Some(value)) = storage.locals.data.get(slot) {
        return Ok(value_invocation(value));
    }
    if let Some(value) = namespaces::constant(program, ctx, storage, namespace, name)? {
        return Ok(value_invocation(&value));
    }
    if let Some(slot) = namespaces::ambient_slot(ctx, frames, storage, current, name)? {
        return Ok(value_invocation(
            storage.locals.data[slot].as_ref().unwrap(),
        ));
    }
    if let Some(value) = file_bindings::get(program, ctx, name)? {
        return Ok(value_invocation(&value));
    }
    if !program.file {
        if let Some(value) = globals::get(ctx, storage, name)? {
            return Ok(value_invocation(&value));
        }
    }
    if program.declaration_names.contains_key(name) {
        return Ok(Target::Plain(Invocation::NonCallable));
    }
    if let Some(&function) = program.names.get(name) {
        return Ok(Target::Plain(Invocation::Function(function)));
    }
    if let Some(host) = program.hosts.iter().position(|host| host == name) {
        return Ok(Target::Plain(Invocation::Host(host)));
    }
    if let Some(binding) = file_bindings::root_binding(program, ctx, storage, name)? {
        return Ok(binding.target());
    }
    if let Some(global) = global_index(program, name) {
        return Ok(value_invocation(&global_value(
            program, ctx, storage, global,
        )?));
    }
    match namespaces::implicit(
        program,
        ctx,
        storage,
        namespace,
        frame.receiver.as_ref(),
        name,
    )? {
        namespaces::Member::Function(call) => Ok(Target::Method(call)),
        namespaces::Member::Value(value) => Ok(value_invocation(&value)),
        namespaces::Member::Helper(receiver, helper)
            if matches!(name.as_str(), "eql?" | "equal?") =>
        {
            Ok(Target::Helper(receiver, helper))
        }
        namespaces::Member::Helper(_, _) => Ok(Target::Member(Value::nil(), index)),
        namespaces::Member::Missing => match namespace {
            Some(module) if !names::universal(name) => Err(namespaces::missing_implicit(
                storage,
                frames.data[current].receiver.as_ref(),
                &program.namespaces[module],
                name,
            )),
            Some(_) => Ok(Target::Member(Value::nil(), index)),
            None => Err(super::undefined(program, frames, storage, current, name)),
        },
    }
}

pub(super) fn member(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: Value,
    site: CallSite,
    namespace: Option<usize>,
    instance: bool,
) -> Result<Target> {
    let name = &program.members[site.name];
    ctx.work_bytes(name.len())?;
    if matches!(receiver.0, Kind::Namespace(_) | Kind::Instance(_)) {
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
                instance,
            },
        )? {
            namespaces::Member::Function(call) => return Ok(Target::Method(call)),
            namespaces::Member::Value(value) => return Ok(value_invocation(&value)),
            namespaces::Member::Helper(receiver, helper)
                if matches!(name.as_str(), "eql?" | "equal?") =>
            {
                return Ok(Target::Helper(receiver, helper));
            }
            namespaces::Member::Helper(_, _) => return Ok(Target::Member(Value::nil(), site.name)),
            namespaces::Member::Missing => {}
        }
    } else if matches!(receiver.0, Kind::Enum(_) | Kind::EnumMember(_)) {
        if !site.scope && matches!(name.as_str(), "itself" | "eql?" | "equal?") {
            return Ok(Target::Member(receiver, site.name));
        }
        if !site.scope && names::universal(name) {
            return Ok(Target::Member(Value::nil(), site.name));
        }
        if !site.scope && matches!(name.as_str(), "to_s" | "string" | "inspect") {
            return Ok(Target::Member(Value::nil(), site.name));
        }
        if let Some(value) = crate::enums::call(
            ctx,
            CallSite { auto: true, ..site },
            name,
            &receiver,
            &[],
            false,
            false,
        )? {
            return Ok(value_invocation(&value));
        }
    } else if let Some(value) = members::field(ctx, site, name, &receiver)? {
        return Ok(value_invocation(&value));
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.tag.protected() && crate::bytecode::mutating_member(name) {
            return Err(hash.tag.mutation_error(name));
        }
    }
    if matches!(name.as_str(), "itself" | "eql?" | "equal?")
        && !names::temporal_method(&receiver, name)
    {
        return Ok(Target::Member(receiver, site.name));
    }
    if matches!(receiver.0, Kind::Range(_))
        && matches!(name.as_str(), "to_s" | "string" | "inspect")
    {
        return Ok(Target::Member(Value::nil(), site.name));
    }
    if let Some(kind) = names::typed(&receiver, name) {
        if kind == "nil" {
            return Ok(Target::Member(Value::nil(), site.name));
        }
        return Ok(Target::Unbound(kind, site.name));
    }
    if names::universal(name) && !names::temporal_method(&receiver, name) {
        return Ok(Target::Member(Value::nil(), site.name));
    }
    if matches!(receiver.0, Kind::Int(_) | Kind::Big(_))
        && matches!(
            name.as_str(),
            "seconds"
                | "second"
                | "minutes"
                | "minute"
                | "hours"
                | "hour"
                | "days"
                | "day"
                | "weeks"
                | "week"
        )
    {
        let (_, value) = members::call(ctx, CallSite { auto: true, ..site }, name, receiver, &[])?;
        return Ok(value_invocation(&value));
    }
    if matches!(
        receiver.0,
        Kind::Money(_) | Kind::Duration(_) | Kind::Time(_) | Kind::Zoned(_)
    ) {
        if names::temporal_method(&receiver, name) {
            if matches!(name.as_str(), "to_s" | "string" | "inspect" | "between?") {
                return Ok(Target::Member(Value::nil(), site.name));
            }
            return Ok(Target::Member(receiver, site.name));
        }
        let (_, value) = members::call(ctx, CallSite { auto: true, ..site }, name, receiver, &[])?;
        return Ok(value_invocation(&value));
    }
    if names::universal(name) {
        return Ok(Target::Member(Value::nil(), site.name));
    }
    let message = match names::Receiver::of(&receiver).unknown() {
        Some((wording, candidates)) => {
            let candidates = candidates.iter().map(|candidate| candidate.as_bytes());
            let suggestion = crate::members::suggest::did_you_mean(name, candidates);
            format!("{wording} {name}{suggestion}")
        }
        None if matches!(receiver.0, Kind::Instance(_)) => format!("unknown member {name}"),
        None => format!("unknown {} member {name}", receiver.type_name()),
    };
    Err(Error::new(ErrorKind::Name, message))
}
