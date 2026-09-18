use crate::{
    CallContext, Result,
    budget::{Buffer, Charge},
    code::Code,
    objects::Instance,
};
use std::sync::Arc;

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
}
