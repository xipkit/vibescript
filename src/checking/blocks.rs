use super::{
    addresses::{Attached, Change},
    facts::{Atom, Fact, Facts, Node},
    globals::Globals,
    pending::Pending,
    slots::Slots,
    sources::CallableId,
};
use crate::{CallContext, ErrorClass, Result, budget::Buffer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum Scope {
    #[default]
    Invocation,
    Declaration {
        given: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Owner {
    Function(CallableId),
    Unknown,
}

impl Owner {
    /// Combines possible binding owners while ignoring absent values.
    pub fn join(self, value: Fact, other: Self, other_value: Fact) -> Self {
        if value == Atom::Never.fact() {
            other
        } else if other_value == Atom::Never.fact() || self == other {
            self
        } else {
            Self::Unknown
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Capture {
    pub slot: usize,
    pub value: Fact,
    pub missing: bool,
    pub owner: Owner,
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
    pub missing: bool,
    pub owner: Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Layer {
    pub scope: Scope,
    pub function: CallableId,
    pub receiver: Option<Fact>,
    pub ambient: Option<CallableId>,
    pub given: bool,
    pub locals: usize,
}

#[derive(Debug)]
pub(super) struct Closure {
    pub scope: Scope,
    pub function: CallableId,
    pub receiver: Option<Fact>,
    // Hidden local slots carry the declaring frame's raw bindings, separately from lexical aliases.
    pub ambient: Option<CallableId>,
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
            scope: self.scope,
            pending: self.pending.snapshot(ctx)?,
            destinations,
            function: self.function,
            receiver: self.receiver,
            ambient: self.ambient,
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
        assert_eq!(self.scope, other.scope);
        assert_eq!(self.ambient, other.ambient);
        assert_eq!(self.locals, other.locals);
        assert_eq!(self.inherited.data, other.inherited.data);
        assert_eq!(self.captures.data.len(), other.captures.data.len());
        ctx.charge(self.destinations.data.len() as u64 + 1)?;
        assert_eq!(self.destinations.data, other.destinations.data);
        let mut changed = self.pending.join(ctx, facts, &other.pending, None)?;
        if self.receiver != other.receiver {
            let receiver = facts.union(
                ctx,
                &[
                    self.receiver.unwrap_or(Atom::Nil.fact()),
                    other.receiver.unwrap_or(Atom::Nil.fact()),
                ],
            )?;
            changed |= self.receiver != Some(receiver);
            self.receiver = Some(receiver);
        }
        for (a, b) in self.captures.data.iter_mut().zip(&other.captures.data) {
            ctx.charge(1)?;
            assert_eq!((a.slot, a.parent), (b.slot, b.parent));
            let value = facts.union(ctx, &[a.value, b.value])?;
            let owner = a.owner.join(a.value, b.owner, b.value);
            changed |= a.value != value || (!a.missing && b.missing) || a.owner != owner;
            a.value = value;
            a.missing |= b.missing;
            a.owner = owner;
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
    // An unknown incoming block returns to a caller outside the declaration scope.
    Escape,
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
    pub refined: Slots<bool>,
    pub pending: Pending,
    pub globals: Globals,
}

impl Exit {
    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            pc: self.pc,
            completion: self.completion,
            value: self.value,
            globals: self.globals.snapshot(ctx)?,
            captures: self.captures.snapshot(ctx)?,
            written: self.written.snapshot(ctx)?,
            refined: self.refined.snapshot(ctx)?,
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
            && self.refined.equal(ctx, &other.refined)?
            && self.pending.equal(ctx, &other.pending)?
            && self.globals.equal(ctx, &other.globals)?)
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
        self.globals
            .join(ctx, facts, &previous.globals, Some(depth))?;
        self.value = facts.widen(ctx, previous.value, self.value, depth)?;
        self.captures.merge(ctx, &previous.captures, |ctx, a, b| {
            facts.widen(ctx, b, a, depth)
        })?;
        self.written
            .merge(ctx, &previous.written, |_, a, b| Ok(a || b))?;
        self.refined
            .merge(ctx, &previous.refined, |_, a, b| Ok(a || b))?;
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct Captures {
    values: Slots<Fact>,
    missing: Slots<bool>,
    owners: Slots<Owner>,
    attached: Slots<Attached>,
    written: Slots<bool>,
    refined: Slots<bool>,
    pub pending: Pending,
}

impl Captures {
    pub fn new(ctx: &mut CallContext, locals: usize, inputs: &[Capture]) -> Result<Self> {
        let mut result = Self {
            values: Slots::new(locals, Atom::Never.fact()),
            missing: Slots::new(locals, true),
            owners: Slots::new(locals, Owner::Unknown),
            attached: Slots::new(locals, Attached::No),
            written: Slots::new(locals, false),
            refined: Slots::new(locals, false),
            pending: Pending::new(),
        };
        for input in inputs {
            ctx.charge(1)?;
            result.values.set(ctx, input.slot, input.value)?;
            result.missing.set(ctx, input.slot, input.missing)?;
            result.owners.set(ctx, input.slot, input.owner)?;
            let attached = if !input.missing {
                Attached::Yes
            } else if input.value == Atom::Never.fact() {
                Attached::No
            } else {
                Attached::Maybe
            };
            result.attached.set(ctx, input.slot, attached)?;
        }
        Ok(result)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            values: self.values.snapshot(ctx)?,
            missing: self.missing.snapshot(ctx)?,
            owners: self.owners.snapshot(ctx)?,
            attached: self.attached.snapshot(ctx)?,
            written: self.written.snapshot(ctx)?,
            refined: self.refined.snapshot(ctx)?,
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

    /// Records a path constraint without publishing a write to the enclosing scope.
    pub fn refine(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
    ) -> Result<()> {
        if self.attached.get(ctx, slot)? == Attached::Yes {
            self.values.set(ctx, slot, value)?;
            self.refined.set(ctx, slot, true)?;
        }
        self.refresh(ctx, facts, slot, value, &Change::Refine)
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

    /// Reports whether the original enclosing binding may be absent.
    pub fn missing(&self, ctx: &mut CallContext, slot: usize) -> Result<bool> {
        self.missing.get(ctx, slot)
    }

    /// Returns the lexical owner of an enclosing binding when it is known.
    pub fn owner(&self, ctx: &mut CallContext, slot: usize) -> Result<Owner> {
        self.owners.get(ctx, slot)
    }

    pub fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        let owners = self.owners.merge(ctx, &other.owners, |_, a, b| {
            Ok(if a == b { a } else { Owner::Unknown })
        })?;
        let values = self.values.merge(ctx, &other.values, |ctx, a, b| {
            facts.joined(ctx, a, b, depth)
        })?;
        let attached = self.attached.merge(ctx, &other.attached, |_, a, b| {
            Ok(if a == b { a } else { Attached::Maybe })
        })?;
        let missing = self
            .missing
            .merge(ctx, &other.missing, |_, a, b| Ok(a || b))?;
        let written = self
            .written
            .merge(ctx, &other.written, |_, a, b| Ok(a || b))?;
        let refined = self
            .refined
            .merge(ctx, &other.refined, |_, a, b| Ok(a || b))?;
        let pending = self.pending.join(ctx, facts, &other.pending, depth)?;
        Ok(values || missing || owners || attached || written || refined || pending)
    }

    pub fn record(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        (exits, index): (&mut Buffer<Exit>, &mut ExitIndex),
        completion: (usize, Completion, Fact),
        globals: Globals,
    ) -> Result<()> {
        let (pc, completion, value) = completion;
        let mut cursor = index.first(exits, pc);
        while let Some(position) = cursor {
            ctx.charge(1)?;
            cursor = index.after(exits, pc, position);
            let exit = &mut exits.data[position];
            if exit.pc == pc
                && exit.completion == completion
                && exit.pending.compatible(ctx, &self.pending)?
                && exit.globals.compatible(ctx, &globals)?
            {
                exit.pending.join(ctx, facts, &self.pending, None)?;
                exit.globals.join(ctx, facts, &globals, None)?;
                exit.value = facts.union(ctx, &[exit.value, value])?;
                exit.captures
                    .merge(ctx, &self.values, |ctx, a, b| facts.union(ctx, &[a, b]))?;
                exit.written
                    .merge(ctx, &self.written, |_, a, b| Ok(a || b))?;
                exit.refined
                    .merge(ctx, &self.refined, |_, a, b| Ok(a || b))?;
                return Ok(());
            }
        }
        let captures = self.values.snapshot(ctx)?;
        let written = self.written.snapshot(ctx)?;
        let refined = self.refined.snapshot(ctx)?;
        let pending = self.pending.snapshot(ctx)?;
        index.link(ctx, exits, pc)?;
        exits.push(
            ctx,
            Exit {
                pc,
                completion,
                value,
                captures,
                written,
                refined,
                pending,
                globals,
            },
        )
    }
}

/// Chains recorded block exits by program counter in record order, so recording an exit
/// visits only the earlier exits at the same instruction instead of every exit.
#[derive(Debug)]
pub(super) struct ExitIndex {
    // The first and last exits at each program counter, and the next exit at the same one.
    first: Buffer<usize>,
    last: Buffer<usize>,
    next: Buffer<usize>,
}

impl ExitIndex {
    pub fn new() -> Self {
        Self {
            first: Buffer::empty(),
            last: Buffer::empty(),
            next: Buffer::empty(),
        }
    }

    // An exit list extended elsewhere is scanned in full rather than trusting stale chains.
    fn indexed(&self, exits: &Buffer<Exit>) -> bool {
        self.next.data.len() == exits.data.len()
    }

    fn first(&self, exits: &Buffer<Exit>, pc: usize) -> Option<usize> {
        if !self.indexed(exits) {
            return (!exits.data.is_empty()).then_some(0);
        }
        self.first
            .data
            .get(pc)
            .copied()
            .filter(|&position| position != usize::MAX)
    }

    fn after(&self, exits: &Buffer<Exit>, pc: usize, position: usize) -> Option<usize> {
        let next = if self.indexed(exits) && exits.data[position].pc == pc {
            self.next.data[position]
        } else {
            position + 1
        };
        (next < exits.data.len()).then_some(next)
    }

    fn link(&mut self, ctx: &mut CallContext, exits: &Buffer<Exit>, pc: usize) -> Result<()> {
        if !self.indexed(exits) {
            return Ok(());
        }
        if pc >= self.first.data.len() {
            let Some(length) = pc.checked_add(1) else {
                return ctx.fail(crate::ErrorKind::Memory, "checker exit index size overflow");
            };
            let capacity = length.max(self.first.data.len().saturating_mul(2));
            ctx.charge((2 * (capacity - self.first.data.len())) as u64)?;
            self.first.ensure(ctx, capacity)?;
            self.last.ensure(ctx, capacity)?;
            self.first.data.resize(capacity, usize::MAX);
            self.last.data.resize(capacity, usize::MAX);
        }
        let position = exits.data.len();
        match self.last.data[pc] {
            usize::MAX => self.first.data[pc] = position,
            previous => self.next.data[previous] = position,
        }
        self.last.data[pc] = position;
        self.next.push(ctx, usize::MAX)
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
