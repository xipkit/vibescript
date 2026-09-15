use crate::{CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, ops, value::Kind};

pub(crate) fn supported(name: &str) -> bool {
    matches!(name, "eql?" | "equal?")
}

pub(super) fn call(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if site.scope || !supported(name) {
        return Ok(None);
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.object {
            if let Some(index) = hash.find(ctx, name.as_bytes())? {
                if super::lifecycle::callable(&hash.buffer.data[index].1) {
                    return Ok(None);
                }
            }
        }
    }
    invoke(ctx, site.auto, name, receiver, args, keywords, block).map(Some)
}

pub(crate) fn invoke(
    ctx: &mut CallContext,
    auto: bool,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Value> {
    ctx.checkpoint()?;
    if auto && args.is_empty() && !keywords && !block {
        return Err(Error::new(
            ErrorKind::Type,
            format!("{name} is a method and cannot be used as a value; call it with {name}(...)"),
        ));
    }
    let temporal = name == "eql?"
        && matches!(
            receiver.0,
            Kind::Duration(_) | Kind::Time(_) | Kind::Zoned(_)
        );
    if keywords || (block && !temporal) {
        let shape = if keywords {
            "keyword arguments"
        } else {
            "a block"
        };
        return Err(Error::new(
            ErrorKind::Argument,
            format!("{}.{name} does not accept {shape}", receiver.type_name()),
        ));
    }
    if args.len() != 1 {
        return Err(Error::new(
            ErrorKind::Argument,
            format!(
                "{}.{name} expects 1 argument, got {}",
                receiver.type_name(),
                args.len()
            ),
        ));
    }
    let result = if name == "eql?" {
        ops::eql(ctx, receiver, &args[0], 0)?
    } else {
        identical(ctx, receiver, &args[0])?
    };
    Ok(Value::boolean(result))
}

fn identical(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<bool> {
    if a.type_name() != b.type_name() {
        ctx.charge(1)?;
        return Ok(false);
    }
    let result = match (&a.0, &b.0) {
        (Kind::Big(a), Kind::Big(b)) => a.identical(b),
        (Kind::Float(a), Kind::Float(b)) => a == b || (a.is_nan() && b.is_nan()),
        (Kind::Enum(a), Kind::Enum(b)) => a.identical(b),
        (Kind::EnumMember(a), Kind::EnumMember(b)) => {
            a.index == b.index && a.enumeration.identical(&b.enumeration)
        }
        _ => return ops::equal(ctx, a, b, 0),
    };
    ctx.charge(1)?;
    Ok(result)
}
