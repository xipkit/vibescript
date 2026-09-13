use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, value::Kind};

struct Hop {
    container: Value,
    key: Value,
}

pub(crate) struct Address {
    root: Option<usize>,
    path: Buffer<Hop>,
    pub value: Value,
    pub selectors: Buffer<Value>,
}

impl Address {
    pub fn new(root: Option<usize>, value: Value) -> Self {
        Self {
            root,
            path: Buffer::empty(),
            value,
            selectors: Buffer::empty(),
        }
    }

    pub fn index(&mut self, ctx: &mut CallContext, args: &[Value]) -> Result<()> {
        let value = if args.len() == 1 {
            ops::index(ctx, &self.value, &args[0])?
        } else {
            crate::sequence::slice(ctx, &self.value, args, false)?
        };
        let addressed = self.root.is_some()
            && args.len() == 1
            && stored_child(ctx, &self.value, &args[0])?.is_some();
        if addressed {
            self.path.push(
                ctx,
                Hop {
                    container: self.value.clone(),
                    key: args[0].clone(),
                },
            )?;
        } else {
            self.root = None;
            self.path.data.clear();
        }
        self.value = value;
        Ok(())
    }

    pub fn read_target(&self, ctx: &mut CallContext) -> Result<Value> {
        if let [key] = self.selectors.data.as_slice() {
            ops::index(ctx, &self.value, key)
        } else {
            crate::sequence::slice(ctx, &self.value, &self.selectors.data, false)
        }
    }

    pub fn assign(
        mut self,
        ctx: &mut CallContext,
        locals: &mut [Option<Value>],
        pending: &mut [Self],
        value: Value,
    ) -> Result<Value> {
        let selectors = std::mem::replace(&mut self.selectors, Buffer::empty());
        self.apply(ctx, locals, pending, |ctx, receiver| {
            let [key] = selectors.data.as_slice() else {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "index assignment expects a single selector",
                ));
            };
            let receiver = ops::set_index(ctx, receiver, key.clone(), value.clone())?;
            Ok((receiver, value))
        })
    }

    pub fn apply(
        self,
        ctx: &mut CallContext,
        locals: &mut [Option<Value>],
        pending: &mut [Self],
        action: impl FnOnce(&mut CallContext, Value) -> Result<(Value, Value)>,
    ) -> Result<Value> {
        let Self {
            root,
            mut path,
            value,
            selectors: _,
        } = self;
        let Some(root) = root else {
            return action(ctx, value).map(|(_, result)| result);
        };
        // The captured path now owns the receiver. Other pending writes and script aliases
        // retain their own views until the mutation has been checked and published.
        locals[root] = None;
        let forward = pending.iter().any(|a| a.root == Some(root));
        let mut changes = Buffer::empty();
        if forward {
            changes.ensure(ctx, path.data.len() + 1)?;
        }
        let old = forward.then(|| value.clone());
        let (mut updated, result) = action(ctx, value)?;
        if let Some(old) = old {
            changes.data.push((old, updated.clone()));
        }
        while let Some(hop) = path.data.pop() {
            ctx.charge(1)?;
            let old = forward.then(|| hop.container.clone());
            updated = ops::set_index(ctx, hop.container, hop.key, updated)?;
            if let Some(old) = old {
                changes.data.push((old, updated.clone()));
            }
        }
        refresh(ctx, root, &updated, pending, &changes.data)?;
        locals[root] = Some(updated);
        Ok(result)
    }
}

fn stored_child(ctx: &mut CallContext, value: &Value, key: &Value) -> Result<Option<Value>> {
    match &value.0 {
        Kind::Array(h) if !matches!(key.0, Kind::Range(_)) => {
            let index = crate::sequence::integer(key)?;
            let index = if index < 0 {
                h.buffer.data.len() as i128 + i128::from(index)
            } else {
                i128::from(index)
            };
            Ok(usize::try_from(index)
                .ok()
                .and_then(|i| h.buffer.data.get(i))
                .cloned())
        }
        Kind::Hash(h) => Ok(h
            .find(ctx, key.require_bytes()?)?
            .map(|i| h.buffer.data[i].1.clone())),
        _ => Ok(None),
    }
}

fn same_storage(a: &Value, b: &Value) -> bool {
    use std::sync::Arc;
    match (&a.0, &b.0) {
        (Kind::Array(a), Kind::Array(b)) => Arc::ptr_eq(a, b),
        (Kind::Hash(a), Kind::Hash(b)) => Arc::ptr_eq(a, b),
        (Kind::Bytes(a), Kind::Bytes(b)) | (Kind::Symbol(a), Kind::Symbol(b)) => Arc::ptr_eq(a, b),
        (Kind::Range(a), Kind::Range(b)) => Arc::ptr_eq(a, b),
        (Kind::Nil, Kind::Nil) => true,
        (Kind::Bool(a), Kind::Bool(b)) => a == b,
        (Kind::Int(a), Kind::Int(b)) => a == b,
        (Kind::Float(a), Kind::Float(b)) => a.to_bits() == b.to_bits(),
        _ => false,
    }
}

pub(crate) fn refresh(
    ctx: &mut CallContext,
    root: usize,
    updated: &Value,
    pending: &mut [Address],
    changes: &[(Value, Value)],
) -> Result<()> {
    for address in pending {
        ctx.charge(1)?;
        if address.root != Some(root) {
            continue;
        }
        let unchanged = |ctx: &mut CallContext, old: &Value, new: &Value| -> Result<bool> {
            if same_storage(old, new) {
                return Ok(true);
            }
            for (from, to) in changes {
                ctx.charge(1)?;
                if same_storage(from, old) && same_storage(to, new) {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        let mut current = updated.clone();
        let mut valid = true;
        for hop in &mut address.path.data {
            ctx.charge(1)?;
            if !unchanged(ctx, &hop.container, &current)? {
                valid = false;
                break;
            }
            let Some(child) = stored_child(ctx, &current, &hop.key)? else {
                valid = false;
                break;
            };
            hop.container = current;
            current = child;
        }
        if valid && unchanged(ctx, &address.value, &current)? {
            address.value = current;
        } else {
            address.root = None;
            address.path.data.clear();
        }
    }
    Ok(())
}
