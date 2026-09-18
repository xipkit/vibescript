use super::{
    builtins,
    facts::{Fact, Facts},
    pending::Pending,
};
use crate::{CallContext, Result, budget::Buffer, bytecode::Program};
use std::hash::{Hash, Hasher};

// Call contexts use program-global indices, independent of the callee's local layout.
#[derive(Debug)]
pub(super) struct Globals {
    pub values: Buffer<Fact>,
    pub written: Buffer<bool>,
    pub pending: Pending,
}

impl Globals {
    pub fn empty() -> Self {
        Self {
            values: Buffer::empty(),
            written: Buffer::empty(),
            pending: Pending::new(),
        }
    }

    pub fn initial(ctx: &mut CallContext, facts: &mut Facts, program: &Program) -> Result<Self> {
        let mut globals = Self::empty();
        for (_, value) in &program.globals {
            ctx.charge(1)?;
            let value = builtins::global(ctx, facts, value)?;
            globals.values.push(ctx, value)?;
            globals.written.push(ctx, false)?;
        }
        Ok(globals)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        let mut globals = Self::empty();
        globals.values.extend(ctx, &self.values.data)?;
        globals.written.extend(ctx, &self.written.data)?;
        globals.pending = self.pending.snapshot(ctx)?;
        Ok(globals)
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge(self.values.data.len() as u64 + self.written.data.len() as u64 + 1)?;
        self.values.data.hash(hash);
        self.written.data.hash(hash);
        self.pending.hash(ctx, hash)
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(self.values.data.len() as u64 + self.written.data.len() as u64 + 1)?;
        Ok(self.values.data == other.values.data
            && self.written.data == other.written.data
            && self.pending.equal(ctx, &other.pending)?)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.values.data.len() == other.values.data.len()
            && self.pending.compatible(ctx, &other.pending)?)
    }

    pub fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        assert_eq!(self.values.data.len(), other.values.data.len());
        let mut changed = self.pending.join(ctx, facts, &other.pending, depth)?;
        for (a, b) in self.values.data.iter_mut().zip(&other.values.data) {
            ctx.charge(1)?;
            let value = facts.joined(ctx, *a, *b, depth)?;
            changed |= *a != value;
            *a = value;
        }
        for (a, b) in self.written.data.iter_mut().zip(&other.written.data) {
            ctx.charge(1)?;
            changed |= !*a && *b;
            *a |= *b;
        }
        Ok(changed)
    }
}
