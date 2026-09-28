use std::sync::Arc;
use vibescript::{
    CallContext, CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, Result, Signature,
    SignatureParam, Value,
};

struct SmsPreview {
    sender: String,
}

impl SmsPreview {
    fn send(&self, ctx: &mut CallContext, phone: &str, body: &str) -> Result<Value> {
        ctx.checkpoint()?;
        let preview = format!("{} -> {phone}: {body}", self.sender);
        ctx.bytes(preview.as_bytes())
    }
}

fn text(value: &Value) -> Result<&str> {
    if value.type_name() != "string" {
        return Err(Error::new(ErrorKind::Type, "SMS arguments must be strings"));
    }
    std::str::from_utf8(value.as_bytes().unwrap())
        .map_err(|_| Error::new(ErrorKind::Argument, "SMS arguments must be valid UTF-8"))
}

/// A text parameter of the published signature.
fn text_param(name: &str) -> SignatureParam {
    SignatureParam {
        name: name.into(),
        ty: "string".into(),
        optional: false,
    }
}

/// The `sms` capability: a template whose `send` method publishes its
/// signature, so the static checker types calls to it.
fn capability(client: Arc<SmsPreview>) -> Result<Capability> {
    let send = HostMethod::new("sms.send", move |ctx, args, _| {
        for value in args {
            ctx.charge(value.as_bytes().unwrap().len() as u64)?;
        }
        client.send(ctx, text(&args[0])?, text(&args[1])?)
    })
    .with_contract(
        |_, args, keywords| {
            if args.len() != 2 || !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "sms.send expects phone and body",
                ));
            }
            if args.iter().any(|value| value.type_name() != "string") {
                return Err(Error::new(ErrorKind::Type, "SMS arguments must be strings"));
            }
            Ok(())
        },
        |_, value| {
            if value.type_name() != "string" {
                return Err(Error::new(
                    ErrorKind::Type,
                    "sms.send must return a preview string",
                ));
            }
            Ok(())
        },
    )
    .with_signature(Signature {
        params: vec![text_param("phone"), text_param("body")],
        result: "string".into(),
        accepts_block: false,
    })?;
    Ok(Capability::from_value(
        "sms",
        Value::object(vec![(b"send".to_vec(), send.value())]),
    ))
}

fn main() -> Result<()> {
    let sms = capability(Arc::new(SmsPreview {
        sender: "Demo".into(),
    }))?;
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine.declare_capability(&sms)?;
    let script = engine.compile(
        "def delivery_update(phone: string, order_id: string) -> string\n  sms.send(phone, \"Order #{order_id} is on its way.\")\nend",
    )?;
    let output = script.call(
        "delivery_update",
        &[Value::bytes("+12025550123"), Value::bytes("1042")],
        CallOptions {
            capabilities: vec![sms],
            ..CallOptions::default()
        },
    )?;
    assert_eq!(
        output.value.as_bytes(),
        Some(b"Demo -> +12025550123: Order 1042 is on its way.".as_slice())
    );
    println!("{}", output.value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sms_preview_checks_arguments_and_respects_cancellation() {
        let sms = capability(Arc::new(SmsPreview {
            sender: "Demo".into(),
        }))
        .unwrap();
        let mut engine = Engine::new();
        engine.set_strict_effects(true);
        engine.declare_capability(&sms).unwrap();
        // The published signature types every call, so arguments that do
        // not match it are refused before the script runs.
        for (source, code) in [
            ("sms.send(1,\"body\")", "V0101"),
            ("sms.send(\"phone\")", "V0301"),
            ("sms.send(\"phone\",\"body\", extra: 1)", "V0302"),
        ] {
            let error = engine.compile(source).err().unwrap();
            assert_eq!(error.diagnostics()[0].code.to_string(), code, "{source}");
        }
        let options = CallOptions {
            capabilities: vec![sms],
            ..CallOptions::default()
        };
        // Bytes that are not UTF-8 are still a string, which the host
        // rejects when the call runs.
        assert_eq!(
            engine
                .compile("sms.send(\"phone\",\"\\xff\")")
                .unwrap()
                .run(options.clone())
                .unwrap_err()
                .kind,
            ErrorKind::Argument
        );
        let script = engine.compile("sms.send(\"phone\",\"body\")").unwrap();
        options.cancellation.cancel();
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Cancelled);
        let undeclared = Engine::new()
            .compile("sms.send(\"phone\",\"body\")")
            .err()
            .unwrap();
        assert_eq!(undeclared.diagnostics()[0].code.to_string(), "V0201");
        main().unwrap();
    }
}
