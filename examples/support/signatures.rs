use serde_json::Value as Json;
use vibescript::{
    CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, Signature, SignatureParam, Value,
};

pub fn configure(engine: &mut Engine, probe: &Json) -> vibescript::Result<HostMethod> {
    let registration = probe["registration"].as_str().unwrap_or("capability");
    let name = if registration == "capability" {
        "typed.echo"
    } else {
        "echo"
    };
    let callback = probe["callback"].as_str().unwrap_or("echo").to_owned();
    let method = HostMethod::new_with_block(name, move |call, args, _| match callback.as_str() {
        "block" => call.call_block(args),
        "list" => call.context().array(args),
        "symbol" => Ok(Value::symbol("draft")),
        "bad" => call.context().bytes(b"bad"),
        "kind" => call.context().bytes(args[0].type_name().as_bytes()),
        _ => Ok(args[0].clone()),
    })
    .with_signature(Signature {
        params: probe["params"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|param| SignatureParam {
                name: param["name"].as_str().unwrap_or("").into(),
                ty: param["type"].as_str().unwrap_or("").into(),
                optional: param["optional"].as_bool().unwrap_or(false),
            })
            .collect(),
        result: probe["result"].as_str().unwrap_or("").into(),
        accepts_block: probe["accepts_block"].as_bool().unwrap_or(false),
    })?;
    let method = if probe["contract"].as_bool().unwrap_or(false) {
        method.with_contract(
            |_, args, _| {
                if args[0].type_name() != "symbol" {
                    return Err(Error::new(ErrorKind::Type, "raw symbol required"));
                }
                Ok(())
            },
            |_, value| {
                if value.as_enum_member().is_none() {
                    return Err(Error::new(ErrorKind::Type, "normalized enum required"));
                }
                Ok(())
            },
        )
    } else {
        method
    };
    if registration == "registered" {
        engine.register_method("echo", method.clone());
    }
    Ok(method)
}

pub fn bind(options: &mut CallOptions, probe: &Json, method: HostMethod) {
    match probe["registration"].as_str().unwrap_or("capability") {
        "registered" => (),
        "global" => {
            options.globals.insert("echo".into(), method.value());
        }
        _ => options
            .capabilities
            .push(Capability::new("typed", move |_| {
                Ok(Value::object(vec![(b"echo".to_vec(), method.value())]))
            })),
    }
}
