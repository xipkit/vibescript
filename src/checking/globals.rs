use super::{
    builtins,
    facts::{Atom, Fact, Facts},
    pending::Pending,
};
use crate::{CallContext, Result, budget::Buffer, bytecode::Program};
use std::hash::{Hash, Hasher};

// Compiled globals, supplied roots, declarations and namespace heaps share one address layout.
#[derive(Debug)]
pub(super) struct Globals {
    pub values: Buffer<Fact>,
    pub missing: Buffer<bool>,
    pub written: Buffer<bool>,
    pub pending: Pending,
}

impl Globals {
    pub fn empty() -> Self {
        Self {
            values: Buffer::empty(),
            missing: Buffer::empty(),
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
            globals.missing.push(ctx, false)?;
            globals.written.push(ctx, false)?;
        }
        Ok(globals)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        let mut globals = Self::empty();
        globals.values.extend(ctx, &self.values.data)?;
        globals.missing.extend(ctx, &self.missing.data)?;
        globals.written.extend(ctx, &self.written.data)?;
        globals.pending = self.pending.snapshot(ctx)?;
        Ok(globals)
    }

    pub fn roots(&mut self, ctx: &mut CallContext, roots: &[super::calls::Root]) -> Result<()> {
        for root in roots {
            ctx.charge(1)?;
            self.values.push(ctx, root.value)?;
            self.missing.push(ctx, root.missing)?;
            self.written.push(ctx, false)?;
        }
        Ok(())
    }

    pub fn files(
        &mut self,
        ctx: &mut CallContext,
        layout: &super::file_bindings::Layout,
    ) -> Result<()> {
        for _ in &layout.names.data {
            ctx.charge(1)?;
            self.values.push(ctx, Atom::Never.fact())?;
            self.missing.push(ctx, true)?;
            self.written.push(ctx, false)?;
        }
        Ok(())
    }

    pub fn namespaces(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        program: &Program,
        owner: usize,
    ) -> Result<()> {
        for declaration in &program.declarations {
            ctx.charge(1)?;
            let value = match &declaration.0 {
                crate::value::Kind::Namespace(namespace) => super::namespaces::value(
                    ctx,
                    facts,
                    program,
                    owner,
                    namespace.definition.index,
                )?,
                crate::value::Kind::Enum(_) => facts.enumeration(ctx, declaration)?,
                _ => unreachable!(),
            };
            self.values.push(ctx, value)?;
            self.missing.push(ctx, false)?;
            self.written.push(ctx, false)?;
        }
        for (module, definition) in program.namespaces.iter().enumerate() {
            ctx.charge(1)?;
            let fields = super::namespaces::initial(ctx, facts, program, owner, module)?;
            let initialized = facts.boolean(ctx, definition.body.is_none())?;
            let instances = facts.tuple(ctx, &[])?;
            for value in [fields, initialized, instances, Atom::Never.fact()] {
                self.values.push(ctx, value)?;
                self.missing.push(ctx, false)?;
                self.written.push(ctx, false)?;
            }
        }
        Ok(())
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge(
            self.values.data.len() as u64
                + self.written.data.len() as u64
                + self.missing.data.len() as u64
                + 1,
        )?;
        self.values.data.hash(hash);
        self.missing.data.hash(hash);
        self.written.data.hash(hash);
        self.pending.hash(ctx, hash)
    }

    /// Keeps recursive calls on either side of initialization in separate contexts.
    pub fn same_initialization(
        &self,
        ctx: &mut CallContext,
        program: &Program,
        other: &Self,
    ) -> Result<bool> {
        ctx.charge(1)?;
        for module in 0..program.namespaces.len() {
            ctx.charge(1)?;
            let a = super::namespaces::slot(self.values.data.len(), program, module) + 1;
            let b = super::namespaces::slot(other.values.data.len(), program, module) + 1;
            if self.values.data[a] != other.values.data[b] {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(
            self.values.data.len() as u64
                + self.written.data.len() as u64
                + self.missing.data.len() as u64
                + 1,
        )?;
        Ok(self.values.data == other.values.data
            && self.missing.data == other.missing.data
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
        for (a, b) in self.missing.data.iter_mut().zip(&other.missing.data) {
            ctx.charge(1)?;
            changed |= !*a && *b;
            *a |= *b;
        }
        Ok(changed)
    }
}
