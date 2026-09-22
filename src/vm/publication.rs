//! Host writes into a capability object during the call that granted it.
//!
//! A capability binding is the host's live state for one invocation. A host
//! method publishes by storing a field in the object it was called on; the
//! write lands in the capability binding that currently holds that object, so
//! later script reads, blocks and host calls observe it. Copies the script made
//! earlier stay independent values.

use super::*;

/// The capability binding and nested hash keys that lead to a receiver.
pub(super) struct Location {
    name: String,
    path: Buffer<Value>,
}

/// Stores `key: value` in `receiver`, publishing the write to the capability
/// binding that holds it. Returns whether a binding received the write.
///
/// `location` caches where the receiver was found, so writes the script makes
/// to the binding between two publications do not detach the receiver.
pub(super) fn set_field(
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: &mut Value,
    location: &mut Option<Location>,
    key: &[u8],
    value: &Value,
) -> Result<bool> {
    ctx.checkpoint()?;
    let Kind::Hash(hash) = &receiver.0 else {
        return Err(Error::new(
            ErrorKind::Type,
            "host receiver does not accept fields",
        ));
    };
    if let Some(index) = hash.find(ctx, key)? {
        if matches!(hash.buffer.data[index].1.0, Kind::Host(_)) {
            return Err(Error::new(
                ErrorKind::Type,
                format!(
                    "cannot replace capability method {}",
                    String::from_utf8_lossy(key)
                ),
            ));
        }
    }
    let key = ctx.bytes(key)?;
    let value = ctx.snapshot(value)?;
    if !matches!(value.0, Kind::Host(_)) {
        crate::exports::check(ctx, &value)?;
    }
    programs::imported(ctx, storage, &value)?;
    if location.is_none() {
        *location = locate(ctx, storage, receiver)?;
    }
    if let Some(found) = location {
        if let Some(mut address) = open(ctx, storage, found)? {
            address.selectors.push(ctx, key.clone())?;
            address.assign(
                ctx,
                address::Bindings {
                    guard: None,
                    recover: true,
                    locals: &mut storage.locals.data,
                    globals: &mut storage.globals.data,
                    namespaces: &mut storage.namespaces.data,
                },
                &mut storage.addresses.data,
                value.clone(),
            )?;
            if let Some(current) = read(ctx, storage, found)? {
                *receiver = current;
                return Ok(true);
            }
        }
        *location = None;
    }
    *receiver = ops::set_index(ctx, std::mem::take(receiver), key, value)?;
    Ok(false)
}

/// Finds the capability binding holding `receiver`, by identity.
fn locate(ctx: &mut CallContext, storage: &Storage, receiver: &Value) -> Result<Option<Location>> {
    let (Kind::Hash(target), Some(bindings)) = (&receiver.0, &storage.bindings) else {
        return Ok(None);
    };
    for index in 0..ctx.capability_names.data.len() {
        ctx.charge(1)?;
        let name = ctx.capability_names.data[index].clone();
        let name = String::from_utf8_lossy(name.as_bytes().unwrap()).into_owned();
        let Some(root) = crate::objects::field(ctx, bindings, &name)? else {
            continue;
        };
        if let Some(path) = find(ctx, &root, target)? {
            return Ok(Some(Location { name, path }));
        }
    }
    Ok(None)
}

/// Searches nested hashes below `root` for `target`, returning the keys that
/// reach it.
fn find(ctx: &mut CallContext, root: &Value, target: &Arc<Hash>) -> Result<Option<Buffer<Value>>> {
    let Kind::Hash(root) = &root.0 else {
        return Ok(None);
    };
    let mut path = Buffer::empty();
    if Arc::ptr_eq(root, target) {
        return Ok(Some(path));
    }
    let mut pending: Buffer<(Arc<Hash>, usize)> = Buffer::empty();
    pending.push(ctx, (root.clone(), 0))?;
    while let Some((hash, next)) = pending.data.last_mut() {
        ctx.charge(1)?;
        let entry = hash.buffer.data.get(*next).cloned();
        *next += 1;
        let Some((key, child)) = entry else {
            pending.data.pop();
            path.data.pop();
            continue;
        };
        let Kind::Hash(child) = child.0 else {
            continue;
        };
        let found = Arc::ptr_eq(&child, target);
        if !found && pending.data.len() >= crate::budget::MAX_VALUE_DEPTH {
            continue;
        }
        path.push(ctx, key)?;
        if found {
            return Ok(Some(path));
        }
        pending.push(ctx, (child, 0))?;
    }
    Ok(None)
}

/// Opens a write address at the located receiver, or `None` if the binding no
/// longer reaches a hash there.
fn open(ctx: &mut CallContext, storage: &Storage, location: &Location) -> Result<Option<Address>> {
    if read(ctx, storage, location)?.is_none() {
        return Ok(None);
    }
    let bindings = storage.bindings.as_ref().unwrap();
    let mut address = crate::objects::address(ctx, bindings, &location.name)?.in_environment();
    for key in &location.path.data {
        address.index(ctx, std::slice::from_ref(key))?;
    }
    Ok(address.has_binding().then_some(address))
}

/// Reads the hash currently stored at `location`.
fn read(ctx: &mut CallContext, storage: &Storage, location: &Location) -> Result<Option<Value>> {
    let Some(bindings) = &storage.bindings else {
        return Ok(None);
    };
    let Some(mut value) = crate::objects::field(ctx, bindings, &location.name)? else {
        return Ok(None);
    };
    for key in &location.path.data {
        ctx.charge(1)?;
        let Kind::Hash(hash) = &value.0 else {
            return Ok(None);
        };
        let Some(index) = hash.find(ctx, key.require_bytes()?)? else {
            return Ok(None);
        };
        value = hash.buffer.data[index].1.clone();
    }
    Ok(matches!(value.0, Kind::Hash(_)).then_some(value))
}
