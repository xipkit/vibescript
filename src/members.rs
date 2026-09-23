use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    bytecode::{CallSite, Method},
    ops,
    value::Kind,
};

mod conversion;
pub(crate) mod equality;
pub(crate) mod forwarding;
pub(crate) mod introspection;
mod lifecycle;
pub(crate) mod names;
pub(crate) mod suggest;

pub(crate) fn call_keywords(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: Value,
    args: &crate::arguments::Arguments,
) -> Result<(Value, Value)> {
    let kind = names::Receiver::of(&receiver);
    dispatch_keywords(ctx, site, name, receiver, args)
        .map_err(|error| unknown(site, kind, name, error))
}

fn dispatch_keywords(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: Value,
    args: &crate::arguments::Arguments,
) -> Result<(Value, Value)> {
    if forwarding::applicable(ctx, site, name, &receiver)? {
        forwarding::method(name, &args.positional.data)?;
        return Err(Error::new(
            ErrorKind::Type,
            "forwarded call requires an execution context",
        ));
    }
    if let Some(value) = introspection::call(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = equality::call(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = lifecycle::call(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = conversion::call(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        (!args.keywords.buffer.data.is_empty(), args.block.is_some()),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::shapes::member(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::enums::call(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::time::member(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::money::member(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::duration::member(
        ctx,
        site,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, value));
    }
    if let Some(value) = field(ctx, site, name, &receiver)? {
        let result = field_call(
            ctx,
            site,
            value,
            &args.positional.data,
            &args.keywords.buffer.data,
            args.block.is_some(),
        )?;
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::basic::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::inspect::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::template::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        &args.keywords.buffer.data,
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::substitute::member(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        &args.keywords.buffer.data,
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::value::member(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::operations::member(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::member(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::sets::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::combinatorics::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::charset::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
        args.block.is_some(),
    )? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::transform::call(
        ctx,
        name,
        &receiver,
        &args.positional.data,
        !args.keywords.buffer.data.is_empty(),
    )? {
        return Ok((receiver, result));
    }
    let numeric = matches!(receiver.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_));
    if args.block.is_some()
        && matches!(
            site.method,
            Some(Method::IsNil | Method::Itself | Method::Dup)
        )
    {
        return Err(Error::new(
            ErrorKind::Argument,
            "method does not accept a block",
        ));
    }
    if numeric
        && (matches!(name, "inspect" | "clamp" | "between?")
            || matches!(
                site.method,
                Some(Method::ToString | Method::ToInt | Method::ToFloat)
            ))
        && (args.block.is_some() || !args.keywords.buffer.data.is_empty())
    {
        return Err(Error::new(
            ErrorKind::Argument,
            format!("{name} does not accept keyword arguments or blocks"),
        ));
    }
    if matches!(receiver.0, Kind::Hash(_)) && !hash_builtin(name) {
        return call(ctx, site, name, receiver, &args.positional.data);
    }
    if !args.keywords.buffer.data.is_empty() {
        let refusal = names::Receiver::of(&receiver).keyword_refusal(
            site.method,
            name,
            args.positional.data.len(),
        );
        if let Some(message) = refusal {
            return Err(Error::new(ErrorKind::Argument, message));
        }
    }
    // Resolve stored-field overrides before rejecting a native block, and
    // reject before `call` can mutate the receiver.
    if args.block.is_some() {
        let rejection = names::Receiver::of(&receiver).rejects_block(
            site.method,
            !args.positional.data.is_empty(),
            name,
        );
        if let Some(message) = rejection {
            return Err(Error::new(ErrorKind::Argument, message));
        }
    }
    call(ctx, site, name, receiver, &args.positional.data)
}

pub(crate) fn call(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: Value,
    args: &[Value],
) -> Result<(Value, Value)> {
    let kind = names::Receiver::of(&receiver);
    dispatch(ctx, site, name, receiver, args).map_err(|error| unknown(site, kind, name, error))
}

fn dispatch(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: Value,
    args: &[Value],
) -> Result<(Value, Value)> {
    if forwarding::applicable(ctx, site, name, &receiver)? {
        forwarding::method(name, args)?;
        return Err(Error::new(
            ErrorKind::Type,
            "forwarded call requires an execution context",
        ));
    }
    if let Some(value) = introspection::call(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = equality::call(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = lifecycle::call(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = conversion::call(ctx, site, name, &receiver, args, (false, false))? {
        return Ok((receiver, value));
    }
    if let Some(result) = crate::text::basic::call(ctx, name, &receiver, args, false, false)? {
        return Ok((receiver, result));
    }
    if let Some(value) = crate::shapes::member(ctx, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::enums::call(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::time::member(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::money::member(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = crate::duration::member(ctx, site, name, &receiver, args, false, false)? {
        return Ok((receiver, value));
    }
    if let Some(value) = field(ctx, site, name, &receiver)? {
        let result = field_call(ctx, site, value, args, &[], false)?;
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::inspect::call(ctx, name, &receiver, args, false, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::template::call(ctx, name, &receiver, args, &[])? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::substitute::member(ctx, name, &receiver, args, &[])? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::value::member(ctx, name, &receiver, args, false, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::operations::member(ctx, name, &receiver, args, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::regex::member(ctx, name, &receiver, args, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::case::call(ctx, name, &receiver, args)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::sets::call(ctx, name, &receiver, args, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::combinatorics::call(ctx, name, &receiver, args, false, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::charset::call(ctx, name, &receiver, args, false, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::text::transform::call(ctx, name, &receiver, args, false)? {
        return Ok((receiver, result));
    }
    if let Some(result) = crate::numeric::call(ctx, name, &receiver, args)? {
        return Ok((receiver, result));
    }
    if name == "inspect" && matches!(receiver.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
        ops::arity(args, 0)?;
        let result = ops::to_string(ctx, &receiver)?;
        return Ok((receiver, result));
    }
    if let Some(value) = crate::iteration::without_block(ctx, name, &receiver, args)? {
        return Ok((receiver, value));
    }
    if matches!(receiver.0, Kind::Bytes(_)) && matches!(name, "unshift" | "append" | "find_index") {
        return Err(Error::new(
            ErrorKind::Name,
            format!("unknown string method {name}"),
        ));
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if !hash_builtin(name) {
            if let Some(index) = hash.find(ctx, name.as_bytes())? {
                if !site.auto {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "attempted to call non-callable value",
                    ));
                }
                let result = hash.buffer.data[index].1.clone();
                return Ok((receiver, result));
            }
            return Err(unknown_hash_member(hash, name));
        }
    }
    let method = site
        .method
        .ok_or_else(|| Error::new(ErrorKind::Name, format!("method {name} is not implemented")))?;
    if let Kind::Hash(hash) = &receiver.0 {
        // The reference serves these hash members as plain methods, so a bare
        // read names them instead of calling them. A protected record refuses
        // the mutators before that.
        let plain = matches!(method, Method::Store | Method::Delete | Method::Replace)
            && !hash.tag.protected()
            || matches!(method, Method::RemapKeys);
        if site.auto && args.is_empty() && plain {
            return Err(Error::new(
                ErrorKind::Argument,
                format!(
                    "{name} is a method and cannot be used as a value; call it with {name}(...)"
                ),
            ));
        }
    }
    if matches!(
        method,
        Method::Push
            | Method::Prepend
            | Method::Pop
            | Method::Shift
            | Method::Delete
            | Method::Insert
            | Method::Clear
            | Method::Fill
            | Method::Store
            | Method::Replace
    ) {
        return crate::mutate::call(ctx, method, name, receiver, args);
    }
    let result = ops::method(ctx, method, name, receiver.clone(), args)?;
    Ok((receiver, result))
}

/// Reports a dispatch failure for a member the receiver's kind does not define
/// the way the reference does, which rejects the name before any argument is
/// examined. Members the port answers beyond the reference tables still run,
/// and scoped lookups keep their own wording.
fn unknown(site: CallSite, kind: names::Receiver, name: &str, error: Error) -> Error {
    if site.scope
        || !matches!(
            error.kind,
            ErrorKind::Type | ErrorKind::Name | ErrorKind::Argument
        )
        || names::universal(name)
        || kind.available(name)
    {
        return error;
    }
    let Some((wording, candidates)) = kind.unknown() else {
        return error;
    };
    let suggestion = suggest::did_you_mean(name, candidates.iter().map(|c| c.as_bytes()));
    error.with_message(format!("{wording} {name}{suggestion}"))
}

/// Reports a hash member that is neither a builtin nor a stored key. Universal
/// helpers keep the plain wording; others suggest builtins and stored keys.
fn unknown_hash_member(hash: &crate::hash::Hash, name: &str) -> Error {
    if names::universal(name) {
        return Error::new(ErrorKind::Name, format!("unknown member {name}"));
    }
    let builtins = names::candidates::HASH.iter().map(|c| c.as_bytes());
    let keys = hash
        .buffer
        .data
        .iter()
        .filter_map(|(key, _)| key.as_bytes());
    let suggestion = suggest::did_you_mean(name, builtins.chain(keys));
    Error::new(
        ErrorKind::Name,
        format!("unknown hash method {name}{suggestion}"),
    )
}

pub(crate) fn exported(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
) -> Result<bool> {
    if site.scope {
        return Ok(true);
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.object {
            return Ok(hash.find(ctx, name.as_bytes())?.is_some());
        }
    }
    Ok(false)
}

pub(crate) fn prepare(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
) -> Result<Option<Value>> {
    if let Kind::Hash(hash) = &receiver.0 {
        if !site.scope
            && (names::universal(name) || names::available(receiver, name))
            && (!hash.object || hash.find(ctx, name.as_bytes())?.is_none())
        {
            return Ok(None);
        }
        return field(ctx, site, name, receiver);
    }
    Ok(None)
}

pub(crate) fn field(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
) -> Result<Option<Value>> {
    if !site.scope && names::universal(name) && lifecycle::callable(receiver) {
        return Ok(None);
    }
    if let Kind::Host(method) = &receiver.0 {
        return Err(method.value_error());
    }
    if let Kind::Offset(offset) = &receiver.0 {
        return Err(offset.value_error());
    }
    if let Kind::Builtin(builtin) = receiver.0 {
        return Err(builtin.value_error());
    }
    if site.scope && !matches!(&receiver.0, Kind::Hash(hash) if hash.object) {
        return Err(Error::new(
            ErrorKind::Type,
            "scoped member access requires a namespace",
        ));
    }
    if let Kind::Hash(hash) = &receiver.0 {
        if hash.object || !hash_builtin(name) {
            if let Some(index) = hash.find(ctx, name.as_bytes())? {
                if !site.scope
                    && names::universal(name)
                    && !matches!(name, "tap" | "yield_self")
                    && !lifecycle::callable(&hash.buffer.data[index].1)
                {
                    return Ok(None);
                }
                return Ok(Some(hash.buffer.data[index].1.clone()));
            }
            if site.scope {
                let keys = hash
                    .buffer
                    .data
                    .iter()
                    .filter_map(|(key, _)| key.as_bytes());
                let suggestion = suggest::did_you_mean(name, keys);
                return Err(Error::new(
                    ErrorKind::Name,
                    format!("unknown member {name}{suggestion}"),
                ));
            }
            if !hash_builtin(name) {
                return Err(unknown_hash_member(hash, name));
            }
        }
    }
    Ok(None)
}

pub(crate) fn field_call(
    ctx: &mut CallContext,
    site: CallSite,
    value: Value,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: bool,
) -> Result<Value> {
    if let Kind::Host(method) = &value.0 {
        return Err(method.value_error());
    }
    if site.auto {
        if let Kind::Offset(offset) = &value.0 {
            if !site.scope {
                return Err(offset.value_error());
            }
        }
        if let Kind::Builtin(builtin) = value.0 {
            if !site.scope {
                if builtin.auto() {
                    return builtin.call(ctx, args, keywords, block);
                }
                return Err(builtin.value_error());
            }
        }
        return Ok(value);
    }
    if let Kind::Offset(offset) = &value.0 {
        return offset.call(ctx, args, keywords, block);
    }
    if let Kind::Builtin(builtin) = value.0 {
        return builtin.call(ctx, args, keywords, block);
    }
    Err(Error::new(
        ErrorKind::Type,
        "attempted to call non-callable value",
    ))
}

pub(crate) fn hash_builtin(name: &str) -> bool {
    matches!(
        name,
        "size"
            | "length"
            | "empty?"
            | "key?"
            | "has_key?"
            | "member?"
            | "include?"
            | "value?"
            | "has_value?"
            | "keys"
            | "values"
            | "values_at"
            | "fetch"
            | "fetch_values"
            | "dig"
            | "each"
            | "each_with_index"
            | "each_key"
            | "each_value"
            | "to_a"
            | "merge"
            | "replace"
            | "store"
            | "delete"
            | "clear"
            | "delete_if"
            | "keep_if"
            | "slice"
            | "except"
            | "flatten"
            | "select"
            | "reject"
            | "map"
            | "map_with_index"
            | "transform_keys"
            | "deep_transform_keys"
            | "remap_keys"
            | "transform_values"
            | "compact"
            | "inspect"
            | "itself"
            | "nil?"
            | "eql?"
            | "equal?"
            | "respond_to?"
            | "is_a?"
            | "kind_of?"
            | "instance_of?"
            | "is_type?"
            | "send"
            | "public_send"
            | "dup"
            | "clone"
            | "freeze"
            | "frozen?"
    )
}
