use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    bytecode::{CallSite, Method},
    ops,
    value::Kind,
};

pub(crate) fn call_keywords(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: Value,
    args: &crate::arguments::Arguments,
) -> Result<(Value, Value)> {
    let numeric = matches!(receiver.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_));
    if numeric
        && (name == "inspect"
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
        use Method::*;
        let rejects = match site.method {
            Some(IsNil | Itself | Dup | ToString | ToInt | ToFloat) => true,
            Some(method) => match &receiver.0 {
                Kind::Array(_) => matches!(
                    method,
                    First
                        | Last
                        | At
                        | Slice
                        | Reverse
                        | Compact
                        | Uniq
                        | Transpose
                        | ToHash
                        | Push
                        | Prepend
                        | Pop
                        | Shift
                        | Delete
                        | Insert
                        | Clear
                        | Fill
                        | Sum
                ),
                Kind::Hash(_) => {
                    matches!(method, ToArray | Flatten | Store | Delete | Replace | Clear)
                }
                Kind::Bytes(_) => {
                    matches!(method, ByteSlice | GetByte | Bytes | Chars | Codepoints)
                }
                Kind::Range(_) => true,
                _ => false,
            },
            None => false,
        };
        if rejects {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} does not accept keyword arguments"),
            ));
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
    if name == "inspect" && matches!(receiver.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
        ops::arity(args, 0)?;
        let result = ops::to_string(ctx, &receiver)?;
        return Ok((receiver, result));
    }
    if let Some(value) = crate::iteration::without_block(ctx, name, &receiver, args)? {
        return Ok((receiver, value));
    }
    if matches!(receiver.0, Kind::Bytes(_)) && matches!(name, "unshift" | "append") {
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
            return Err(Error::new(
                ErrorKind::Name,
                format!("unknown hash member {name}"),
            ));
        }
    }
    let method = site
        .method
        .ok_or_else(|| Error::new(ErrorKind::Name, format!("method {name} is not implemented")))?;
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
        return crate::mutate::call(ctx, method, receiver, args);
    }
    let result = ops::method(ctx, method, receiver.clone(), args)?;
    Ok((receiver, result))
}

fn hash_builtin(name: &str) -> bool {
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
            | "dup"
    )
}
