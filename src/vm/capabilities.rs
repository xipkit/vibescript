use super::*;

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
    method: &crate::capability::BoundMethod,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: bool,
    auto: bool,
) -> Result<Value> {
    if auto {
        return Err(method.value_error());
    }
    let value = method.call(ctx, args, keywords, block)?;
    programs::imported(ctx, storage, &value)?;
    Ok(value)
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
    block: bool,
) -> Result<Value> {
    if let Kind::Host(method) = &value.0 {
        call(ctx, storage, method, args, keywords, block, site.auto)
    } else {
        members::field_call(ctx, site, value, args, keywords, block)
    }
}
