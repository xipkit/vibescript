use super::*;
use crate::{
    bytecode::CallSite,
    members::introspection::{Predicate, Query},
    namespace::Helper,
};

mod send;

pub(super) struct Call<'a> {
    pub site: CallSite,
    pub name: &'a str,
    pub mutating: bool,
    pub args: Arguments,
    pub access: namespaces::Access,
}

pub(super) fn member(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    call: Call<'_>,
) -> Result<()> {
    let receiver = if let Some(crate::arguments::Target::Receiver(receiver)) = &call.args.target {
        receiver
    } else if call.mutating {
        &storage.addresses.data.last().unwrap().value
    } else {
        stack.data.last().unwrap()
    };
    let selected = if matches!(receiver.0, Kind::Namespace(_) | Kind::Instance(_)) {
        let receiver = receiver.clone();
        let selected =
            namespaces::member(ctx, storage, &receiver, call.site, call.name, call.access)?;
        // `as` casts any value, so a namespace without its own `as` still answers it.
        if matches!(selected, namespaces::Member::Missing) && call.name != "as" {
            namespaces::fallback(storage, &receiver, call.name, call.access.implicit)?;
        }
        selected
    } else {
        namespaces::Member::Missing
    };
    invoke(program, ctx, frames, storage, stack, call, selected)
}

fn invoke(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    call: Call<'_>,
    selected: namespaces::Member,
) -> Result<()> {
    let Call {
        site,
        name,
        mut mutating,
        mut args,
        access,
    } = call;
    if name == "as"
        && !site.scope
        && !mutating
        && args.target.is_none()
        && matches!(selected, namespaces::Member::Missing)
        && !members::introspection::field_named(ctx, name, stack.data.last().unwrap())?
    {
        let receiver = stack.data.pop().unwrap();
        let value = cast(program, ctx, frames, storage, receiver, &args)?;
        stack.push(ctx, value)?;
        return Ok(());
    }
    let captured = if matches!(args.target, Some(crate::arguments::Target::Receiver(_))) {
        let Some(crate::arguments::Target::Receiver(receiver)) = args.target.take() else {
            unreachable!()
        };
        Some(receiver)
    } else {
        None
    };
    let receiver = captured.as_ref().unwrap_or_else(|| {
        if mutating {
            &storage.addresses.data.last().unwrap().value
        } else {
            stack.data.last().unwrap()
        }
    });
    let method = if mutating {
        storage.addresses.data.last().unwrap().capability.clone()
    } else {
        None
    };
    let method = match method {
        Some(method) => Some(method),
        None => capabilities::member(ctx, site, name, receiver)?.map(|method| {
            crate::capability::SelectedMethod {
                method,
                receiver: receiver.clone(),
            }
        }),
    };
    if let Some(method) = method {
        let value = capabilities::call_on(
            ctx,
            storage,
            &method.method,
            Some(&method.receiver),
            &args.positional.data,
            &args.keywords.buffer.data,
            args.block,
            site.auto,
        )?;
        if mutating {
            storage.addresses.data.pop();
        } else {
            stack.data.pop();
        }
        value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
        return Ok(());
    }
    let retained = if mutating {
        storage.addresses.data.last().unwrap().exported.clone()
    } else {
        None
    };
    let function = if retained.is_some() {
        retained
    } else {
        crate::exports::member(ctx, site, name, receiver)?
    };
    if let Some(function) = function {
        if site.auto && site.scope {
            return Err(function.value_error());
        }
        if mutating {
            storage.addresses.data.pop();
        } else {
            stack.data.pop();
        }
        return requires::invoke(
            ctx,
            frames,
            storage,
            &function,
            args,
            site.auto,
            stack.data.len(),
        );
    }
    match selected {
        namespaces::Member::Function(function) => {
            if site.parenthesized && !function.constructor {
                args.options_hash = false;
            }
            if mutating {
                storage.addresses.data.pop();
            } else {
                stack.data.pop();
            }
            enter_arguments(
                program,
                ctx,
                frames,
                storage,
                function,
                args,
                stack.data.len(),
            )?;
            return Ok(());
        }
        namespaces::Member::Value(value) => {
            let receiver = receiver.clone();
            let value = capabilities::field_on(
                ctx,
                storage,
                site,
                Some(&receiver),
                value,
                &args.positional.data,
                &args.keywords.buffer.data,
                args.block,
            )?;
            if mutating {
                storage.addresses.data.pop();
            } else {
                stack.data.pop();
            }
            value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack)?;
            return Ok(());
        }
        namespaces::Member::Helper(module, helper) => {
            let value = dispatch::helper(
                program,
                ctx,
                frames,
                storage,
                (module, helper),
                &args,
                site.auto,
            )?;
            if mutating {
                storage.addresses.data.pop();
            } else {
                stack.data.pop();
            }
            stack.push(ctx, value)?;
            return Ok(());
        }
        namespaces::Member::Missing => {}
    }
    let receiver = if let Some(receiver) = &captured {
        receiver
    } else if mutating {
        &storage.addresses.data.last().unwrap().value
    } else {
        stack.data.last().unwrap()
    };
    if members::forwarding::applicable(ctx, site, name, receiver)? {
        return send::call(
            program,
            ctx,
            frames,
            storage,
            stack,
            Call {
                site,
                name,
                mutating,
                args,
                access,
            },
            captured,
        );
    }
    if let Some(receiver) = captured {
        storage.addresses.data.pop().unwrap();
        stack.push(ctx, receiver)?;
        mutating = false;
    }
    if name == "is_type?"
        && members::introspection::applicable(ctx, site, name, stack.data.last().unwrap())?
    {
        let receiver = stack.data.pop().unwrap();
        let value = type_predicate(program, ctx, frames, storage, &receiver, &args)?;
        stack.push(ctx, value)?;
        return Ok(());
    }
    let receiver = if mutating {
        &storage.addresses.data.last().unwrap().value
    } else {
        stack.data.last().unwrap()
    };
    if mutating {
        storage.addresses.data.last().unwrap().check_writable()?;
    }
    let arity = args
        .block
        .map(|block| frames.data[block.parent].program.functions[block.function].block_arity);
    let driver = if members::exported(ctx, site, name, receiver)? {
        None
    } else {
        iteration::start(
            ctx,
            name,
            receiver,
            &args.positional.data,
            &args.keywords.buffer.data,
            arity,
        )?
    };
    if let Some(iteration) = driver {
        if !mutating {
            stack.data.pop();
        }
        enter_iteration(
            program,
            ctx,
            frames,
            storage,
            stack.data.len(),
            args,
            iteration,
        )?;
        if mutating {
            let frame = frames.data.last_mut().unwrap();
            frame.mutating = true;
            // The native frame owns this address, including on nonlocal exits.
            frame.address_base -= 1;
        }
        return Ok(());
    }
    let value = if mutating {
        let address = storage.addresses.data.pop().unwrap();
        let guard_program = programs::address(ctx, storage, &address)?;
        let guard = address_guard(guard_program.as_deref(), ctx, frames, storage, &address)?;
        address.apply(
            ctx,
            address::Bindings {
                recover: !storage.handlers.data.is_empty(),
                guard,
                locals: &mut storage.locals.data,
                globals: &mut storage.globals.data,
                namespaces: &mut storage.namespaces.data,
            },
            &mut storage.addresses.data,
            |ctx, receiver| members::call_keywords(ctx, site, name, receiver, &args),
        )?
    } else {
        let receiver = stack.data.pop().unwrap();
        members::call_keywords(ctx, site, name, receiver, &args)?.1
    };
    stack.push(ctx, value)?;
    Ok(())
}

pub(super) fn helper(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    target: (Value, Helper),
    args: &Arguments,
    auto: bool,
) -> Result<Value> {
    let (receiver, helper) = target;
    if matches!(helper, Helper::Predicate(Predicate::IsType, _)) {
        type_predicate(program, ctx, frames, storage, &receiver, args)
    } else {
        namespaces::call_helper(ctx, storage, receiver, helper, args, auto)
    }
}

/// `value.as(T)`: checks the value against a type literal as a typed
/// parameter does, raising the same boundary error on a mismatch.
fn cast(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    receiver: Value,
    args: &Arguments,
) -> Result<Value> {
    if !args.keywords.buffer.data.is_empty() || args.block.is_some() {
        let shape = if args.block.is_some() {
            "a block"
        } else {
            "keyword arguments"
        };
        return Err(Error::argument(format!("as does not take {shape}")));
    }
    let [literal] = args.positional.data.as_slice() else {
        return Err(Error::argument("as expects exactly one type"));
    };
    // A class or enum names its own type.
    let nominal = match &literal.0 {
        Kind::Enum(enumeration) => Some(enumeration.definition.name.clone()),
        Kind::Namespace(class) => Some(class.definition.name.clone()),
        _ => None,
    };
    if let Some(name) = nominal {
        let ty = crate::types::Type {
            name: name.to_string(),
            kind: crate::types::TypeKind::Named,
            nullable: false,
        };
        let literal = literal.clone();
        return crate::types::prepare(ctx, &ty, |_, _| Ok(literal.clone()))?.normalize_with(
            ctx,
            receiver,
            crate::types::Context::Cast,
        );
    }
    let Kind::Shape(shape) = &literal.0 else {
        return Err(Error::new(
            ErrorKind::Type,
            "as expects a type, as in value.as(int)",
        ));
    };
    let lexical = lexical_scope(ctx, frames)?;
    let mut failed = None;
    let prepared = crate::types::prepare(ctx, &shape.definition.ty, |ctx, name| {
        resolve_type(program, ctx, frames, storage, lexical, name, false).inspect_err(|_| {
            failed = Some(name.to_owned());
        })
    });
    let prepared = match (prepared, failed) {
        (Err(error), Some(name)) => {
            return Err(crate::types::host_resolution(
                ctx,
                crate::types::Context::Cast,
                &name,
                error,
            )?);
        }
        (prepared, _) => prepared?,
    };
    prepared.normalize_with(ctx, receiver, crate::types::Context::Cast)
}

fn type_predicate(
    program: &Program,
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
    storage: &mut Storage,
    receiver: &Value,
    args: &Arguments,
) -> Result<Value> {
    let Query::Type(atom) = Predicate::IsType.validate(
        ctx,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )?
    else {
        unreachable!()
    };
    let resolved = if atom.nominal {
        let lexical = lexical_scope(ctx, frames)?;
        match resolve_type(program, ctx, frames, storage, lexical, atom.name, true) {
            Ok(value) => Some(value),
            Err(error) if error.kind == ErrorKind::Type => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    atom.matches(ctx, receiver, resolved.as_ref())
        .map(Value::boolean)
}

pub(super) fn lexical_scope(
    ctx: &mut CallContext,
    frames: &Buffer<Frame>,
) -> Result<Option<usize>> {
    for (index, frame) in frames.data.iter().enumerate().rev() {
        ctx.charge(1)?;
        if frame.function.is_some() {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

pub(super) fn reduce(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    values: [Value; 3],
) -> Result<()> {
    let [receiver, operation, argument] = values;
    let bytes = operation.require_bytes()?;
    let name = members::introspection::method_name(ctx, bytes)?;
    let args = Arguments::from_values(ctx, &[argument])?;
    let Some(name) = name else {
        if let Kind::Hash(hash) = &receiver.0 {
            if let Some(index) = hash.find(ctx, bytes)? {
                let site = CallSite {
                    name: 0,
                    method: None,
                    auto: false,
                    parenthesized: false,
                    scope: false,
                };
                let value = capabilities::field_on(
                    ctx,
                    storage,
                    site,
                    Some(&receiver),
                    hash.buffer.data[index].1.clone(),
                    &args.positional.data,
                    &[],
                    None,
                )?;
                return value.finish(program, ctx, frames, storage, stack, ReturnTo::Stack);
            }
        }
        return Err(Error::new(ErrorKind::Argument, "invalid reduce operation"));
    };
    let caller = &frames.data[lexical_scope(ctx, frames)?.unwrap()];
    let access = namespaces::Access {
        program: caller.program.index,
        caller: caller.program.functions[caller.function.unwrap()].namespace,
        implicit: false,
        instance: matches!(caller.receiver, Some(Value(Kind::Instance(_)))),
    };
    let site = CallSite {
        name: 0,
        method: crate::bytecode::Method::parse(name),
        auto: false,
        parenthesized: false,
        scope: false,
    };
    stack.push(ctx, receiver)?;
    member(
        program,
        ctx,
        frames,
        storage,
        stack,
        Call {
            site,
            name,
            mutating: false,
            args,
            access,
        },
    )
}
