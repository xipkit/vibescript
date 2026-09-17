use super::{addresses::Address, facts::Facts};
use crate::{CallContext, Result, budget::Buffer};
use std::hash::{Hash, Hasher};

// These addresses belong to suspended callers. Their roots use the same flattened
// lexical slots as captures, and each write refreshes them before flow can join.
#[derive(Debug)]
pub(super) struct Pending {
    pub addresses: Buffer<Address>,
}

impl Pending {
    pub fn new() -> Self {
        Self {
            addresses: Buffer::empty(),
        }
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut result = Self::new();
        for address in &self.addresses.data {
            let address = address.snapshot(ctx)?;
            result.addresses.push(ctx, address)?;
        }
        Ok(result)
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge(1)?;
        self.addresses.data.len().hash(hash);
        for address in &self.addresses.data {
            address.hash(ctx, hash)?;
        }
        Ok(())
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        if self.addresses.data.len() != other.addresses.data.len() {
            return Ok(false);
        }
        for (a, b) in self.addresses.data.iter().zip(&other.addresses.data) {
            if !a.equal(ctx, b)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        if self.addresses.data.len() != other.addresses.data.len() {
            return Ok(false);
        }
        for (a, b) in self.addresses.data.iter().zip(&other.addresses.data) {
            ctx.charge(1)?;
            if !a.compatible(b) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        assert_eq!(self.addresses.data.len(), other.addresses.data.len());
        let mut changed = false;
        for (a, b) in self.addresses.data.iter_mut().zip(&other.addresses.data) {
            ctx.charge(1)?;
            changed |= a.join(ctx, facts, b, depth)?;
        }
        Ok(changed)
    }
}
