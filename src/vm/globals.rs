use super::*;

pub(super) fn validate(ctx: &mut CallContext) -> Result<()> {
    if !ctx.strict_effects || ctx.options.globals.is_empty() {
        return Ok(());
    }
    let globals = std::mem::take(&mut ctx.options.globals);
    let result = crate::globals::validate(ctx, &globals);
    ctx.options.globals = globals;
    result
}

pub(super) fn contains(ctx: &mut CallContext, name: &str) -> Result<bool> {
    for index in 0..ctx.capability_names.data.len() {
        ctx.charge(1)?;
        let candidate = ctx.capability_names.data[index].clone();
        if crate::json::bytes_equal(ctx, candidate.as_bytes().unwrap(), name.as_bytes())? {
            return Ok(true);
        }
    }
    input_contains(ctx, name)
}

pub(super) fn input_contains(ctx: &mut CallContext, name: &str) -> Result<bool> {
    if ctx.options.globals.is_empty() {
        return Ok(false);
    }
    ctx.work_bytes(name.len())?;
    ctx.charge(ctx.options.globals.len().ilog2() as u64 + 1)?;
    Ok(ctx.options.globals.contains_key(name))
}

pub(super) fn import(ctx: &mut CallContext, name: &str) -> Result<Option<Value>> {
    if !input_contains(ctx, name)? {
        return Ok(None);
    }
    let Some(value) = ctx.options.globals.get(name).cloned() else {
        return Ok(None);
    };
    let active = std::mem::replace(&mut ctx.enum_rebind.active, true);
    let value = ctx.import(&value);
    ctx.enum_rebind.active = active;
    let value = value?;
    if !matches!(value.0, Kind::Host(_)) {
        crate::exports::check(ctx, &value)?;
    }
    Ok(Some(value))
}

pub(super) fn get(
    ctx: &mut CallContext,
    storage: &mut Storage,
    name: &str,
) -> Result<Option<Value>> {
    if contains(ctx, name)? {
        requires::get(ctx, storage, name)
    } else {
        Ok(None)
    }
}

pub(super) fn types(
    ctx: &mut CallContext,
    storage: &mut Storage,
    binding: &str,
    fold: bool,
) -> Result<()> {
    let sources = std::mem::take(&mut ctx.options.globals);
    let result = (|| {
        let mut names = Buffer::empty();
        for name in sources.keys() {
            if type_name_matches(ctx, name, binding, fold)? {
                let name = ctx.bytes(name.as_bytes())?;
                names.push(ctx, name)?;
            }
        }
        Ok::<_, Error>(names)
    })();
    ctx.options.globals = sources;
    for name in result?.data {
        requires::get(
            ctx,
            storage,
            std::str::from_utf8(name.as_bytes().unwrap()).unwrap(),
        )?;
    }
    Ok(())
}
