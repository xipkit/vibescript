use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, ops, value::Kind};

struct Hop {
    container: Value,
    key: Value,
}

impl Hop {
    /// The error for a write through the element this hop found missing.
    fn missing(&self, ctx: &mut CallContext) -> Result<Error> {
        const WHAT: &str = "cannot write through a missing element";
        let Kind::Array(array) = &self.container.0 else {
            return crate::collections::missing_key(ctx, &format!("{WHAT}: hash"), &self.key);
        };
        let index = crate::sequence::integer(&self.key)?;
        let length = array.buffer.data.len();
        Ok(Error::new(
            ErrorKind::Argument,
            format!(
                "{WHAT}: array index {index} outside of array bounds: {}...{length}",
                -(length as i128)
            ),
        ))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Root {
    Local(usize),
    Global(usize),
    Field(usize, usize),
    Object(std::sync::Arc<crate::objects::Instance>, usize),
    Environment(std::sync::Arc<crate::objects::Instance>, usize),
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
    pub globals: &'a mut [Option<Value>],
    pub namespaces: &'a mut [crate::namespace::State],
}

impl Bindings<'_> {
    fn set(&mut self, ctx: &mut CallContext, root: &Root, value: Option<Value>) -> Result<()> {
        match root {
            Root::Local(slot) => self.locals[*slot] = value,
            Root::Global(slot) => self.globals[*slot] = value,
            Root::Field(module, field) => {
                self.namespaces[*module].fields.buffer.data[*field].1 = value.unwrap_or_default()
            }
            Root::Object(instance, field) | Root::Environment(instance, field) => {
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
    pub exported: Option<std::sync::Arc<crate::exports::Function>>,
    pub capability: Option<crate::capability::SelectedMethod>,
    /// Whether an index on the way here found its element missing, so that
    /// the value is nil and a write cannot reach its place. The address is
    /// then unrooted, and its path holds only that index.
    missing: bool,
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
            exported: None,
            capability: None,
            missing: false,
        }
    }

    pub fn global(slot: usize, value: Value) -> Self {
        let mut address = Self::new(None, value);
        address.root = Some(Root::Global(slot));
        address
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

    pub fn in_environment(mut self) -> Self {
        let Some(Root::Object(instance, field)) = self.root.take() else {
            unreachable!()
        };
        self.root = Some(Root::Environment(instance, field));
        self
    }

    pub fn index(&mut self, ctx: &mut CallContext, args: &[Value]) -> Result<()> {
        self.check_present(ctx)?;
        let value = if args.len() == 1 {
            ops::index(ctx, &self.value, &args[0])?
        } else {
            ops::index_many(ctx, &self.value, args)?
        };
        if let Kind::Hash(hash) = &self.value.0 {
            if hash.tag.protected() {
                self.protected = hash.tag;
            }
        }
        let single = args.len() == 1 && !matches!(args[0].0, Kind::Range(_));
        let nil = matches!(value.0, Kind::Nil);
        let child = if args.len() == 1 && (self.root.is_some() || (nil && single)) {
            stored_child(ctx, &self.value, &args[0])?
        } else {
            None
        };
        let addressed = self.root.is_some() && args.len() == 1 && child.is_some();
        if addressed {
            let key = captured_key(&self.value, &args[0])?;
            self.path.push(
                ctx,
                Hop {
                    container: self.value.clone(),
                    key,
                },
            )?;
        } else {
            self.root = None;
            self.path.data.clear();
        }
        let missing = nil
            && single
            && child.is_none()
            && matches!(self.value.0, Kind::Array(_) | Kind::Hash(_));
        if missing {
            let container = std::mem::take(&mut self.value);
            self.path.push(
                ctx,
                Hop {
                    container,
                    key: args[0].clone(),
                },
            )?;
            self.missing = true;
        }
        self.value = value;
        Ok(())
    }

    /// Fails when an index on the way to this address found its element
    /// missing, which a write through the address cannot create.
    /// Whether [`Self::check_present`] would fail.
    pub fn is_missing(&self) -> bool {
        self.missing && !self.path.data.is_empty()
    }

    pub fn check_present(&self, ctx: &mut CallContext) -> Result<()> {
        match self.path.data.last() {
            Some(hop) if self.missing => Err(hop.missing(ctx)?),
            _ => Ok(()),
        }
    }

    pub fn read_target(&mut self, ctx: &mut CallContext) -> Result<Value> {
        if let [key] = self.selectors.data.as_slice() {
            let value = ops::index(ctx, &self.value, key)?;
            self.selectors.data[0] = captured_key(&self.value, key)?;
            Ok(value)
        } else {
            ops::index_many(ctx, &self.value, &self.selectors.data)
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
                return Err(match &receiver.0 {
                    Kind::Array(_) => Error::new(
                        ErrorKind::Argument,
                        "array index assignment expects a single index",
                    ),
                    Kind::Hash(_) => Error::new(
                        ErrorKind::Argument,
                        format!(
                            "{} index assignment expects a single key",
                            receiver.type_name()
                        ),
                    ),
                    _ => ops::cannot_index(&receiver),
                });
            };
            if let Kind::Hash(hash) = &receiver.0 {
                if hash.tag.protected() {
                    return Err(hash.tag.mutation_error("index assignment"));
                }
            }
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
            exported: _,
            capability: _,
            missing: _,
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

    /// Rejects a write whose path passes through a protected record. The
    /// reference reports such a write-back as an assignment.
    pub fn check_writable(&self) -> Result<()> {
        if self.protected.protected() {
            Err(self.protected.mutation_error("assignment"))
        } else {
            Ok(())
        }
    }
}

// A selected array element keeps its position while arguments or blocks grow its parent.
fn captured_key(value: &Value, key: &Value) -> Result<Value> {
    if let Kind::Array(array) = &value.0 {
        if !matches!(key.0, Kind::Range(_)) {
            let length = array.buffer.data.len();
            if let Some(index) = ops::normalized(crate::sequence::integer(key)?, length)
                .filter(|&index| index < length)
            {
                return Ok(Value::int(index as i64));
            }
        }
    }
    Ok(key.clone())
}

fn stored_child(ctx: &mut CallContext, value: &Value, key: &Value) -> Result<Option<Value>> {
    match &value.0 {
        Kind::Array(h) if !matches!(key.0, Kind::Range(_)) => Ok(ops::normalized(
            crate::sequence::integer(key)?,
            h.buffer.data.len(),
        )
        .and_then(|i| h.buffer.data.get(i))
        .cloned()),
        Kind::Hash(h) => Ok(h
            .find(ctx, key.hash_key()?)?
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
