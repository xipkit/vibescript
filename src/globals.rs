use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    value::Kind,
};
use std::{collections::BTreeMap, sync::Arc};

/// Validates strict-effects inputs without importing values or executing host code.
pub(crate) fn validate(ctx: &mut CallContext, globals: &BTreeMap<String, Value>) -> Result<()> {
    let mut seen = Buffer::empty();
    for (name, value) in globals {
        ctx.work_bytes(name.len())?;
        if !data(ctx, value, &mut seen, 0)? {
            return Err(Error::new(
                ErrorKind::Runtime,
                format!(
                    "strict effects: global {name} must be data-only; register host capabilities separately"
                ),
            ));
        }
    }
    Ok(())
}

fn data(
    ctx: &mut CallContext,
    value: &Value,
    seen: &mut Buffer<usize>,
    depth: usize,
) -> Result<bool> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH || value.depth() > MAX_VALUE_DEPTH - depth {
        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
    }
    let identity = match &value.0 {
        Kind::Function(_)
        | Kind::Host(_)
        | Kind::Instance(_)
        | Kind::Namespace(_)
        | Kind::Builtin(_)
        | Kind::Offset(_)
        | Kind::Shape(_) => return Ok(false),
        Kind::Array(array) => Arc::as_ptr(array) as usize,
        Kind::Hash(hash) => Arc::as_ptr(hash) as usize,
        _ => return Ok(true),
    };
    for &previous in &seen.data {
        ctx.charge(1)?;
        if previous == identity {
            return Ok(true);
        }
    }
    match &value.0 {
        Kind::Array(array) => {
            for value in &array.buffer.data {
                if !data(ctx, value, seen, depth + 1)? {
                    return Ok(false);
                }
            }
        }
        Kind::Hash(hash) => {
            for (key, value) in &hash.buffer.data {
                if !data(ctx, key, seen, depth + 1)? || !data(ctx, value, seen, depth + 1)? {
                    return Ok(false);
                }
            }
        }
        _ => unreachable!(),
    }
    seen.push(ctx, identity)?;
    Ok(true)
}
