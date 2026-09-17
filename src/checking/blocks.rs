use super::{
    addresses::{Attached, Change},
    facts::{Atom, Fact, Facts, Node},
    pending::Pending,
    slots::Slots,
};
use crate::{CallContext, ErrorClass, Result, budget::Buffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Capture {
    pub slot: usize,
    pub value: Fact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Parent {
    Local(usize),
    Capture(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Link {
    pub slot: usize,
    pub parent: Parent,
    pub value: Fact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Layer {
    pub function: usize,
    pub given: bool,
    pub locals: usize,
}

#[derive(Debug)]
pub(super) struct Closure {
    pub function: usize,
    pub given: bool,
    pub locals: usize,
    // Each layer belongs to an earlier lexical function home, ordered nearest first.
    pub inherited: Buffer<Layer>,
    pub captures: Buffer<Link>,
    pub pending: Pending,
    // Return locations belong to the caller, and are not part of a callee context.
    pub destinations: Buffer<Parent>,
}

impl Closure {
    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        let mut captures = Buffer::empty();
        captures.extend(ctx, &self.captures.data)?;
        let mut inherited = Buffer::empty();
        inherited.extend(ctx, &self.inherited.data)?;
        let mut destinations = Buffer::empty();
        destinations.extend(ctx, &self.destinations.data)?;
        Ok(Self {
            pending: self.pending.snapshot(ctx)?,
            destinations,
            function: self.function,
            given: self.given,
            locals: self.locals,
            inherited,
            captures,
        })
    }

    pub fn extent(&self, ctx: &mut CallContext) -> Result<usize> {
        extent(ctx, self.locals, &self.inherited.data)
    }

    pub fn join(&mut self, ctx: &mut CallContext, facts: &mut Facts, other: &Self) -> Result<bool> {
        ctx.charge(self.inherited.data.len() as u64 + 1)?;
        assert_eq!((self.function, self.given), (other.function, other.given));
        assert_eq!(self.locals, other.locals);
        assert_eq!(self.inherited.data, other.inherited.data);
        assert_eq!(self.captures.data.len(), other.captures.data.len());
        ctx.charge(self.destinations.data.len() as u64 + 1)?;
        assert_eq!(self.destinations.data, other.destinations.data);
        let mut changed = self.pending.join(ctx, facts, &other.pending, None)?;
        for (a, b) in self.captures.data.iter_mut().zip(&other.captures.data) {
            ctx.charge(1)?;
            assert_eq!((a.slot, a.parent), (b.slot, b.parent));
            let value = facts.union(ctx, &[a.value, b.value])?;
            changed |= a.value != value;
            a.value = value;
        }
        Ok(changed)
    }
}

pub(super) fn extent(ctx: &mut CallContext, locals: usize, inherited: &[Layer]) -> Result<usize> {
    ctx.charge(1)?;
    let mut total = locals;
    for layer in inherited {
        ctx.charge(1)?;
        let Some(next) = total.checked_add(layer.locals) else {
            return ctx.fail(
                crate::ErrorKind::Memory,
                "checker capture layout size overflow",
            );
        };
        total = next;
    }
    Ok(total)
}

pub(super) struct Inputs<'a> {
    pub arguments: &'a [Fact],
    // The caller resolves lexical owners; absent entries are local to this invocation.
    pub captures: &'a [Capture],
    pub pending: &'a Pending,
    pub given: bool,
    pub inherited: &'a [Layer],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Completion {
    Value,
    // The home of the indicated callback layer, including the invoked block itself.
    Return(usize),
    Break(bool),
    Error(ErrorClass),
}

#[derive(Debug)]
pub(super) struct Exit {
    pub pc: usize,
    pub completion: Completion,
    pub value: Fact,
    pub captures: Slots<Fact>,
    pub written: Slots<bool>,
    pub pending: Pending,
}

impl Exit {
    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            pc: self.pc,
            completion: self.completion,
            value: self.value,
            captures: self.captures.snapshot(ctx)?,
            written: self.written.snapshot(ctx)?,
            pending: self.pending.snapshot(ctx)?,
        })
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.pc == other.pc
            && self.completion == other.completion
            && self.value == other.value
            && self.captures.equal(ctx, &other.captures)?
            && self.written.equal(ctx, &other.written)?
            && self.pending.equal(ctx, &other.pending)?)
    }

    pub fn widen(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        previous: &Self,
        depth: usize,
    ) -> Result<()> {
        self.pending
            .join(ctx, facts, &previous.pending, Some(depth))?;
        self.value = facts.widen(ctx, previous.value, self.value, depth)?;
        self.captures.merge(ctx, &previous.captures, |ctx, a, b| {
            facts.widen(ctx, b, a, depth)
        })?;
        self.written
            .merge(ctx, &previous.written, |_, a, b| Ok(a || b))?;
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Captures {
    values: Slots<Fact>,
    attached: Slots<Attached>,
    written: Slots<bool>,
    pub pending: Pending,
}

impl Captures {
    pub fn new(ctx: &mut CallContext, locals: usize, inputs: &[Capture]) -> Result<Self> {
        let mut result = Self {
            values: Slots::new(locals, Atom::Never.fact()),
            attached: Slots::new(locals, Attached::No),
            written: Slots::new(locals, false),
            pending: Pending::new(),
        };
        for input in inputs {
            ctx.charge(1)?;
            result.values.set(ctx, input.slot, input.value)?;
            result.attached.set(ctx, input.slot, Attached::Yes)?;
        }
        Ok(result)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            values: self.values.snapshot(ctx)?,
            attached: self.attached.snapshot(ctx)?,
            written: self.written.snapshot(ctx)?,
            pending: self.pending.snapshot(ctx)?,
        })
    }

    pub fn attachment(&self, ctx: &mut CallContext, slot: usize) -> Result<Attached> {
        self.attached.get(ctx, slot)
    }

    pub fn refresh(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
        change: &Change<'_>,
    ) -> Result<()> {
        let attached = self.attached.get(ctx, slot)?;
        if attached == Attached::No {
            return Ok(());
        }
        for address in &mut self.pending.addresses.data {
            ctx.charge(1)?;
            if address.root != Some(slot) {
                continue;
            }
            if attached == Attached::Yes {
                address.refresh(ctx, facts, value, change)?;
            } else {
                let mut updated = address.snapshot(ctx)?;
                updated.refresh(ctx, facts, value, change)?;
                address.join(ctx, facts, &updated, None)?;
            }
        }
        Ok(())
    }

    pub fn shadow(&mut self, ctx: &mut CallContext, slot: usize) -> Result<()> {
        self.attached.set(ctx, slot, Attached::No)
    }

    pub fn store(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
    ) -> Result<()> {
        let value = match self.attached.get(ctx, slot)? {
            Attached::No => return Ok(()),
            Attached::Yes => value,
            Attached::Maybe => {
                let previous = self.values.get(ctx, slot)?;
                facts.union(ctx, &[previous, value])?
            }
        };
        self.written.set(ctx, slot, true)?;
        self.values.set(ctx, slot, value)
    }

    pub fn value(&self, ctx: &mut CallContext, slot: usize) -> Result<Fact> {
        self.values.get(ctx, slot)
    }

    pub fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        let values = self.values.merge(ctx, &other.values, |ctx, a, b| {
            facts.joined(ctx, a, b, depth)
        })?;
        let attached = self.attached.merge(ctx, &other.attached, |_, a, b| {
            Ok(if a == b { a } else { Attached::Maybe })
        })?;
        let written = self
            .written
            .merge(ctx, &other.written, |_, a, b| Ok(a || b))?;
        let pending = self.pending.join(ctx, facts, &other.pending, depth)?;
        Ok(values || attached || written || pending)
    }

    pub fn record(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        exits: &mut Buffer<Exit>,
        pc: usize,
        completion: Completion,
        value: Fact,
    ) -> Result<()> {
        for exit in &mut exits.data {
            ctx.charge(1)?;
            if exit.pc == pc && exit.completion == completion {
                exit.pending.join(ctx, facts, &self.pending, None)?;
                exit.value = facts.union(ctx, &[exit.value, value])?;
                exit.captures
                    .merge(ctx, &self.values, |ctx, a, b| facts.union(ctx, &[a, b]))?;
                exit.written
                    .merge(ctx, &self.written, |_, a, b| Ok(a || b))?;
                return Ok(());
            }
        }
        let captures = self.values.snapshot(ctx)?;
        let written = self.written.snapshot(ctx)?;
        let pending = self.pending.snapshot(ctx)?;
        exits.push(
            ctx,
            Exit {
                pc,
                completion,
                value,
                captures,
                written,
                pending,
            },
        )
    }
}

pub(super) fn argument(
    ctx: &mut CallContext,
    facts: &mut Facts,
    args: &[Fact],
    index: usize,
    autosplat: bool,
) -> Result<Fact> {
    if !autosplat || args.len() != 1 {
        ctx.charge(1)?;
        return Ok(args.get(index).copied().unwrap_or(Atom::Nil.fact()));
    }
    let mut value = Atom::Never.fact();
    let Ok(index_value) = i64::try_from(index) else {
        ctx.charge(1)?;
        return Ok(Atom::Nil.fact());
    };
    let key = facts.integer(ctx, index_value)?;
    for i in 0..facts.arm_count(args[0]) {
        ctx.charge(1)?;
        let arm = facts.arm(args[0], i);
        let next = match facts.node(arm) {
            Node::Array(_) | Node::Tuple(_) => facts.collection_index(ctx, arm, &[key])?.value,
            Node::Atom(Atom::Unknown | Atom::Any | Atom::Never) => arm,
            _ if index == 0 => arm,
            _ => Atom::Nil.fact(),
        };
        value = facts.union(ctx, &[value, next])?;
    }
    Ok(value)
}
