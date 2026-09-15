use crate::{CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, value::Kind};

pub(crate) fn supported(name: &str) -> bool {
    matches!(name, "send" | "public_send")
}

pub(crate) fn applicable(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
) -> Result<bool> {
    if site.scope || !supported(name) {
        return Ok(false);
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.object {
            if let Some(index) = hash.find(ctx, name.as_bytes())? {
                if super::lifecycle::callable(&hash.buffer.data[index].1) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

pub(crate) fn method<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value> {
    let Some(value) = args.first() else {
        return Err(Error::new(
            ErrorKind::Argument,
            format!("{name} expects a method name"),
        ));
    };
    if !matches!(value.0, Kind::Bytes(_) | Kind::Symbol(_)) {
        return Err(Error::new(
            ErrorKind::Type,
            format!("{name} expects a symbol or string method name"),
        ));
    }
    Ok(value)
}

pub(crate) fn unknown(ctx: &mut CallContext, receiver: &Value, name: &[u8]) -> Result<Error> {
    let (kind, label) = match receiver.0 {
        Kind::Hash(_) => ("hash", " method "),
        Kind::Namespace(_) => ("class", " member "),
        Kind::Instance(_) => ("", "member "),
        Kind::Enum(_) => ("enum", " property "),
        Kind::EnumMember(_) => ("enum member", " property "),
        _ => (receiver.type_name(), " method "),
    };
    let Some(size) = name.len().checked_add(8 + kind.len() + label.len()) else {
        return ctx.fail(ErrorKind::Memory, "diagnostic size overflow");
    };
    let mut bytes = crate::budget::Buffer::with_capacity(ctx, size)?;
    bytes.extend(ctx, b"unknown ")?;
    bytes.extend(ctx, kind.as_bytes())?;
    bytes.extend(ctx, label.as_bytes())?;
    for chunk in name.chunks(crate::budget::CHUNK) {
        ctx.work_bytes(chunk.len())?;
        bytes.extend(ctx, chunk)?;
    }
    let mut error = Error::from_bytes(ctx, &bytes.data)?;
    error.kind = ErrorKind::Name;
    Ok(error)
}
