use vibescript::{Capability, Error, ErrorKind, HostMethod, Value};

/// Builds the `blocks` capability whose methods drive script blocks.
pub fn capability() -> Capability {
    Capability::new("blocks", |_| Ok(template()))
}

/// A fresh value of the `blocks` capability, which each call binds and which
/// also declares the capability to a statically typed engine.
pub fn template() -> Value {
    let once = HostMethod::new_with_block("blocks.once", |call, args, _| call.call_block(args));
    let each = HostMethod::new_with_block("blocks.each", |call, args, _| {
        let mut result = Vec::new();
        let items = args
            .first()
            .and_then(Value::as_array)
            .ok_or_else(|| Error::new(ErrorKind::Argument, "blocks.each expects an array"))?;
        for item in items {
            result.push(call.call_block(std::slice::from_ref(item))?);
        }
        call.context().array(&result)
    });
    let optional = HostMethod::new_with_block("blocks.optional", |call, _, _| {
        Ok(Value::boolean(call.block_given()))
    });
    let recover = HostMethod::new_with_block("blocks.recover", |call, args, _| {
        match call.call_block(args) {
            Ok(value) => Ok(value),
            Err(error) if error.kind == ErrorKind::ControlFlow => Err(error),
            Err(_) => call.context().bytes(b"recovered"),
        }
    });
    let ignore = HostMethod::new_with_block("blocks.ignore", |call, args, _| {
        if call.call_block(args).is_err() {
            let _ = call.call_block(args);
        }
        Ok(Value::int(99))
    });
    let checked =
        HostMethod::new_with_block("blocks.checked", |call, args, _| call.call_block(args))
            .with_block_contract(
                |_, args, keywords, block| {
                    if args.len() != 1 || !keywords.is_empty() || !block {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            "one argument and block required",
                        ));
                    }
                    Ok(())
                },
                |_, value| {
                    if value.as_int().is_none() {
                        return Err(Error::new(ErrorKind::Type, "integer result required"));
                    }
                    Ok(())
                },
            );
    let keywords = HostMethod::new_with_block("blocks.keywords", |call, args, keywords| {
        let args = call.context().array(args)?;
        let keywords = Value::object(
            keywords
                .iter()
                .map(|(key, value)| {
                    let key = key.as_bytes().ok_or_else(|| {
                        Error::new(ErrorKind::Internal, "block keyword must have a bytes key")
                    })?;
                    Ok((key.to_vec(), value.clone()))
                })
                .collect::<vibescript::Result<_>>()?,
        );
        call.call_block(&[args, keywords])
    });
    Value::object(vec![
        (b"once".to_vec(), once.value()),
        (b"each".to_vec(), each.value()),
        (b"optional".to_vec(), optional.value()),
        (b"recover".to_vec(), recover.value()),
        (b"ignore".to_vec(), ignore.value()),
        (b"checked".to_vec(), checked.value()),
        (b"keywords".to_vec(), keywords.value()),
    ])
}
