use super::*;
use crate::{namespace::Call, syntax::modules::Visibility};
use std::sync::Arc;

pub(super) struct Resolved {
    pub call: Call,
    pub negate: bool,
}

fn method(
    ctx: &mut CallContext,
    receiver: &Value,
    name: &str,
) -> Result<Option<(Call, Visibility)>> {
    let Kind::Instance(instance) = &receiver.0 else {
        return Ok(None);
    };
    let definition = &instance.class().definition;
    for method in &definition.instance_methods {
        ctx.charge(1)?;
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name == name {
            return Ok(Some((
                Call {
                    receiver: Some(receiver.clone()),
                    ..method.function.into()
                },
                method.visibility,
            )));
        }
    }
    Ok(None)
}

pub(super) fn resolve(
    program: &Program,
    ctx: &mut CallContext,
    receiver: &Value,
    name: &str,
    caller: (Option<usize>, bool),
) -> Result<Option<Resolved>> {
    let mut found = method(ctx, receiver, name)?;
    let mut negate = false;
    if found.is_none() && name == "!=" {
        found = method(ctx, receiver, "==")?;
        negate = found.is_some();
    }
    let Some((call, visibility)) = found else {
        return Ok(None);
    };
    let allowed = match visibility {
        Visibility::Public => true,
        Visibility::Private => false,
        Visibility::Protected => {
            caller.1
                && caller
                    .0
                    .and_then(|index| program.namespaces.get(index))
                    .is_some_and(|definition| {
                        matches!(&receiver.0, Kind::Instance(instance)
                if Arc::ptr_eq(definition, &instance.class().definition))
                    })
        }
    };
    if !allowed {
        return Err(Error::new(
            ErrorKind::Name,
            "operator method is not accessible with this receiver",
        ));
    }
    Ok(Some(Resolved { call, negate }))
}

pub(super) fn index(
    program: &Program,
    ctx: &mut CallContext,
    receiver: &Value,
    name: &str,
    caller: (Option<usize>, bool),
) -> Result<Call> {
    resolve(program, ctx, receiver, name, caller)?
        .map(|resolved| resolved.call)
        .ok_or_else(|| Error::new(ErrorKind::Name, format!("instance does not define {name}")))
}

pub(super) fn string(ctx: &mut CallContext, receiver: &Value) -> Result<Option<Call>> {
    let Some((call, _)) = method(ctx, receiver, "to_s")? else {
        return Ok(None);
    };
    let Kind::Instance(instance) = &receiver.0 else {
        unreachable!()
    };
    let owner = instance
        .class()
        .owner
        .as_ref()
        .ok_or_else(|| Error::new(ErrorKind::Type, "namespace has no executable source"))?;
    for param in &owner.program.functions[call.function].params {
        ctx.charge(1)?;
        if matches!(
            param.kind,
            crate::syntax::ParamKind::Positional | crate::syntax::ParamKind::Keyword
        ) && !param.default
        {
            return Ok(None);
        }
    }
    Ok(Some(call))
}
