use crate::{
    CallContext, Result,
    budget::{Buffer, Charge},
    code::Code,
    objects::Instance,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct SourceId(usize);

impl SourceId {
    /// Identifies a borrowed program supplied directly to the internal analyzer.
    pub const ROOT: Self = Self(usize::MAX);
}

#[derive(Debug)]
struct Source {
    code: Arc<Code>,
    scope: Option<u64>,
    _charge: Option<Charge>,
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
        ctx.checkpoint()?;
        let scope = environment.map(Instance::checking_id);
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
            _charge: charge,
        });
        let owner = &*entry as *const Source as usize;
        self.entries.push(ctx, entry)?;
        Ok(owner)
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
