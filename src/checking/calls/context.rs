use super::*;
use crate::checking::blocks::{Capture, Closure};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    Plain,
    Receiving { function: usize, given: bool },
    Invoked { given: bool },
}

pub(super) struct Context {
    pub kind: Kind,
    pub captures: Buffer<Capture>,
    pub arguments: Buffer<Fact>,
}

impl Context {
    pub fn plain() -> Self {
        Self {
            kind: Kind::Plain,
            captures: Buffer::empty(),
            arguments: Buffer::empty(),
        }
    }

    pub fn receiving(ctx: &mut CallContext, block: &Closure) -> Result<Self> {
        let mut result = Self::plain();
        result.kind = Kind::Receiving {
            function: block.function,
            given: block.given,
        };
        for link in &block.captures.data {
            result.captures.push(
                ctx,
                Capture {
                    slot: link.slot,
                    value: link.value,
                },
            )?;
        }
        Ok(result)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut next = Self::plain();
        next.kind = self.kind;
        next.captures.extend(ctx, &self.captures.data)?;
        next.arguments.extend(ctx, &self.arguments.data)?;
        Ok(next)
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge((self.captures.data.len() + self.arguments.data.len()) as u64 + 1)?;
        self.kind.hash(hash);
        self.captures.data.hash(hash);
        self.arguments.data.hash(hash);
        Ok(())
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge((self.captures.data.len() + self.arguments.data.len()) as u64 + 1)?;
        Ok(self.kind == other.kind
            && self.captures.data == other.captures.data
            && self.arguments.data == other.arguments.data)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        if self.kind != other.kind
            || self.captures.data.len() != other.captures.data.len()
            || self.arguments.data.len() != other.arguments.data.len()
        {
            return Ok(false);
        }
        for (a, b) in self.captures.data.iter().zip(&other.captures.data) {
            ctx.charge(1)?;
            if a.slot != b.slot {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn widen(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: usize,
    ) -> Result<bool> {
        let mut changed = false;
        for (a, b) in self.captures.data.iter_mut().zip(&other.captures.data) {
            ctx.charge(1)?;
            let value = facts.widen(ctx, a.value, b.value, depth)?;
            changed |= a.value != value;
            a.value = value;
        }
        for (a, b) in self.arguments.data.iter_mut().zip(&other.arguments.data) {
            ctx.charge(1)?;
            let value = facts.widen(ctx, *a, *b, depth)?;
            changed |= *a != value;
            *a = value;
        }
        Ok(changed)
    }

    pub fn incoming(&self, ctx: &mut CallContext) -> Result<Option<Closure>> {
        let Kind::Receiving { function, given } = self.kind else {
            return Ok(None);
        };
        let mut captures = Buffer::empty();
        for capture in &self.captures.data {
            captures.push(
                ctx,
                super::blocks::Link {
                    slot: capture.slot,
                    parent: usize::MAX,
                    value: capture.value,
                },
            )?;
        }
        Ok(Some(Closure {
            function,
            given,
            captures,
        }))
    }
}
