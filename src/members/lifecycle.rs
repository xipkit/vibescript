use crate::{CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, value::Kind};

pub(super) fn supported(name: &str) -> bool {
    matches!(name, "clone" | "freeze" | "frozen?")
}

pub(super) fn callable(value: &Value) -> bool {
    matches!(
        value.0,
        Kind::Builtin(_) | Kind::Offset(_) | Kind::Function(_) | Kind::Host(_)
    )
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
                if callable(&hash.buffer.data[index].1) {
                    return Ok(None);
                }
            }
        }
    }
    ctx.checkpoint()?;
    let failure = if !args.is_empty() {
        Some("does not take arguments")
    } else if keywords {
        Some("does not take keyword arguments")
    } else if block {
        Some("does not accept blocks")
    } else {
        None
    };
    if let Some(failure) = failure {
        let prefix = if name == "frozen?" {
            format!("{}.frozen?", receiver.type_name())
        } else {
            name.to_owned()
        };
        return Err(Error::new(
            ErrorKind::Argument,
            format!("{prefix} {failure}"),
        ));
    }
    Ok(Some(if name == "frozen?" {
        Value::boolean(true)
    } else {
        // Copy-on-write preserves logical copies and protected-object tags.
        receiver.clone()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, builtin::Builtin, hash::Hash};

    #[test]
    fn object_exports_override_helpers_but_plain_data_does_not() {
        let mut ctx = CallContext::new(CallOptions::default());
        for name in ["clone", "freeze", "frozen?"] {
            for export in [false, true] {
                let mut hash = Hash::empty();
                hash.object = true;
                let key = ctx.bytes(name.as_bytes()).unwrap();
                let value = if export {
                    Value(Kind::Builtin(Builtin::JsonParse))
                } else {
                    Value::int(42)
                };
                hash.insert(&mut ctx, key, value).unwrap();
                let receiver = Value::from_hash(&mut ctx, hash).unwrap();
                let site = CallSite {
                    name: 0,
                    method: None,
                    auto: false,
                    parenthesized: true,
                    scope: false,
                };
                let arguments = if export {
                    vec![Value::bytes(b"[8]".to_vec())]
                } else {
                    vec![]
                };
                let (_, result) =
                    super::super::call(&mut ctx, site, name, receiver.clone(), &arguments).unwrap();
                if export {
                    assert_eq!(result.as_array().unwrap()[0].as_int(), Some(8));
                } else if name == "frozen?" {
                    assert!(matches!(result.0, Kind::Bool(true)));
                } else {
                    assert!(crate::ops::equal(&mut ctx, &receiver, &result, 0).unwrap());
                }
                let field = super::super::field(&mut ctx, site, name, &receiver).unwrap();
                assert_eq!(field.is_some(), export);
                let raw = super::super::field(
                    &mut ctx,
                    CallSite {
                        scope: true,
                        ..site
                    },
                    name,
                    &receiver,
                )
                .unwrap()
                .unwrap();
                assert_eq!(callable(&raw), export);
            }
        }
    }
}
