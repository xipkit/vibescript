use serde_json::{Value as Json, json};
use vibescript::{CallOptions, Capability, Error, ErrorKind, HostMethod, Value, stringify_json};

/// Encodes comparison results without changing the language's JSON contract.
pub fn encode(
    value: &Value,
    encoding: &str,
    options: CallOptions,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    match encoding {
        "" | "json" => Ok(stringify_json(value, options)?
            .value
            .as_bytes()
            .unwrap()
            .to_vec()),
        "typed" => Ok(serde_json::to_vec(&json!(["typed-v1", typed(value, 0)?]))?),
        _ => Err(format!("unknown result encoding {encoding:?}").into()),
    }
}

fn typed(value: &Value, depth: usize) -> Result<Json, Box<dyn std::error::Error>> {
    if depth > 256 {
        return Err("typed result nesting exceeds 256".into());
    }
    let kind = value.type_name();
    Ok(match kind {
        "nil" => json!([kind]),
        "bool" => json!([kind, value.truthy()]),
        "int" => json!([kind, value.to_string()]),
        "float" => json!([
            kind,
            format!("{:016x}", value.as_float().unwrap().to_bits())
        ]),
        "string" | "symbol" => json!([kind, hex(value.as_bytes().unwrap())]),
        "money" => {
            let (cents, currency) = value.as_money().unwrap();
            json!([kind, currency, cents.to_string()])
        }
        "duration" => json!([kind, value.as_duration().unwrap().to_string()]),
        "array" => {
            let items = value
                .as_array()
                .unwrap()
                .iter()
                .map(|value| typed(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            json!([kind, items])
        }
        "hash" | "object" => {
            let entries = value
                .as_hash()
                .unwrap()
                .iter()
                .map(|(key, value)| {
                    Ok(json!([
                        hex(key.as_bytes().unwrap()),
                        typed(value, depth + 1)?
                    ]))
                })
                .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
            json!([kind, entries])
        }
        _ => return Err(format!("unsupported typed result {kind}").into()),
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 15) as usize] as char);
    }
    output
}

/// Supplies the website's deterministic notification previews.
pub fn notification(name: &str) -> vibescript::Result<Capability> {
    let fields: &[&str] = match name {
        "sms" => &["to", "body"],
        "email" => &["to", "subject", "body"],
        _ => return Err(Error::new(ErrorKind::Name, "unknown notification preview")),
    };
    let send = HostMethod::new(format!("{name}.send"), move |ctx, args, _| {
        ctx.charge(args.len() as u64)?;
        let mut entries = vec![(b"status".to_vec(), Value::bytes("preview"))];
        entries.extend(
            fields
                .iter()
                .zip(args)
                .map(|(name, value)| (name.as_bytes().to_vec(), value.clone())),
        );
        Ok(Value::hash(entries))
    })
    .with_contract(
        move |_, args, keywords| {
            if args.len() != fields.len() || !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "invalid notification arguments",
                ));
            }
            if args.iter().any(|value| value.type_name() != "string") {
                return Err(Error::new(
                    ErrorKind::Type,
                    "notification arguments must be strings",
                ));
            }
            Ok(())
        },
        |_, value| {
            if value.type_name() != "hash" {
                return Err(Error::new(
                    ErrorKind::Type,
                    "notification preview must be a hash",
                ));
            }
            Ok(())
        },
    );
    Ok(Capability::new(name, move |_| {
        Ok(Value::object(vec![(b"send".to_vec(), send.value())]))
    }))
}
