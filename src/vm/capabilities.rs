use super::*;

pub(super) enum Call {
    Value(Value),
    Block(Arguments),
}

impl Call {
    pub fn finish(
        self,
        program: &Program,
        ctx: &mut CallContext,
        frames: &mut Buffer<Frame>,
        storage: &mut Storage,
        stack: &mut Buffer<Value>,
        return_to: ReturnTo,
    ) -> Result<()> {
        match self {
            Self::Value(value) => match return_to {
                ReturnTo::Stack => stack.push(ctx, value),
                ReturnTo::Address => storage.addresses.push(ctx, Address::new(None, value)),
                _ => unreachable!(),
            },
            Self::Block(args) => {
                ctx.charge(1)?;
                if frames.data.len() >= ctx.options.limits.recursion {
                    return super::recursion_exceeded(ctx);
                }
                let mut frame = new_frame(ctx, program, storage, None, stack.data.len())?;
                frame.host = true;
                frame.block = args.block;
                frame.return_to = return_to;
                frame.arguments.push(ctx, args)?;
                frames.push(ctx, frame)
            }
        }
    }
}

pub(super) fn bind(
    ctx: &mut CallContext,
    storage: &mut Storage,
    declared: &crate::declared::Declarations,
) -> Result<()> {
    let capabilities = std::mem::take(&mut ctx.options.capabilities);
    let result = (|| {
        for (index, capability) in capabilities.iter().enumerate() {
            let name = ctx.bytes(capability.name.as_bytes())?;
            let mut names = std::mem::replace(&mut ctx.capability_names, Buffer::empty());
            let inserted = names.push(ctx, name);
            ctx.capability_names = names;
            inserted?;
            let value = capability.bind(ctx)?;
            if !matches!(value.0, Kind::Host(_)) {
                crate::exports::check(ctx, &value)?;
            }
            programs::imported(ctx, storage, &value)?;
            if !globals::input_contains(ctx, &capability.name)? {
                // A later grant of the name replaces this one. A host that
                // declares nothing pays for no check.
                if !declared.is_empty() {
                    ctx.charge((capabilities.len() - index) as u64)?;
                    let replaced = capabilities[index + 1..]
                        .iter()
                        .any(|later| later.name == capability.name);
                    if !replaced {
                        crate::declared::check_capability(ctx, declared, &capability.name, &value)?;
                    }
                }
                requires::set(ctx, storage, &capability.name, &value)?;
            }
        }
        Ok(())
    })();
    ctx.options.capabilities = capabilities;
    result
}

/// Selects the member receiver a host callback may snapshot.
///
/// A bare descriptor reached without a member lookup, or through another bare
/// descriptor, has no receiver.
fn member_receiver(receiver: Option<&Value>) -> Option<Value> {
    receiver
        .filter(|value| !matches!(value.0, Kind::Host(_)))
        .cloned()
}

/// Calls a bound method without a member receiver.
pub(super) fn call(
    ctx: &mut CallContext,
    storage: &mut Storage,
    method: &Arc<crate::capability::BoundMethod>,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
    auto: bool,
) -> Result<Call> {
    call_on(ctx, storage, method, None, args, keywords, block, auto)
}

/// Calls a bound method selected from `receiver`.
///
/// The receiver rides on the transient frame arguments of block-capable and
/// signed methods only; plain callbacks never observe it, and it is released
/// with the invocation.
#[allow(clippy::too_many_arguments)]
pub(super) fn call_on(
    ctx: &mut CallContext,
    storage: &mut Storage,
    method: &Arc<crate::capability::BoundMethod>,
    receiver: Option<&Value>,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
    auto: bool,
) -> Result<Call> {
    if auto
        && method
            .signature()
            .is_some_and(|sig| sig.source.params.iter().any(|p| !p.optional))
    {
        return Err(method.value_error());
    }
    if method.needs_frame() {
        let mut saved = Arguments::from_values(ctx, args)?;
        for (key, value) in keywords {
            saved.keywords.insert(ctx, key.clone(), value.clone())?;
        }
        saved.block = block;
        saved.target = Some(crate::arguments::Target::Capability(method.clone()));
        saved.receiver = member_receiver(receiver);
        return Ok(Call::Block(saved));
    }
    let args = snapshot_arguments(ctx, storage, args, keywords)?;
    method.begin(
        ctx,
        &args.positional.data,
        &args.keywords.buffer.data,
        block.is_some(),
    )?;
    let value = method.invoke_plain(ctx, &args.positional.data, &args.keywords.buffer.data)?;
    let value = ctx.snapshot(&value)?;
    let value = method.finish(ctx, value)?;
    programs::imported(ctx, storage, &value)?;
    Ok(Call::Value(value))
}

pub(super) fn registered(
    ctx: &mut CallContext,
    storage: &mut Storage,
    host: &crate::capability::Registered,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
) -> Result<Call> {
    match host {
        crate::capability::Registered::Callback(callback) => {
            ctx.checkpoint()?;
            let args = snapshot_arguments(ctx, storage, args, keywords)?;
            let result = callback(ctx, &args.positional.data, &args.keywords.buffer.data);
            ctx.checkpoint()?;
            let value = ctx.snapshot(&result?)?;
            crate::exports::check(ctx, &value)?;
            programs::imported(ctx, storage, &value)?;
            Ok(Call::Value(value))
        }
        crate::capability::Registered::Method(method) => {
            let Value(Kind::Host(method)) = ctx.import(&method.value())? else {
                unreachable!()
            };
            call(ctx, storage, &method, args, keywords, block, false)
        }
    }
}

fn snapshot_arguments(
    ctx: &mut CallContext,
    storage: &Storage,
    args: &[Value],
    keywords: &[(Value, Value)],
) -> Result<Arguments> {
    let mut args = Arguments::from_values(ctx, args)?;
    for (key, value) in keywords {
        args.keywords.insert(ctx, key.clone(), value.clone())?;
    }
    programs::snapshot_arguments(
        ctx,
        storage,
        &mut args.positional.data,
        &mut args.keywords.buffer.data,
    )?;
    Ok(args)
}

pub(super) fn member(
    ctx: &mut CallContext,
    site: crate::bytecode::CallSite,
    name: &str,
    receiver: &Value,
) -> Result<Option<Arc<crate::capability::BoundMethod>>> {
    if let Kind::Host(method) = &receiver.0 {
        return Ok(Some(method.clone()));
    }
    if ctx.has_exports && matches!(receiver.0, Kind::Hash(_)) {
        if let Some(Value(Kind::Host(method))) = members::prepare(ctx, site, name, receiver)? {
            return Ok(Some(method));
        }
    }
    Ok(None)
}

/// Calls a stored field value without a member receiver.
pub(super) fn field(
    ctx: &mut CallContext,
    storage: &mut Storage,
    site: crate::bytecode::CallSite,
    value: Value,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
) -> Result<Call> {
    field_on(ctx, storage, site, None, value, args, keywords, block)
}

/// Calls a field value selected from `receiver`; host methods keep the receiver.
#[allow(clippy::too_many_arguments)]
pub(super) fn field_on(
    ctx: &mut CallContext,
    storage: &mut Storage,
    site: crate::bytecode::CallSite,
    receiver: Option<&Value>,
    value: Value,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
) -> Result<Call> {
    if let Kind::Host(method) = &value.0 {
        call_on(
            ctx, storage, method, receiver, args, keywords, block, site.auto,
        )
    } else {
        members::field_call(ctx, site, value, args, keywords, block.is_some()).map(Call::Value)
    }
}
