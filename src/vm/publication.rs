//! Host writes into a capability object during the call that granted it.
//!
//! A capability binding, or a host global holding host methods, is the host's
//! live state for one invocation. A host method publishes by storing a field in
//! the object it was called on. The first publication finds the binding path
//! that holds that object; the write and every later publication in the same
//! host call land at that path, so later script reads, blocks and host calls
//! observe them. Copies the script made earlier stay independent values.

use super::*;

/// The root binding and nested hash keys that lead to a receiver.
pub(super) struct Location {
    name: Value,
    path: Buffer<Value>,
}

/// Stores `key: value` in `receiver`, publishing the write to the binding path
/// that holds it. Returns whether a binding received the write.
///
/// `location` caches where the receiver was found, so writes the script makes
/// at that path between two publications are kept.
pub(super) fn set_field(
    ctx: &mut CallContext,
    storage: &mut Storage,
    receiver: &mut Value,
    location: &mut Option<Location>,
    key: &[u8],
    value: &Value,
) -> Result<bool> {
    ctx.checkpoint()?;
    if !matches!(receiver.0, Kind::Hash(_)) {
        return Err(Error::new(
            ErrorKind::Type,
            "host receiver does not accept fields",
        ));
    }
    if location.is_none() {
        *location = locate(ctx, storage, receiver)?;
    }
    let target = match location {
        Some(found) => read(ctx, storage, found)?,
        None => None,
    };
    if target.is_none() {
        *location = None;
    }
    reject_method_field(ctx, target.as_ref().unwrap_or(receiver), key)?;
    crate::capability::member_name(key, value)?;
    let key = ctx.bytes(key)?;
    let value = ctx.snapshot(value)?;
    if !matches!(value.0, Kind::Host(_)) {
        crate::exports::check(ctx, &value)?;
    }
    programs::imported(ctx, storage, &value)?;
    if let Some(found) = location {
        let mut address = open(ctx, storage, found)?;
        address.selectors.push(ctx, key)?;
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
            value,
        )?;
        if let Some(current) = read(ctx, storage, found)? {
            *receiver = current;
        }
        return Ok(true);
    }
    *receiver = ops::set_index(ctx, receiver.clone(), key, value)?;
    Ok(false)
}

/// Rejects replacing a field that holds a host method; methods are the
/// capability's interface, not published state.
fn reject_method_field(ctx: &mut CallContext, node: &Value, key: &[u8]) -> Result<()> {
    let Kind::Hash(hash) = &node.0 else {
        return Ok(());
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
    Ok(())
}

/// One hash visited while searching root bindings breadth first.
struct Node {
    root: usize,
    parent: usize,
    key: Value,
    hash: Arc<Hash>,
    depth: usize,
}

/// Finds the root binding that holds `receiver` by identity, preferring the
/// shallowest path, then capabilities in grant order, then host globals.
fn locate(ctx: &mut CallContext, storage: &Storage, receiver: &Value) -> Result<Option<Location>> {
    let (Kind::Hash(target), Some(bindings)) = (&receiver.0, &storage.bindings) else {
        return Ok(None);
    };
    let mut names = Buffer::empty();
    for index in 0..ctx.capability_names.data.len() {
        let name = ctx.capability_names.data[index].clone();
        names.push(ctx, name)?;
    }
    let globals = std::mem::take(&mut ctx.options.globals);
    let listed = (|| {
        for name in globals.keys() {
            let name = ctx.bytes(name.as_bytes())?;
            names.push(ctx, name)?;
        }
        Ok(())
    })();
    ctx.options.globals = globals;
    listed?;
    let mut nodes: Buffer<Node> = Buffer::empty();
    for (root, name) in names.data.iter().enumerate() {
        ctx.charge(1)?;
        let Some(Value(Kind::Hash(hash))) = crate::objects::field(ctx, bindings, text(name))?
        else {
            continue;
        };
        if Arc::ptr_eq(&hash, target) {
            return Ok(Some(Location {
                name: name.clone(),
                path: Buffer::empty(),
            }));
        }
        nodes.push(
            ctx,
            Node {
                root,
                parent: usize::MAX,
                key: Value::nil(),
                hash,
                depth: 0,
            },
        )?;
    }
    let mut next = 0;
    while next < nodes.data.len() {
        let (hash, depth) = (nodes.data[next].hash.clone(), nodes.data[next].depth);
        if depth + 1 < crate::budget::MAX_VALUE_DEPTH {
            for (key, child) in &hash.buffer.data {
                ctx.charge(1)?;
                let Kind::Hash(child) = &child.0 else {
                    continue;
                };
                if Arc::ptr_eq(child, target) {
                    let mut path = Buffer::with_capacity(ctx, depth + 1)?;
                    path.data.push(key.clone());
                    let mut at = next;
                    while nodes.data[at].parent != usize::MAX {
                        path.data.push(nodes.data[at].key.clone());
                        at = nodes.data[at].parent;
                    }
                    path.data.reverse();
                    return Ok(Some(Location {
                        name: names.data[nodes.data[at].root].clone(),
                        path,
                    }));
                }
                let root = nodes.data[next].root;
                nodes.push(
                    ctx,
                    Node {
                        root,
                        parent: next,
                        key: key.clone(),
                        hash: child.clone(),
                        depth: depth + 1,
                    },
                )?;
            }
        }
        next += 1;
    }
    Ok(None)
}

/// Root binding names are UTF-8: they come from grant names and global keys.
fn text(name: &Value) -> &str {
    std::str::from_utf8(name.as_bytes().unwrap_or_default()).unwrap_or_default()
}

/// Opens a write address at a location `read` has just confirmed.
fn open(ctx: &mut CallContext, storage: &Storage, location: &Location) -> Result<Address> {
    let bindings = storage.bindings.as_ref().unwrap();
    let mut address =
        crate::objects::address(ctx, bindings, text(&location.name))?.in_environment();
    for key in &location.path.data {
        address.index(ctx, std::slice::from_ref(key))?;
    }
    Ok(address)
}

/// Reads the hash currently stored at `location`.
fn read(ctx: &mut CallContext, storage: &Storage, location: &Location) -> Result<Option<Value>> {
    let Some(bindings) = &storage.bindings else {
        return Ok(None);
    };
    let Some(mut value) = crate::objects::field(ctx, bindings, text(&location.name))? else {
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
