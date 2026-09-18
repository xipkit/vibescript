use super::{
    builtins,
    facts::{Fact, Facts},
    pending::Pending,
};
use crate::{
    CallContext, Result,
    budget::Buffer,
    bytecode::Program,
    types::{Type, TypeKind},
};
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

/// Identifies contracts that need live global type resolution instead of cached declarations.
pub(super) fn live_contract(ctx: &mut CallContext, program: &Program, ty: &Type) -> Result<bool> {
    let mut pending = Buffer::empty();
    pending.push(ctx, ty)?;
    while let Some(ty) = pending.data.pop() {
        ctx.charge(1)?;
        match &ty.kind {
            TypeKind::Named => {
                ctx.work_bytes(ty.name.len())?;
                let (root, qualified) = ty
                    .name
                    .split_once('.')
                    .map_or((ty.name.as_str(), false), |(root, _)| (root, true));
                for (global, _) in &program.globals {
                    ctx.charge(1)?;
                    if crate::types::binding_name_matches(
                        ctx,
                        global.name().as_bytes(),
                        root.as_bytes(),
                        !qualified,
                    )? {
                        return Ok(true);
                    }
                }
            }
            TypeKind::Array(Some(element)) => pending.push(ctx, element)?,
            TypeKind::Hash(Some(pair)) => pending.extend(ctx, &[&pair.0, &pair.1])?,
            TypeKind::Shape(fields, _) => {
                for field in fields {
                    ctx.charge(1)?;
                    pending.push(ctx, &field.ty)?;
                }
            }
            TypeKind::Union(types) => {
                for ty in types {
                    ctx.charge(1)?;
                    pending.push(ctx, ty)?;
                }
            }
            _ => (),
        }
    }
    Ok(false)
}
