use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, value::Kind};

struct Hop {
    container: Value,
    key: Value,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Root {
    Local(usize),
    Field(usize, usize),
    Object(std::sync::Arc<crate::objects::Instance>, usize),
}

impl From<usize> for Root {
    fn from(slot: usize) -> Self {
        Self::Local(slot)
    }
}

pub(crate) struct Bindings<'a> {
    pub guard: Option<crate::types::Prepared<'a>>,
    pub recover: bool,
    pub locals: &'a mut [Option<Value>],
    pub namespaces: &'a mut [crate::namespace::State],
}

impl Bindings<'_> {
    fn set(&mut self, ctx: &mut CallContext, root: &Root, value: Option<Value>) -> Result<()> {
        match root {
            Root::Local(slot) => self.locals[*slot] = value,
            Root::Field(module, field) => {
                self.namespaces[*module].fields.buffer.data[*field].1 = value.unwrap_or_default()
            }
            Root::Object(instance, field) => {
                crate::objects::set_slot(ctx, instance, *field, value.unwrap_or_default())?
            }
        }
        Ok(())
    }
}

pub(crate) struct Address {
    protected: crate::hash::Tag,
    root: Option<Root>,
    path: Buffer<Hop>,
    pub value: Value,
    pub selectors: Buffer<Value>,
    pub member_target: bool,
}

impl Address {
    pub fn has_binding(&self) -> bool {
        self.root.is_some()
    }

    pub fn object_binding(&self) -> Option<(&std::sync::Arc<crate::objects::Instance>, usize)> {
        match self.root.as_ref() {
            Some(Root::Object(instance, field)) => Some((instance, *field)),
            _ => None,
        }
    }
    pub fn new(root: Option<usize>, value: Value) -> Self {
        Self {
            protected: crate::hash::Tag::None,
            root: root.map(Root::Local),
            path: Buffer::empty(),
            value,
            selectors: Buffer::empty(),
            member_target: false,
        }
    }

    pub fn field(module: usize, field: usize, value: Value) -> Self {
        let mut address = Self::new(None, value);
        address.root = Some(Root::Field(module, field));
        address
    }

    pub fn object(
        instance: std::sync::Arc<crate::objects::Instance>,
        field: usize,
        value: Value,
    ) -> Self {
        let mut address = Self::new(None, value);
        address.root = Some(Root::Object(instance, field));
        address
    }

    pub fn index(&mut self, ctx: &mut CallContext, args: &[Value]) -> Result<()> {
        let value = if args.len() == 1 {
            ops::index(ctx, &self.value, &args[0])?
        } else {
            crate::sequence::slice(ctx, &self.value, args, false)?
        };
        if let Kind::Hash(hash) = &self.value.0 {
            if hash.tag.protected() {
                self.protected = hash.tag;
            }
        }
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
        bindings: Bindings<'_>,
        pending: &mut [Self],
        value: Value,
    ) -> Result<Value> {
        let selectors = std::mem::replace(&mut self.selectors, Buffer::empty());
        self.apply(ctx, bindings, pending, |ctx, receiver| {
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
        mut bindings: Bindings<'_>,
        pending: &mut [Self],
        action: impl FnOnce(&mut CallContext, Value) -> Result<(Value, Value)>,
    ) -> Result<Value> {
        self.check_writable()?;
        let Self {
            protected: _,
            root,
            mut path,
            value,
            selectors: _,
            member_target: _,
        } = self;
        let Some(root) = root else {
            return action(ctx, value).map(|(_, result)| result);
        };
        // A handler or type guard can observe a rejected update. Keep the binding
        // intact until publication; otherwise transfer ownership for in-place writes.
        if bindings.guard.is_none() && !bindings.recover {
            bindings.set(ctx, &root, None)?;
        }
        let forward = pending.iter().any(|a| a.root.as_ref() == Some(&root));
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
        if let Some(guard) = &bindings.guard {
            let name = match &root {
                Root::Object(instance, field) => {
                    Some(crate::objects::field_name(instance, *field)?)
                }
                _ => None,
            };
            let context = name.as_ref().map_or(crate::types::Context::Value, |name| {
                crate::types::Context::Ivar(name.as_bytes().unwrap())
            });
            updated = guard.normalize_with(ctx, updated, context)?;
        }
        refresh(ctx, root.clone(), &updated, pending, &changes.data)?;
        bindings.set(ctx, &root, Some(updated))?;
        Ok(result)
    }

    pub fn check_writable(&self) -> Result<()> {
        if self.protected.protected() {
            Err(self.protected.mutation_error())
        } else {
            Ok(())
        }
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
        (Kind::Big(a), Kind::Big(b)) => Arc::ptr_eq(a, b),
        (Kind::Nil, Kind::Nil) => true,
        (Kind::Bool(a), Kind::Bool(b)) => a == b,
        (Kind::Int(a), Kind::Int(b)) => a == b,
        (Kind::Float(a), Kind::Float(b)) => a.to_bits() == b.to_bits(),
        _ => false,
    }
}

pub(crate) fn refresh(
    ctx: &mut CallContext,
    root: impl Into<Root>,
    updated: &Value,
    pending: &mut [Address],
    changes: &[(Value, Value)],
) -> Result<()> {
    let root = root.into();
    for address in pending {
        ctx.charge(1)?;
        if address.root.as_ref() != Some(&root) {
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
