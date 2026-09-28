use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{Capability, Error, ErrorKind, HostMethod, Value};

/// Builds the `host` capability that the capability conformance cases probe.
pub fn capability() -> Capability {
    Capability::new("host", |_| Ok(template()))
}

/// A fresh value of the `host` capability, which each call binds and which
/// also declares the capability to a statically typed engine.
pub fn template() -> Value {
    let count = Arc::new(AtomicUsize::new(0));
    let next_count = count.clone();
    let next = HostMethod::new("host.next", move |_, _, _| {
        Ok(Value::int(
            next_count.fetch_add(1, Ordering::Relaxed) as i64 + 1,
        ))
    });
    let checked = HostMethod::new("host.checked", move |_, args, _| {
        count.fetch_add(1, Ordering::Relaxed);
        Ok(if args[0].as_int() == Some(0) {
            Value::bytes("invalid result")
        } else {
            args[0].clone()
        })
    })
    .with_contract(
        |_, args, keywords| {
            if args.len() != 1 || args[0].as_int().is_none() || !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "host.checked expects one integer",
                ));
            }
            Ok(())
        },
        |_, value| {
            if value.as_int().is_none() {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "host.checked must return an integer",
                ));
            }
            Ok(())
        },
    );
    let nested = checked.clone();
    let factory = HostMethod::new("host.factory", move |_, _, _| {
        Ok(Value::object(vec![(b"checked".to_vec(), nested.value())]))
    });
    let echo = HostMethod::new("host.echo", |ctx, args, keywords| {
        let options = Value::hash(
            keywords
                .iter()
                .map(|(key, value)| (key.as_bytes().unwrap().to_vec(), value.clone()))
                .collect(),
        );
        let args = ctx.array(args)?;
        ctx.array(&[args, options])
    });
    let fail = HostMethod::new("host.fail", |_, _, _| {
        Err(Error::new(ErrorKind::Runtime, "host failure"))
    });
    Value::object(vec![
        (b"next".to_vec(), next.value()),
        (b"checked".to_vec(), checked.value()),
        (b"factory".to_vec(), factory.value()),
        (b"echo".to_vec(), echo.value()),
        (b"map".to_vec(), echo.value()),
        (b"fail".to_vec(), fail.value()),
        (b"items".to_vec(), Value::array(vec![Value::int(1)])),
    ])
}
