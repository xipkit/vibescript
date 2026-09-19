use super::{
    builtins,
    facts::{Atom, Fact, Facts},
    pending::Pending,
};
use crate::{CallContext, Result, budget::Buffer, bytecode::Program};
use std::hash::{Hash, Hasher};

pub(super) mod layout;

#[cfg(test)]
pub(super) mod tests;

#[derive(Debug)]
pub(super) struct Globals {
    pub layout: layout::Layout,
    pub values: Buffer<Fact>,
    pub missing: Buffer<bool>,
    pub written: Buffer<bool>,
    pub pending: Pending,
}

impl Globals {
    pub fn empty() -> Self {
        Self {
            layout: layout::Layout::default(),
            values: Buffer::empty(),
            missing: Buffer::empty(),
            written: Buffer::empty(),
            pending: Pending::new(),
        }
    }

    pub fn initial(ctx: &mut CallContext, layout: &layout::Layout) -> Result<Self> {
        let mut globals = Self::empty();
        globals.expand(ctx, layout)?;
        Ok(globals)
    }

    pub fn expand(&mut self, ctx: &mut CallContext, layout: &layout::Layout) -> Result<bool> {
        let next = self.layout.latest(ctx, layout)?;
        if self.layout.same(&next) {
            return Ok(false);
        }
        for index in self.values.data.len()..next.len() {
            ctx.charge(1)?;
            let initial = next.initial(index);
            self.values.push(ctx, initial.value)?;
            self.missing.push(ctx, initial.missing)?;
            self.written.push(ctx, false)?;
        }
        self.layout = next;
        Ok(true)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        let mut globals = Self::empty();
        globals.layout = self.layout.clone();
        globals.values.extend(ctx, &self.values.data)?;
        globals.missing.extend(ctx, &self.missing.data)?;
        globals.written.extend(ctx, &self.written.data)?;
        globals.pending = self.pending.snapshot(ctx)?;
        Ok(globals)
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge(
            self.values.data.len() as u64
                + self.written.data.len() as u64
                + self.missing.data.len() as u64
                + 1,
        )?;
        self.layout.version().hash(hash);
        self.values.data.hash(hash);
        self.missing.data.hash(hash);
        self.written.data.hash(hash);
        self.pending.hash(ctx, hash)
    }

    /// Keeps recursive calls on either side of initialization in separate contexts.
    pub fn same_initialization(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        let layout = self.layout.latest(ctx, &other.layout)?;
        for source in layout.sources() {
            ctx.charge(1)?;
            for flag in source
                .namespaces
                .clone()
                .step_by(super::namespaces::WIDTH)
                .map(|root| root + 1)
                .chain(source.import)
            {
                ctx.charge(1)?;
                let initial = layout.initial(flag).value;
                if self.values.data.get(flag).copied().unwrap_or(initial)
                    != other.values.data.get(flag).copied().unwrap_or(initial)
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Replaces a shared binding and refreshes suspended indexed writes.
    pub fn store(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
    ) -> Result<()> {
        ctx.charge(1)?;
        let same = self.values.data[slot] == value;
        self.values.data[slot] = value;
        self.missing.data[slot] = false;
        self.written.data[slot] = true;
        for address in &mut self.pending.addresses.data {
            ctx.charge(1)?;
            if address.root == Some(slot) {
                address.refresh(
                    ctx,
                    facts,
                    value,
                    &super::addresses::Change::Store { same, fresh: false },
                )?;
            }
        }
        Ok(())
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(
            self.values.data.len() as u64
                + self.written.data.len() as u64
                + self.missing.data.len() as u64
                + 1,
        )?;
        Ok(self.layout.same(&other.layout)
            && self.values.data == other.values.data
            && self.missing.data == other.missing.data
            && self.written.data == other.written.data
            && self.pending.equal(ctx, &other.pending)?)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.layout.compatible(&other.layout)
            && self.pending.compatible(ctx, &other.pending)?
            && self.layout.same_imports(
                ctx,
                &other.layout,
                |_, slot| Ok(self.values.data.get(slot).copied()),
                |_, slot| Ok(other.values.data.get(slot).copied()),
            )?)
    }

    /// Combines compatible states without mixing distinct import lifetimes.
    pub fn join_into(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        alternatives: &mut Buffer<Self>,
    ) -> Result<()> {
        for current in &mut alternatives.data {
            ctx.charge(1)?;
            if current.compatible(ctx, &self)? {
                current.join(ctx, facts, &self, None)?;
                return Ok(());
            }
        }
        alternatives.push(ctx, self)
    }

    /// Preserves writes from earlier stages of a composed call.
    pub fn inherit_writes(&mut self, ctx: &mut CallContext, earlier: &Self) -> Result<()> {
        self.expand(ctx, &earlier.layout)?;
        for (current, earlier) in self.written.data.iter_mut().zip(&earlier.written.data) {
            ctx.charge(1)?;
            *current |= earlier;
        }
        Ok(())
    }

    pub fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        let mut changed = self.expand(ctx, &other.layout)?;
        changed |= self.pending.join(ctx, facts, &other.pending, depth)?;
        for (index, a) in self.values.data.iter_mut().enumerate() {
            ctx.charge(1)?;
            let b = other
                .values
                .data
                .get(index)
                .copied()
                .unwrap_or_else(|| self.layout.initial(index).value);
            let value = facts.joined(ctx, *a, b, depth)?;
            changed |= *a != value;
            *a = value;
        }
        for (a, b) in self.written.data.iter_mut().zip(&other.written.data) {
            ctx.charge(1)?;
            changed |= !*a && *b;
            *a |= *b;
        }
        for (index, a) in self.missing.data.iter_mut().enumerate() {
            ctx.charge(1)?;
            let b = other
                .missing
                .data
                .get(index)
                .copied()
                .unwrap_or_else(|| self.layout.initial(index).missing);
            changed |= !*a && b;
            *a |= b;
        }
        Ok(changed)
    }
}
