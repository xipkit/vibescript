use super::{
    builtins,
    facts::{Atom, Fact, Facts},
    pending::Pending,
    slots::Slots,
};
use crate::{CallContext, Result, budget::Buffer, bytecode::Program};
use std::hash::{DefaultHasher, Hash, Hasher};

pub(super) mod layout;

#[cfg(test)]
pub(super) mod tests;

/// A shared binding's value and whether it may be unbound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Global {
    pub value: Fact,
    pub missing: bool,
}

/// The binding of a shared slot before anything describes it.
pub(super) const ABSENT: Global = Global {
    value: Atom::Never.fact(),
    missing: true,
};

/// Persistent entries with a digest of the entries that differ from the empty value.
///
/// The digest sums a hash of each such entry and its index, so equal tables digest alike
/// whatever order wrote them, and a write or merge updates it for the entries it changes.
/// Copies share storage, so calls and exits that carry every shared binding cost only the
/// bindings they change.
#[derive(Debug)]
pub(super) struct Table<T> {
    slots: Slots<T>,
    digest: u64,
}

impl<T: Copy + Eq + Hash> Table<T> {
    pub fn new(len: usize, empty: T) -> Self {
        Self {
            slots: Slots::new(len, empty),
            digest: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn digest(&self) -> u64 {
        self.digest
    }

    fn entry(&self, index: usize, value: T) -> u64 {
        if value == self.slots.empty() {
            return 0;
        }
        let mut hash = DefaultHasher::new();
        (index, value).hash(&mut hash);
        hash.finish()
    }

    pub fn get(&self, ctx: &mut CallContext, index: usize) -> Result<T> {
        self.slots.get(ctx, index)
    }

    pub fn set(&mut self, ctx: &mut CallContext, index: usize, value: T) -> Result<()> {
        let previous = self.slots.replace(ctx, index, value)?;
        if previous != value {
            self.digest = self
                .digest
                .wrapping_sub(self.entry(index, previous))
                .wrapping_add(self.entry(index, value));
        }
        Ok(())
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            slots: self.slots.snapshot(ctx)?,
            digest: self.digest,
        })
    }

    /// Appends empty entries.
    pub fn grow(&mut self, ctx: &mut CallContext, len: usize) -> Result<()> {
        self.slots.grow(ctx, len)
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.digest == other.digest && self.slots.equal(ctx, &other.slots)?)
    }

    /// Merges the entries that differ from `other` with `join`, skipping shared storage.
    pub fn merge(
        &mut self,
        ctx: &mut CallContext,
        other: &Self,
        mut join: impl FnMut(&mut CallContext, usize, T, T) -> Result<T>,
    ) -> Result<bool> {
        let mut digest = self.digest;
        let empty = self.slots.empty();
        let entry = |index: usize, value: T| {
            if value == empty {
                return 0;
            }
            let mut hash = DefaultHasher::new();
            (index, value).hash(&mut hash);
            hash.finish()
        };
        let changed = self
            .slots
            .merge_indexed(ctx, &other.slots, |ctx, index, a, b| {
                let value = join(ctx, index, a, b)?;
                if value != a {
                    digest = digest
                        .wrapping_sub(entry(index, a))
                        .wrapping_add(entry(index, value));
                }
                Ok(value)
            })?;
        self.digest = digest;
        Ok(changed)
    }

    /// Visits the entries that differ from `other`, skipping shared storage.
    pub fn changed(
        &self,
        ctx: &mut CallContext,
        other: &Self,
        visit: &mut impl FnMut(&mut CallContext, usize, T, T) -> Result<()>,
    ) -> Result<()> {
        self.slots.changed(ctx, &other.slots, visit)
    }

    /// Visits the entries that differ from the empty value, in index order.
    pub fn entries(
        &self,
        ctx: &mut CallContext,
        visit: &mut impl FnMut(&mut CallContext, usize, T) -> Result<()>,
    ) -> Result<()> {
        let empty = Slots::new(self.slots.len(), self.slots.empty());
        self.slots
            .changed(ctx, &empty, &mut |ctx, index, value, _| {
                visit(ctx, index, value)
            })
    }
}

#[derive(Debug)]
pub(super) struct Globals {
    pub layout: layout::Layout,
    pub bindings: Table<Global>,
    pub written: Table<bool>,
    pub pending: Pending,
}

impl Globals {
    pub fn empty() -> Self {
        Self {
            layout: layout::Layout::default(),
            bindings: Table::new(0, ABSENT),
            written: Table::new(0, false),
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
        let start = self.len();
        self.bindings.grow(ctx, next.len())?;
        self.written.grow(ctx, next.len())?;
        for index in start..next.len() {
            ctx.charge(1)?;
            let initial = next.initial(index);
            self.bindings.set(
                ctx,
                index,
                Global {
                    value: initial.value,
                    missing: initial.missing,
                },
            )?;
        }
        self.layout = next;
        Ok(true)
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn get(&self, ctx: &mut CallContext, slot: usize) -> Result<Global> {
        self.bindings.get(ctx, slot)
    }

    pub fn value(&self, ctx: &mut CallContext, slot: usize) -> Result<Fact> {
        Ok(self.bindings.get(ctx, slot)?.value)
    }

    pub fn missing(&self, ctx: &mut CallContext, slot: usize) -> Result<bool> {
        Ok(self.bindings.get(ctx, slot)?.missing)
    }

    /// Returns a slot's value, or none beyond this state's slots.
    pub fn value_at(&self, ctx: &mut CallContext, slot: usize) -> Result<Option<Fact>> {
        if slot < self.len() {
            self.value(ctx, slot).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Replaces a slot's value without changing whether it may be unbound.
    pub fn set_value(&mut self, ctx: &mut CallContext, slot: usize, value: Fact) -> Result<()> {
        let binding = self.bindings.get(ctx, slot)?;
        self.bindings.set(ctx, slot, Global { value, ..binding })
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        Ok(Self {
            layout: self.layout.clone(),
            bindings: self.bindings.snapshot(ctx)?,
            written: self.written.snapshot(ctx)?,
            pending: self.pending.snapshot(ctx)?,
        })
    }

    /// Maps every shared value and suspended address.
    pub fn rename(
        &mut self,
        ctx: &mut CallContext,
        rename: &mut super::heaps::Rename<'_>,
    ) -> Result<()> {
        for index in 0..self.len() {
            let value = self.value(ctx, index)?;
            let renamed = rename(ctx, value)?;
            if renamed != value {
                self.set_value(ctx, index, renamed)?;
            }
        }
        self.pending.rename(ctx, rename)
    }

    /// Renames folded objects and merges their heap entries into the summaries.
    pub fn fold(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        renamer: &mut super::heaps::Renamer<'_>,
    ) -> Result<()> {
        if !renamer.active() {
            return Ok(());
        }
        self.rename(ctx, &mut |ctx, fact| renamer.fact(ctx, facts, fact))?;
        for index in 0..self.len() {
            ctx.charge(1)?;
            let value = self.value(ctx, index)?;
            let merged = renamer.merge(ctx, facts, index, value)?;
            if merged != value {
                self.set_value(ctx, index, merged)?;
            }
        }
        Ok(())
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge(1)?;
        self.layout.version().hash(hash);
        self.len().hash(hash);
        self.bindings.digest().hash(hash);
        self.written.digest().hash(hash);
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
                .chain(source.activation)
            {
                ctx.charge(1)?;
                let initial = layout.initial(flag).value;
                if self.value_at(ctx, flag)?.unwrap_or(initial)
                    != other.value_at(ctx, flag)?.unwrap_or(initial)
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
        let same = self.value(ctx, slot)? == value;
        self.bindings.set(
            ctx,
            slot,
            Global {
                value,
                missing: false,
            },
        )?;
        self.written.set(ctx, slot, true)?;
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
        ctx.charge(1)?;
        Ok(self.layout.same(&other.layout)
            && self.bindings.equal(ctx, &other.bindings)?
            && self.written.equal(ctx, &other.written)?
            && self.pending.equal(ctx, &other.pending)?)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.layout.compatible(&other.layout)
            && self.pending.compatible(ctx, &other.pending)?
            && self.layout.same_imports(
                ctx,
                &other.layout,
                |ctx, slot| self.value_at(ctx, slot),
                |ctx, slot| other.value_at(ctx, slot),
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

    /// Joins like [`Self::join_into`], but keeps states whose instance heaps hold different
    /// allocation counts apart, so a later declaration check can still select each instance.
    pub fn join_allocations_into(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        alternatives: &mut Buffer<Self>,
    ) -> Result<()> {
        for current in &mut alternatives.data {
            ctx.charge(1)?;
            if current.compatible(ctx, &self)? && current.same_allocations(ctx, facts, &self)? {
                current.join(ctx, facts, &self, None)?;
                return Ok(());
            }
        }
        alternatives.push(ctx, self)
    }

    fn same_allocations(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
    ) -> Result<bool> {
        let layout = self.layout.latest(ctx, &other.layout)?;
        for source in layout.sources() {
            for root in source.namespaces.clone().step_by(super::namespaces::WIDTH) {
                ctx.charge(1)?;
                let heap = root + 2;
                let (Some(a), Some(b)) = (self.value_at(ctx, heap)?, other.value_at(ctx, heap)?)
                else {
                    continue;
                };
                let a = super::heaps::entries(ctx, facts, a)?.map(|entries| entries.data.len());
                let b = super::heaps::entries(ctx, facts, b)?.map(|entries| entries.data.len());
                if let (Some(a), Some(b)) = (a, b) {
                    if a != b {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Preserves writes from earlier stages of a composed call.
    pub fn inherit_writes(&mut self, ctx: &mut CallContext, earlier: &Self) -> Result<()> {
        self.expand(ctx, &earlier.layout)?;
        let mut written = earlier.written.snapshot(ctx)?;
        written.grow(ctx, self.len())?;
        self.written.merge(ctx, &written, |_, _, a, b| Ok(a || b))?;
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
        // Slots that `other` predates hold their initial bindings there.
        let mut expanded;
        let other = if other.layout.same(&self.layout) {
            other
        } else {
            expanded = other.snapshot(ctx)?;
            expanded.expand(ctx, &self.layout)?;
            &expanded
        };
        let layout = &self.layout;
        changed |= self
            .bindings
            .merge(ctx, &other.bindings, |ctx, index, a, b| {
                ctx.charge(1)?;
                // Heaps of different lengths or with summaries join entry by entry and keep
                // object positions.
                let heap = if a.value != b.value && super::heaps::is_heap(ctx, layout, index)? {
                    super::heaps::join(ctx, facts, a.value, b.value, depth)?
                } else {
                    None
                };
                let value = match heap {
                    Some(value) => value,
                    None => facts.joined(ctx, a.value, b.value, depth)?,
                };
                Ok(Global {
                    value,
                    missing: a.missing || b.missing,
                })
            })?;
        changed |= self
            .written
            .merge(ctx, &other.written, |_, _, a, b| Ok(a || b))?;
        Ok(changed)
    }
}
