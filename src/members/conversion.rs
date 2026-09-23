use crate::{CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, ops, value::Kind};

pub(super) fn call(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
    args: &[Value],
    flags: (bool, bool),
) -> Result<Option<Value>> {
    if site.scope {
        return Ok(None);
    }
    let supported = match receiver.0 {
        Kind::Symbol(_) => matches!(name, "id2name" | "to_s" | "string" | "to_sym"),
        Kind::Nil | Kind::Bool(_) | Kind::Range(_) => matches!(name, "to_s" | "string"),
        _ => false,
    };
    if !supported {
        return Ok(None);
    }
    ctx.charge(1)?;
    if matches!(receiver.0, Kind::Range(_)) {
        let refusal = if !args.is_empty() {
            Some("does not take arguments")
        } else if flags.0 {
            Some("does not take keyword arguments")
        } else if flags.1 {
            Some("does not take a block")
        } else {
            None
        };
        if let Some(refusal) = refusal {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("range.{name} {refusal}"),
            ));
        }
    }
    ops::arity(args, 0)?;
    if flags.0 || flags.1 {
        return Err(Error::new(
            ErrorKind::Argument,
            format!("{name} does not accept keyword arguments or blocks"),
        ));
    }
    let value = if let Kind::Symbol(bytes) = &receiver.0 {
        if name == "to_sym" {
            receiver.clone()
        } else {
            Value(Kind::Bytes(bytes.clone()))
        }
    } else {
        ops::to_string(ctx, receiver)?
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, bytecode::Method};

    #[test]
    fn symbol_aliases_share_accounted_storage_and_release_the_last_owner() {
        for name in ["id2name", "to_s", "string", "to_sym"] {
            let mut ctx = CallContext::new(CallOptions::default());
            let source = ctx.import(&Value::symbol(vec![0xff; 131072])).unwrap();
            let before = ctx.stats();
            let site = CallSite {
                name: 0,
                method: Method::parse(name),
                auto: true,
                parenthesized: false,
                scope: false,
            };
            let value = call(&mut ctx, site, name, &source, &[], (false, false))
                .unwrap()
                .unwrap();
            assert_eq!(
                source.as_bytes().unwrap().as_ptr(),
                value.as_bytes().unwrap().as_ptr()
            );
            assert_eq!(ctx.stats().peak_memory_bytes, before.peak_memory_bytes);
            drop(source);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                before.retained_memory_bytes
            );
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn scalar_conversion_observes_latched_stops_before_reusing_storage() {
        for stop in 0..3 {
            let mut ctx = CallContext::new(CallOptions::default());
            let source = ctx.import(&Value::symbol("name")).unwrap();
            let expected = match stop {
                0 => {
                    ctx.options.limits.steps = Some(ctx.stats().steps);
                    ErrorKind::Steps
                }
                1 => {
                    ctx.options.cancellation.cancel();
                    ErrorKind::Cancelled
                }
                _ => {
                    ctx.options.deadline = Some(std::time::Instant::now());
                    ErrorKind::Deadline
                }
            };
            let latched = if stop == 0 {
                ctx.charge(1).unwrap_err()
            } else {
                ctx.checkpoint().unwrap_err()
            };
            assert_eq!(latched.kind, expected);
            let site = CallSite {
                name: 0,
                method: None,
                auto: true,
                parenthesized: false,
                scope: false,
            };
            let error = call(&mut ctx, site, "id2name", &source, &[], (false, false)).unwrap_err();
            assert_eq!(error, latched);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
            drop(source);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
