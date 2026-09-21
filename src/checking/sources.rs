use crate::{
    CallContext, Result,
    budget::{Buffer, Charge},
    code::Code,
    objects::Instance,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct SourceId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct CallableId {
    pub source: SourceId,
    pub index: usize,
}

impl SourceId {
    /// Identifies a borrowed program supplied directly to the internal analyzer.
    pub const ROOT: Self = Self(usize::MAX);

    /// Associates a function or host table index with its defining source.
    pub fn callable(self, index: usize) -> CallableId {
        CallableId {
            source: self,
            index,
        }
    }
}

#[derive(Debug)]
struct Source {
    code: Arc<Code>,
    scope: Scope,
    nominal_scope: Scope,
    _charge: Option<Charge>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    Runtime(Option<u64>),
    Import(SourceId, usize),
}

#[derive(Debug)]
pub(super) struct Sources {
    entries: Buffer<Box<Source>>,
}

impl Sources {
    pub fn new() -> Self {
        Self {
            entries: Buffer::empty(),
        }
    }

    /// Keeps identity stable for the fact arena's lifetime, including across source drops.
    pub fn owner(
        &mut self,
        ctx: &mut CallContext,
        code: &Arc<Code>,
        environment: Option<&Instance>,
    ) -> Result<usize> {
        self.scoped_owner(
            ctx,
            code,
            Scope::Runtime(environment.map(Instance::checking_id)),
            Scope::Runtime(
                environment
                    .map(Instance::nominal_scope)
                    .filter(|id| *id != 0),
            ),
        )
    }

    /// Gives each receiving invocation and retry its own private analysis environment.
    pub fn import_owner(
        &mut self,
        ctx: &mut CallContext,
        code: &Arc<Code>,
        receiving: SourceId,
        attempt: usize,
    ) -> Result<usize> {
        let scope = Scope::Import(receiving, attempt);
        self.scoped_owner(ctx, code, scope, scope)
    }

    fn scoped_owner(
        &mut self,
        ctx: &mut CallContext,
        code: &Arc<Code>,
        scope: Scope,
        nominal_scope: Scope,
    ) -> Result<usize> {
        ctx.checkpoint()?;
        for entry in &self.entries.data {
            ctx.charge(1)?;
            if Arc::ptr_eq(&entry.code, code) && entry.scope == scope {
                return Ok(&**entry as *const Source as usize);
            }
        }
        // Boxing keeps the identity address stable when the metered table grows.
        let charge = ctx.reserve(size_of::<Source>())?;
        let entry = Box::new(Source {
            code: code.clone(),
            scope,
            nominal_scope,
            _charge: charge,
        });
        let owner = &*entry as *const Source as usize;
        self.entries.push(ctx, entry)?;
        Ok(owner)
    }

    /// Keeps type compatibility across snapshots without combining their state.
    pub fn same_type(&self, ctx: &mut CallContext, left: usize, right: usize) -> Result<bool> {
        if left == right {
            return Ok(true);
        }
        let mut a = None;
        let mut b = None;
        for entry in &self.entries.data {
            ctx.charge(1)?;
            let owner = &**entry as *const Source as usize;
            if owner == left {
                a = Some(entry);
            }
            if owner == right {
                b = Some(entry);
            }
        }
        Ok(matches!((a, b), (Some(a), Some(b)) if
            Arc::ptr_eq(&a.code, &b.code) && a.nominal_scope == b.nominal_scope))
    }

    /// Uses registration order for hashing while preserving the stable identity.
    /// Heap addresses must not change collision work between equivalent checks.
    pub fn key(&self, ctx: &mut CallContext, owner: usize) -> Result<(bool, usize)> {
        for (index, entry) in self.entries.data.iter().enumerate() {
            ctx.charge(1)?;
            if &**entry as *const Source as usize == owner {
                return Ok((true, index));
            }
        }
        Ok((false, owner))
    }

    /// Resolves a source owner to its stable registration ordinal.
    pub fn id(&self, ctx: &mut CallContext, owner: usize) -> Result<SourceId> {
        ctx.checkpoint()?;
        let (registered, index) = self.key(ctx, owner)?;
        if registered {
            Ok(SourceId(index))
        } else if owner == 0 {
            Ok(SourceId::ROOT)
        } else {
            Err(crate::Error::new(
                crate::ErrorKind::Runtime,
                "unknown checker source identity",
            ))
        }
    }

    /// Keeps a selected source alive while its metadata or diagnostics are read.
    pub fn code(&self, ctx: &mut CallContext, source: SourceId) -> Result<Option<Arc<Code>>> {
        ctx.charge(1)?;
        if source == SourceId::ROOT {
            return Ok(None);
        }
        self.entries
            .data
            .get(source.0)
            .map(|entry| Some(entry.code.clone()))
            .ok_or_else(|| {
                crate::Error::new(crate::ErrorKind::Runtime, "unknown checker source identity")
            })
    }
}
