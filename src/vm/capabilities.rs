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
                    return ctx.guard(ErrorKind::Recursion, "recursion limit exceeded");
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

pub(super) fn bind(ctx: &mut CallContext, storage: &mut Storage) -> Result<()> {
    let capabilities = std::mem::take(&mut ctx.options.capabilities);
    let result = (|| {
        for capability in &capabilities {
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
                requires::set(ctx, storage, &capability.name, &value)?;
            }
        }
        Ok(())
    })();
    ctx.options.capabilities = capabilities;
    result
}

pub(super) fn call(
    ctx: &mut CallContext,
    storage: &mut Storage,
    method: &Arc<crate::capability::BoundMethod>,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
    auto: bool,
) -> Result<Call> {
    if auto {
        return Err(method.value_error());
    }
    if method.needs_frame() {
        let mut saved = Arguments::from_values(ctx, args)?;
        for (key, value) in keywords {
            saved.keywords.insert(ctx, key.clone(), value.clone())?;
        }
        saved.block = block;
        saved.target = Some(crate::arguments::Target::Capability(method.clone()));
        return Ok(Call::Block(saved));
    }
    let value = method.call(ctx, args, keywords, block.is_some())?;
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
            let result = callback(ctx, args, keywords);
            ctx.checkpoint()?;
            let value = ctx.import(&result?)?;
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

pub(super) fn field(
    ctx: &mut CallContext,
    storage: &mut Storage,
    site: crate::bytecode::CallSite,
    value: Value,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: Option<Block>,
) -> Result<Call> {
    if let Kind::Host(method) = &value.0 {
        call(ctx, storage, method, args, keywords, block, site.auto)
    } else {
        members::field_call(ctx, site, value, args, keywords, block.is_some()).map(Call::Value)
    }
}
