use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, value::Kind};

pub(crate) fn contains(ctx: &mut CallContext, values: &[Value], key: &Value) -> Result<bool> {
    for value in values {
        ctx.charge(1)?;
        // Scalar set keys separate kinds and collapse NaNs. Nested values use
        // ordinary equality, including numeric coercion and unequal NaNs.
        if matches!((&value.0, &key.0), (Kind::Float(a), Kind::Float(b)) if a.is_nan() && b.is_nan())
        {
            return Ok(true);
        }
        let same_type = std::mem::discriminant(&value.0) == std::mem::discriminant(&key.0)
            || (crate::time::stamp(value).is_some() && crate::time::stamp(key).is_some());
        if same_type && ops::equal(ctx, value, key, 0)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<Option<Value>> {
    let Some(array) = receiver.as_array() else {
        return Ok(None);
    };
    if !matches!(name, "union" | "difference") {
        return Ok(None);
    }
    ctx.checkpoint()?;
    if keywords {
        return Err(Error::new(
            ErrorKind::Argument,
            format!("array.{name} does not accept keyword arguments"),
        ));
    }
    for arg in args {
        ctx.charge(1)?;
        require_array(arg)?;
    }
    let value = if name == "union" {
        let mut output = Buffer::empty();
        append_unique(ctx, &mut output, array)?;
        for arg in args {
            ctx.charge(1)?;
            append_unique(ctx, &mut output, arg.as_array().unwrap())?;
        }
        Value::from_array(ctx, output)?
    } else {
        difference(ctx, array, args)?
    };
    Ok(Some(value))
}

fn append_unique(
    ctx: &mut CallContext,
    output: &mut Buffer<Value>,
    values: &[Value],
) -> Result<()> {
    for value in values {
        ctx.charge(1)?;
        if !contains(ctx, &output.data, value)? {
            output.push(ctx, value.clone())?;
        }
    }
    Ok(())
}

pub(crate) fn binary(
    ctx: &mut CallContext,
    op: &str,
    left: &Value,
    right: &Value,
) -> Result<Value> {
    ctx.checkpoint()?;
    let array = require_array(left)?;
    let other = require_array(right)?;
    if op == "-" {
        return difference(ctx, array, std::slice::from_ref(right));
    }
    let mut output = Buffer::empty();
    if !other.is_empty() {
        for value in array {
            ctx.charge(1)?;
            if contains(ctx, other, value)? && !contains(ctx, &output.data, value)? {
                output.push(ctx, value.clone())?;
            }
        }
    }
    Value::from_array(ctx, output)
}

fn difference(ctx: &mut CallContext, array: &[Value], others: &[Value]) -> Result<Value> {
    if others.is_empty() {
        return ctx.array(array);
    }
    let mut output = Buffer::empty();
    'item: for value in array {
        ctx.charge(1)?;
        for other in others {
            ctx.charge(1)?;
            if contains(ctx, other.as_array().unwrap(), value)? {
                continue 'item;
            }
        }
        output.push(ctx, value.clone())?;
    }
    Value::from_array(ctx, output)
}

fn require_array(value: &Value) -> Result<&[Value]> {
    value.as_array().ok_or_else(|| {
        Error::new(
            ErrorKind::Type,
            "array set operations require array operands",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn small_results_drop_input_backing_and_receive_independent_import_charges() {
        for operation in ["union", "difference", "&", "-"] {
            let mut ctx = CallContext::new(CallOptions::default());
            let large = Value::bytes(vec![b'x'; 65536]);
            let input = if operation == "union" {
                Value::array(vec![Value::int(1); 4096])
            } else {
                Value::array(vec![large.clone(), Value::int(1)])
            };
            let input = ctx.import(&input).unwrap();
            let other = if operation == "&" {
                Value::array(vec![Value::int(1)])
            } else {
                Value::array(vec![large])
            };
            let other = ctx.import(&other).unwrap();
            let result = match operation {
                "&" | "-" => binary(&mut ctx, operation, &input, &other).unwrap(),
                _ => call(
                    &mut ctx,
                    operation,
                    &input,
                    if operation == "union" {
                        &[]
                    } else {
                        std::slice::from_ref(&other)
                    },
                    false,
                )
                .unwrap()
                .unwrap(),
            };
            assert_eq!(result.as_array().unwrap().len(), 1);
            assert_eq!(result.as_array().unwrap()[0].as_int(), Some(1));
            drop(input);
            drop(other);
            assert!(ctx.stats().retained_memory_bytes < 1024);
            let mut imported = CallContext::new(CallOptions::default());
            let copy = imported.import(&result).unwrap();
            assert!(imported.stats().retained_memory_bytes > 0);
            drop(result);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(copy.as_array().unwrap()[0].as_int(), Some(1));
            drop(copy);
            assert_eq!(imported.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn rejected_kinds_and_nested_comparisons_consume_bounded_work() {
        let different_kinds = vec![Value::nil(); 4096];
        let long = Value::array(vec![Value::int(0); 4096]);
        let deep = Value::array(vec![long.clone(), long]);
        for (values, key) in [
            (different_kinds, Value::int(1)),
            (vec![deep.clone()], deep),
            (
                vec![Value::bytes(vec![b'a'; 65536])],
                Value::bytes(vec![b'a'; 65536]),
            ),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            assert_eq!(
                contains(&mut ctx, &values, &key).unwrap_err().kind,
                ErrorKind::Steps
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Steps);
        }
    }

    #[test]
    fn partial_results_are_released_after_step_and_memory_failures() {
        let input = Value::array((0..128).map(Value::int).collect::<Vec<_>>());
        let other = input.clone();
        for operation in ["union", "difference", "&", "-"] {
            for kind in [ErrorKind::Steps, ErrorKind::Memory] {
                let mut ctx = CallContext::new(CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(64),
                        memory_bytes: (kind == ErrorKind::Memory).then_some(128),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                });
                let empty = Value::array(Vec::new());
                let result = match operation {
                    "&" => binary(&mut ctx, operation, &input, &other),
                    "-" => binary(&mut ctx, operation, &input, &empty),
                    _ => call(&mut ctx, operation, &input, &[], false).map(Option::unwrap),
                };
                assert_eq!(result.unwrap_err().kind, kind, "{operation}");
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
                assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
            }
        }
    }

    #[test]
    fn cancelled_and_expired_operations_stop_even_on_empty_inputs() {
        let empty = Value::array(Vec::new());
        for operation in ["union", "difference", "&", "-"] {
            for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
                let mut options = CallOptions::default();
                if kind == ErrorKind::Cancelled {
                    options.cancellation.cancel();
                } else {
                    options.deadline = Some(std::time::Instant::now());
                }
                let mut ctx = CallContext::new(options);
                let result = match operation {
                    "&" | "-" => binary(&mut ctx, operation, &empty, &empty),
                    _ => call(&mut ctx, operation, &empty, &[], false).map(Option::unwrap),
                };
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(ctx.stats().peak_memory_bytes, 0);
                assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
            }
        }
    }
}
