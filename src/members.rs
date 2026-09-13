use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    bytecode::{CallSite, Method},
    ops,
    value::Kind,
};

pub(crate) fn call(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: Value,
    args: &[Value],
) -> Result<(Value, Value)> {
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
