use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::Buffer,
    ops,
    value::{Bytes, Kind},
};
use std::cmp::Ordering;

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    let Kind::Bytes(bytes) = &receiver.0 else {
        return Ok(None);
    };
    if !matches!(
        name,
        "concat"
            | "index"
            | "rindex"
            | "hex"
            | "oct"
            | "clamp"
            | "between?"
            | "to_sym"
            | "intern"
            | "to_s"
            | "string"
            | "to_i"
            | "to_f"
    ) {
        return Ok(None);
    }
    ctx.checkpoint()?;
    if !matches!(name, "concat" | "hex" | "oct" | "index" | "rindex") && (keywords || block) {
        return Err(argument("string method does not accept keywords or blocks"));
    }
    let value = match name {
        "concat" => concat(ctx, receiver, args)?,
        "index" | "rindex" => super::index::call(ctx, &bytes.data, args, name == "rindex")?,
        "hex" | "oct" => {
            if !args.is_empty() {
                return Err(argument(&format!("string.{name} does not take arguments")));
            }
            inum(ctx, &bytes.data, name == "oct")?
        }
        "clamp" => clamp(ctx, receiver, args)?,
        "between?" => {
            crate::arguments::between("string.between?", args, false, false)?;
            let lower = ops::compare(ctx, &args[0], receiver)?;
            let within = matches!(lower, Some(Ordering::Less | Ordering::Equal))
                && matches!(
                    ops::compare(ctx, receiver, &args[1])?,
                    Some(Ordering::Less | Ordering::Equal)
                );
            Value::boolean(within)
        }
        _ => {
            ops::arity(args, 0)?;
            match name {
                "to_sym" | "intern" => Value(Kind::Symbol(bytes.clone())),
                "to_s" | "string" => receiver.clone(),
                "to_i" => crate::conversion::integer(ctx, &bytes.data, "string.to_i")?,
                "to_f" => Value::float(crate::conversion::float(ctx, &bytes.data, "string.to_f")?),
                _ => unreachable!(),
            }
        }
    };
    Ok(Some(value))
}

fn concat(ctx: &mut CallContext, receiver: &Value, args: &[Value]) -> Result<Value> {
    let mut length = receiver.require_bytes()?.len();
    for value in args {
        ctx.charge(1)?;
        let Kind::Bytes(bytes) = &value.0 else {
            return Err(Error::new(
                ErrorKind::Type,
                "concat expects string arguments",
            ));
        };
        length = match length.checked_add(bytes.data.len()) {
            Some(length) => length,
            None => return ctx.fail(ErrorKind::Memory, "concatenation size overflow"),
        };
    }
    if length == receiver.require_bytes()?.len() {
        return Ok(receiver.clone());
    }
    let Some(projected) = length.checked_add(Bytes::header_bytes()) else {
        return ctx.fail(ErrorKind::Memory, "concatenation size overflow");
    };
    ctx.check_memory(projected)?;
    let mut output = Buffer::with_capacity(ctx, length)?;
    output.extend(ctx, receiver.require_bytes()?)?;
    for value in args {
        output.extend(ctx, value.require_bytes()?)?;
    }
    Value::from_bytes(ctx, output)
}

fn clamp(ctx: &mut CallContext, receiver: &Value, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(argument("string.clamp expects min and max"));
    }
    for value in args {
        if !matches!(value.0, Kind::Bytes(_) | Kind::Nil) {
            return Err(argument("string.clamp bounds must be strings or nil"));
        }
    }
    let lower = !matches!(args[0].0, Kind::Nil);
    let upper = !matches!(args[1].0, Kind::Nil);
    if lower && upper && ops::compare(ctx, &args[0], &args[1])? == Some(Ordering::Greater) {
        return Err(argument("string.clamp min must be <= max"));
    }
    if lower && ops::compare(ctx, receiver, &args[0])? == Some(Ordering::Less) {
        return Ok(args[0].clone());
    }
    if upper && ops::compare(ctx, receiver, &args[1])? == Some(Ordering::Greater) {
        return Ok(args[1].clone());
    }
    Ok(receiver.clone())
}

fn inum(ctx: &mut CallContext, text: &[u8], detect_base: bool) -> Result<Value> {
    let overflow = || {
        Error::new(
            ErrorKind::Arithmetic,
            if detect_base {
                "string.oct integer out of range"
            } else {
                "string.hex integer out of range"
            },
        )
    };
    let mut position = 0;
    while position < text.len() && super::split::space(text[position]) {
        ctx.charge(1)?;
        position += 1;
    }
    let negative = text.get(position) == Some(&b'-');
    if matches!(text.get(position), Some(b'+' | b'-')) {
        position += 1;
    }
    let mut base = if detect_base { 8u64 } else { 16 };
    if text.get(position) == Some(&b'0') {
        let prefix = match text.get(position + 1) {
            Some(b'x' | b'X') => Some(16),
            Some(b'b' | b'B') if detect_base => Some(2),
            Some(b'o' | b'O') if detect_base => Some(8),
            Some(b'd' | b'D') if detect_base => Some(10),
            _ => None,
        };
        if let Some(next) = prefix {
            base = next;
            position += 2;
        }
    }
    let mut magnitude = 0u64;
    let mut parsed = false;
    let mut underscore = false;
    while let Some(&byte) = text.get(position) {
        ctx.charge(1)?;
        if byte == b'_' {
            if !parsed || underscore {
                break;
            }
            underscore = true;
            position += 1;
            continue;
        }
        let digit = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            b'a'..=b'f' => u64::from(byte - b'a') + 10,
            b'A'..=b'F' => u64::from(byte - b'A') + 10,
            _ => break,
        };
        if digit >= base {
            break;
        }
        magnitude = magnitude
            .checked_mul(base)
            .and_then(|n| n.checked_add(digit))
            .ok_or_else(overflow)?;
        parsed = true;
        underscore = false;
        position += 1;
    }
    let value = if negative {
        -(magnitude as i128)
    } else {
        magnitude as i128
    };
    i64::try_from(value).map(Value::int).map_err(|_| overflow())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn concatenation_preflights_exact_storage_and_reclaims_the_result() {
        for short in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let receiver = ctx.bytes(&vec![b'a'; 8192]).unwrap();
            let suffix = ctx.bytes(b"tail").unwrap();
            let baseline = ctx.stats();
            let storage = Bytes::header_bytes() + 8196;
            ctx.options.limits.memory_bytes =
                Some(baseline.retained_memory_bytes + storage - usize::from(short));
            let output = concat(&mut ctx, &receiver, &[suffix]);
            if short {
                assert_eq!(output.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
                assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
            } else {
                let output = output.unwrap();
                assert_eq!(&output.as_bytes().unwrap()[8192..], b"tail");
                assert_eq!(
                    ctx.stats().peak_memory_bytes,
                    baseline.retained_memory_bytes + storage
                );
                drop(output);
            }
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                Bytes::header_bytes() + 8192
            );
        }
    }

    #[test]
    fn symbol_conversion_shares_storage_and_imports_keep_independent_charges() {
        let mut origin = CallContext::new(CallOptions::default());
        let input = origin.bytes(b"name\xff\0").unwrap();
        let baseline = origin.stats().retained_memory_bytes;
        origin.options.limits.memory_bytes = Some(baseline);
        let symbol = call(&mut origin, "to_sym", &input, &[], false, false)
            .unwrap()
            .unwrap();
        assert_eq!(symbol.type_name(), "symbol");
        assert_eq!(
            symbol.as_bytes().unwrap().as_ptr(),
            input.as_bytes().unwrap().as_ptr()
        );
        assert_eq!(origin.stats().peak_memory_bytes, baseline);
        drop(input);
        assert_eq!(origin.stats().retained_memory_bytes, baseline);
        let mut other = CallContext::new(CallOptions::default());
        let imported = other.import(&symbol).unwrap();
        assert_eq!(other.stats().retained_memory_bytes, baseline);
        drop(symbol);
        assert_eq!(origin.stats().retained_memory_bytes, 0);
        assert_eq!(imported.as_bytes().unwrap(), b"name\xff\0");
        assert_eq!(other.stats().retained_memory_bytes, baseline);
        drop(imported);
        assert_eq!(other.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn bounds_reuse_values_and_long_comparisons_observe_work_limits() {
        let mut ctx = CallContext::new(CallOptions::default());
        let receiver = ctx.bytes(b"a").unwrap();
        let lower = ctx.bytes(b"m").unwrap();
        let upper = ctx.bytes(b"z").unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(baseline);
        let output = clamp(&mut ctx, &receiver, &[lower.clone(), upper]).unwrap();
        assert_eq!(
            output.as_bytes().unwrap().as_ptr(),
            lower.as_bytes().unwrap().as_ptr()
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline);

        let mut ctx = CallContext::new(CallOptions::default());
        let receiver = ctx.bytes(&vec![b'a'; 65536]).unwrap();
        let bound = ctx.bytes(&vec![b'a'; 65536]).unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.steps = Some(baseline.steps + 1);
        assert_eq!(
            clamp(&mut ctx, &receiver, &[bound, Value::nil()])
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    }

    #[test]
    fn radix_scans_use_constant_storage_and_latch_work_exhaustion() {
        for octal in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            ctx.options.limits.memory_bytes = Some(0);
            assert_eq!(
                inum(&mut ctx, b"  -17_tail", octal).unwrap().as_int(),
                Some(if octal { -15 } else { -23 })
            );
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
            ctx.options.limits.steps = Some(ctx.stats().steps + 20);
            assert_eq!(
                inum(&mut ctx, &vec![b'0'; 65536], octal).unwrap_err().kind,
                ErrorKind::Steps
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
        }
    }
}
