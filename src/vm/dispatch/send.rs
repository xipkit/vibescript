use super::*;
use crate::members::{forwarding, names};

pub(super) fn call(
    program: &Program,
    ctx: &mut CallContext,
    frames: &mut Buffer<Frame>,
    storage: &mut Storage,
    stack: &mut Buffer<Value>,
    mut call: Call<'_>,
    mut captured: Option<Value>,
) -> Result<()> {
    let mut consumed = 0;
    let mut helper = call.name;
    loop {
        ctx.charge(1)?;
        let operation = forwarding::method(helper, &call.args.positional.data[consumed..])?.clone();
        let bytes = operation.require_bytes()?;
        let name = members::introspection::method_name(ctx, bytes)?;
        let receiver = if let Some(receiver) = &captured {
            receiver.clone()
        } else if call.mutating {
            storage.addresses.data.last().unwrap().value.clone()
        } else {
            stack.data.last().unwrap().clone()
        };
        let access = namespaces::Access {
            implicit: helper == "send",
            ..call.access
        };
        let site = CallSite {
            name: 0,
            method: name.and_then(crate::bytecode::Method::parse),
            auto: false,
            scope: false,
        };
        let selected = resolve(
            program,
            ctx,
            storage,
            &receiver,
            (bytes, name),
            site,
            access,
        )?;
        consumed += 1;
        if matches!(selected, namespaces::Member::Missing)
            && name.is_some_and(forwarding::supported)
        {
            helper = if name == Some("send") {
                "send"
            } else {
                "public_send"
            };
            continue;
        }
        let name = name.unwrap_or("");
        let mutating = matches!(selected, namespaces::Member::Missing)
            && crate::bytecode::mutating_member(name);
        call.args.skip(ctx, consumed)?;
        call.args.options_hash = true;
        if call.mutating && !mutating {
            let address = storage.addresses.data.pop().unwrap();
            stack.push(ctx, captured.take().unwrap_or(address.value))?;
        } else if !call.mutating && mutating {
            let value = stack.data.pop().unwrap();
            storage.addresses.push(ctx, Address::new(None, value))?;
        }
        drop(receiver);
        drop(captured);
        return invoke(
            program,
            ctx,
            frames,
            storage,
            stack,
            Call {
                site,
                name,
                mutating,
                args: call.args,
                access,
            },
            selected,
        );
    }
}

fn resolve(
    program: &Program,
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: &Value,
    method: (&[u8], Option<&str>),
    site: CallSite,
    access: namespaces::Access,
) -> Result<namespaces::Member> {
    use namespaces::Member;
    let (bytes, name) = method;
    let Some(name) = name else {
        if let Kind::Hash(hash) = &receiver.0 {
            if let Some(index) = hash.find(ctx, bytes)? {
                return Ok(Member::Value(hash.buffer.data[index].1.clone()));
            }
        }
        return Err(forwarding::unknown(ctx, receiver, bytes)?);
    };
    if matches!(receiver.0, Kind::Namespace(_) | Kind::Instance(_)) {
        let selected = namespaces::member(program, ctx, storage, receiver, site, name, access)?;
        if !matches!(selected, Member::Missing) || names::universal(name) {
            return Ok(selected);
        }
        return Err(forwarding::unknown(ctx, receiver, bytes)?);
    }
    if let Kind::Hash(hash) = &receiver.0 {
        let universal = names::universal(name);
        let data_safe = universal && !matches!(name, "tap" | "yield_self");
        if !hash.object && (data_safe || names::typed(receiver, name).is_some()) {
            return Ok(Member::Missing);
        }
        if let Some(index) = hash.find(ctx, bytes)? {
            let value = &hash.buffer.data[index].1;
            if !data_safe || members::introspection::callable(value) {
                return Ok(Member::Value(value.clone()));
            }
        }
        if universal || names::available(receiver, name) {
            return Ok(Member::Missing);
        }
        return Err(forwarding::unknown(ctx, receiver, bytes)?);
    }
    if names::property(receiver, name) {
        let (_, value) = members::call(
            ctx,
            CallSite { auto: true, ..site },
            name,
            receiver.clone(),
            &[],
        )?;
        return Ok(Member::Value(value));
    }
    if names::universal(name) || names::available(receiver, name) {
        return Ok(Member::Missing);
    }
    if let Kind::Builtin(builtin) = receiver.0 {
        return Err(builtin.value_error());
    }
    if let Kind::Offset(offset) = &receiver.0 {
        return Err(offset.value_error());
    }
    Err(forwarding::unknown(ctx, receiver, bytes)?)
}
