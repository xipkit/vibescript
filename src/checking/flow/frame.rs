use super::{Binding, blocks};
use crate::checking::{
    globals::{Global, Table},
    slots::Slots,
};
use crate::{CallContext, Result};

/// A walk state's bindings: lexical locals, then the shared slots at the global base.
///
/// Shared slots live in the same persistent table that call contexts and exits carry, so
/// entering, calling and leaving a function share that storage instead of copying every
/// shared binding. Shared slots have no lexical owner.
#[derive(Debug)]
pub(super) struct Frame {
    locals: Slots<Binding>,
    globals: Table<Global>,
}

impl Frame {
    pub fn new(locals: usize, globals: usize, empty: Binding) -> Self {
        Self {
            locals: Slots::new(locals, empty),
            globals: Table::new(globals, crate::checking::globals::ABSENT),
        }
    }

    pub fn len(&self) -> usize {
        self.locals.len() + self.globals.len()
    }

    /// The shared bindings, in the table that call contexts and exits carry.
    pub fn globals(&self) -> &Table<Global> {
        &self.globals
    }

    pub fn replace_globals(&mut self, globals: Table<Global>) {
        self.globals = globals;
    }

    pub fn grow_globals(&mut self, ctx: &mut CallContext, len: usize) -> Result<()> {
        self.globals.grow(ctx, len)
    }

    pub fn get(&self, ctx: &mut CallContext, slot: usize) -> Result<Binding> {
        let base = self.locals.len();
        if slot < base {
            return self.locals.get(ctx, slot);
        }
        Ok(shared(self.globals.get(ctx, slot - base)?))
    }

    pub fn set(&mut self, ctx: &mut CallContext, slot: usize, binding: Binding) -> Result<()> {
        let base = self.locals.len();
        if slot < base {
            return self.locals.set(ctx, slot, binding);
        }
        self.globals.set(
            ctx,
            slot - base,
            Global {
                value: binding.value,
                missing: binding.missing,
            },
        )
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            locals: self.locals.snapshot(ctx)?,
            globals: self.globals.snapshot(ctx)?,
        })
    }

    pub fn merge_indexed(
        &mut self,
        ctx: &mut CallContext,
        other: &Self,
        mut join: impl FnMut(&mut CallContext, usize, Binding, Binding) -> Result<Binding>,
    ) -> Result<bool> {
        let base = self.locals.len();
        let mut changed = self.locals.merge_indexed(ctx, &other.locals, &mut join)?;
        changed |= self
            .globals
            .merge(ctx, &other.globals, |ctx, index, a, b| {
                let binding = join(ctx, base + index, shared(a), shared(b))?;
                Ok(Global {
                    value: binding.value,
                    missing: binding.missing,
                })
            })?;
        Ok(changed)
    }

    /// Visits the slots, bindings and previous bindings that differ from `other`.
    pub fn changed(
        &self,
        ctx: &mut CallContext,
        other: &Self,
        visit: &mut impl FnMut(&mut CallContext, usize, Binding, Binding) -> Result<()>,
    ) -> Result<()> {
        let base = self.locals.len();
        self.locals.changed(ctx, &other.locals, visit)?;
        self.globals
            .changed(ctx, &other.globals, &mut |ctx, index, value, previous| {
                visit(ctx, base + index, shared(value), shared(previous))
            })
    }
}

fn shared(global: Global) -> Binding {
    Binding {
        value: global.value,
        missing: global.missing,
        owner: blocks::Owner::Unknown,
    }
}
