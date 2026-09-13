use std::sync::Arc;
use vibescript::{CallContext, CallOptions, Engine, Error, ErrorKind, Result, Value};

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

fn engine(client: Arc<SmsPreview>) -> Engine {
    let mut engine = Engine::new();
    engine.register("sms_send", move |ctx, args| {
        let [phone, body] = args else {
            return Err(Error::new(
                ErrorKind::Argument,
                "sms_send expects phone and body",
            ));
        };
        ctx.charge(1)?;
        client.send(ctx, text(phone)?, text(body)?)
    });
    engine
}

fn main() -> Result<()> {
    let engine = engine(Arc::new(SmsPreview {
        sender: "Demo".into(),
    }));
    let script = engine.compile(
        "def delivery_update(phone, order_id)\n sms_send(phone, \"Order \" + order_id + \" is on its way.\")\nend",
    )?;
    let output = script.call(
        "delivery_update",
        &[Value::bytes("+12025550123"), Value::bytes("1042")],
        CallOptions::default(),
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
        let engine = engine(Arc::new(SmsPreview {
            sender: "Demo".into(),
        }));
        for (source, kind) in [
            ("sms_send(1,\"body\")", ErrorKind::Type),
            ("sms_send(\"phone\")", ErrorKind::Argument),
            ("sms_send(\"phone\",\"\\xff\")", ErrorKind::Argument),
        ] {
            assert_eq!(
                engine
                    .compile(source)
                    .unwrap()
                    .run(CallOptions::default())
                    .unwrap_err()
                    .kind,
                kind
            );
        }
        let script = engine.compile("sms_send(\"phone\",\"body\")").unwrap();
        let options = CallOptions::default();
        options.cancellation.cancel();
        assert_eq!(script.run(options).unwrap_err().kind, ErrorKind::Cancelled);
        assert_eq!(
            Engine::new()
                .compile("sms_send(\"phone\",\"body\")")
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Name
        );
        main().unwrap();
    }
}
