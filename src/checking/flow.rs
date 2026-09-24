use super::{
    addresses::{Address, Attached, Change},
    arguments::{Arguments, Failure, Input},
    blocks, builtins,
    calls::{Calls, Root, Target},
    facts::{Atom, Fact, Facts, HashKind},
    globals::{
        Globals, Table,
        layout::{Layout as GlobalLayout, Source as SourceSlots},
    },
    graph::{Block, Exit, Graph},
    lexical::Layouts,
    relation::Relation,
    scalar::Test,
    slots::Slots,
    sources::{CallableId, SourceId},
};
use crate::{
    CallContext, ErrorClass, Result,
    budget::Buffer,
    bytecode::{ArgumentOp, CallSite, Function, Invocation, Method, Op, Program, Receiving},
    value::Kind,
};

mod ambient;
mod attached;
mod bindings;
mod bypass;
mod call_targets;
mod callbacks;
mod collection_blocks;
mod declarations;
mod effects;
mod files;
mod frame;
pub(super) mod general;
mod globals;
mod handlers;
mod host_blocks;
mod member_addresses;
mod namespaces;
mod native;
mod operators;
mod publication;
mod rendering;
mod requires;
mod roots;
mod splats;
mod types;
use handlers::{Phase, Transfer};
use native::MemberSite;
use publication::MutationOutput;

// Absence must survive joins with inherited rescued errors.
pub(super) const NO_ERROR: u16 = 1 << 8;
const INVALID_CLASS: u16 = 1 << 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IssueKind {
    DetachedValue(Target),
    /// A member receiver kept executable code as a value, which has no members.
    CallableMember {
        target: Target,
        member: usize,
    },
    TypeBinding {
        ty: usize,
        ambiguous: bool,
    },
    TypeFactBinding {
        expected: Fact,
        ambiguous: bool,
    },
    MissingBlock,
    LoopControl {
        breaking: bool,
    },
    Ordering {
        name: usize,
        left: Fact,
        right: Fact,
    },
    BlockGivenArguments,
    CallbackResult {
        name: usize,
        actual: Fact,
        expected: Fact,
    },
    Index {
        receiver: Fact,
        arguments: Fact,
    },
    Write {
        receiver: Fact,
        selectors: Fact,
        value: Fact,
    },
    Member {
        name: usize,
        receiver: Fact,
        arguments: Fact,
    },
    Range {
        value: Fact,
    },
    Iterate {
        value: Fact,
    },
    CaseSplat {
        value: Fact,
    },
    Regex {
        pattern: usize,
    },
    Raise {
        value: Fact,
    },
    Call {
        target: Target,
        failure: Failure,
    },
    Splat {
        actual: Fact,
        keyword: bool,
    },
    Return {
        actual: Fact,
        expected: Fact,
    },
    Default {
        actual: Fact,
        expected: Fact,
    },
    Property {
        actual: Fact,
        expected: Fact,
    },
    Reassignment {
        slot: usize,
        before: Fact,
        after: Fact,
    },
    Unary {
        op: &'static str,
        value: Fact,
    },
    Operator {
        name: &'static str,
        receiver: Fact,
    },
    Binary {
        op: &'static str,
        left: Fact,
        right: Fact,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Issue {
    pub pc: usize,
    pub kind: IssueKind,
}

#[derive(Debug)]
pub(super) struct Report {
    pub returns: Fact,
    pub normal_returns: Fact,
    pub throws: u8,
    pub issues: Buffer<Issue>,
    pub incomplete: Buffer<usize>,
    pub block_exits: Buffer<blocks::Exit>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    value: Fact,
    missing: bool,
    owner: blocks::Owner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Predicate {
    slot: usize,
    test: Test,
    yes: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Operand {
    value: Fact,
    origin: Option<usize>,
    predicate: Option<Predicate>,
    literal: Option<usize>,
    fresh: bool,
}

impl Operand {
    /// Maps the value and any fact its retained predicate tests against.
    fn rename(self, ctx: &mut CallContext, rename: &mut super::heaps::Rename<'_>) -> Result<Self> {
        let predicate = match self.predicate {
            Some(Predicate {
                slot,
                test: Test::Case { matcher, splat },
                yes,
            }) => Some(Predicate {
                slot,
                test: Test::Case {
                    matcher: rename(ctx, matcher)?,
                    splat,
                },
                yes,
            }),
            Some(Predicate {
                slot,
                test: Test::Integer { comparison, other },
                yes,
            }) => Some(Predicate {
                slot,
                test: Test::Integer {
                    comparison,
                    other: rename(ctx, other)?,
                },
                yes,
            }),
            predicate => predicate,
        };
        Ok(Self {
            value: rename(ctx, self.value)?,
            predicate,
            ..self
        })
    }

    fn invalidate(&mut self, slot: usize) {
        if self.origin == Some(slot) {
            self.origin = None;
        }
        if self.predicate.is_some_and(|p| p.slot == slot) {
            self.predicate = None;
        }
    }
    fn new(value: Fact) -> Self {
        Self {
            value,
            origin: None,
            predicate: None,
            literal: None,
            fresh: false,
        }
    }

    fn local(value: Fact, slot: usize) -> Self {
        Self {
            origin: Some(slot),
            ..Self::new(value)
        }
    }

    fn predicate(self) -> Option<Predicate> {
        self.predicate.or_else(|| {
            self.origin.map(|slot| Predicate {
                slot,
                test: Test::Truth,
                yes: true,
            })
        })
    }

    fn join(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: Self,
        depth: Option<usize>,
    ) -> Result<Self> {
        Ok(Self {
            value: facts.joined(ctx, self.value, other.value, depth)?,
            origin: if self.origin == other.origin {
                self.origin
            } else {
                None
            },
            predicate: if self.predicate == other.predicate {
                self.predicate
            } else {
                None
            },
            fresh: self.fresh && other.fresh,
            literal: if self.literal == other.literal {
                self.literal
            } else {
                None
            },
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Loop {
    end: usize,
    base: usize,
    argument_base: usize,
    address_base: usize,
    attempt_base: usize,
    raise_base: usize,
    text_base: usize,
    expression: bool,
    source: Fact,
    repeat: Fact,
    last: Fact,
    result: Fact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Attempt {
    spec: usize,
    stack: usize,
    arguments: usize,
    addresses: usize,
    loops: usize,
    raises: usize,
    texts: usize,
    phase: Phase,
    error: u8,
    pending: Option<Transfer>,
}

#[derive(Debug)]
struct State {
    function: CallableId,
    global_layout: GlobalLayout,
    source_slots: std::sync::Arc<SourceSlots>,
    // Global address roots occupy the suffix after lexical locals and captures.
    global_base: usize,
    global_count: usize,
    global_pending: super::pending::Pending,
    global_written: Table<bool>,
    locals: frame::Frame,
    captures: Option<blocks::Captures>,
    capture_locals: bool,
    stack: Buffer<Operand>,
    loops: Buffer<Loop>,
    arguments: Buffer<Pending>,
    addresses: Buffer<Address>,
    attempts: Buffer<Attempt>,
    raises: Buffer<u16>,
    texts: Buffer<Fact>,
    widening: Option<usize>,
    integer_thresholds: Buffer<i64>,
}

#[derive(Debug)]
struct Pending {
    target: Target,
    receiver: Option<Fact>,
    arguments: Arguments,
}

impl State {
    /// Collects the binding and capture values that differ from `entry`, each with the value
    /// it replaced.
    fn changed_values(
        &self,
        ctx: &mut CallContext,
        entry: &Self,
        values: &mut Buffer<(Fact, Fact)>,
    ) -> Result<()> {
        self.locals
            .changed(ctx, &entry.locals, &mut |ctx, _, binding, previous| {
                values.push(ctx, (binding.value, previous.value))
            })?;
        if let (Some(captures), Some(before)) = (&self.captures, &entry.captures) {
            let mut changed = Buffer::empty();
            captures.changed(ctx, before, &mut changed)?;
            for (_, value, previous) in changed.data {
                values.push(ctx, (value, previous))?;
            }
        }
        Ok(())
    }

    /// Merges the structural alternatives of the binding and capture values that gained
    /// collection alternatives since `entry`, as a loop backedge widens them.
    fn generalize_changed(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        entry: &Self,
        depth: usize,
    ) -> Result<()> {
        let mut bindings = Buffer::empty();
        self.locals
            .changed(ctx, &entry.locals, &mut |ctx, slot, binding, previous| {
                bindings.push(ctx, (slot, binding, previous.value))
            })?;
        for (slot, binding, previous) in bindings.data {
            if facts.alternatives(binding.value) > facts.alternatives(previous) {
                // Merging heap alternatives would lose object positions; coalesce them.
                let heap = slot >= self.global_base
                    && super::heaps::is_heap(ctx, &self.global_layout, slot - self.global_base)?;
                let value = match heap {
                    true => match super::heaps::entries(ctx, facts, binding.value)? {
                        Some(entries) => facts.tuple(ctx, &entries.data)?,
                        None => facts.generalize(ctx, binding.value, depth)?,
                    },
                    false => facts.generalize(ctx, binding.value, depth)?,
                };
                self.locals.set(ctx, slot, Binding { value, ..binding })?;
            }
        }
        if let (Some(captures), Some(before)) = (&mut self.captures, &entry.captures) {
            let mut changed = Buffer::empty();
            captures.changed(ctx, before, &mut changed)?;
            for (slot, value, previous) in changed.data {
                if facts.alternatives(value) > facts.alternatives(previous) {
                    let value = facts.generalize(ctx, value, depth)?;
                    captures.replace(ctx, slot, value)?;
                }
            }
        }
        Ok(())
    }

    fn polarity(&self, ctx: &mut CallContext, facts: &mut Facts) -> Result<usize> {
        let Some(operand) = self.stack.data.last() else {
            return Ok(0);
        };
        let result = facts.test_result(ctx, operand.value, Test::Truth)?;
        Ok(match facts.node(result) {
            super::facts::Node::Boolean(false) => 1,
            super::facts::Node::Boolean(true) => 2,
            _ => 0,
        })
    }
    fn new(
        ctx: &mut CallContext,
        locals: usize,
        function: CallableId,
        layout: &GlobalLayout,
    ) -> Result<Self> {
        let globals = layout.len();
        if locals.checked_add(globals).is_none() {
            return ctx.fail(crate::ErrorKind::Memory, "checker state size overflow");
        }
        Ok(Self {
            function,
            global_layout: layout.clone(),
            source_slots: layout.source(ctx, function.source)?,
            global_base: locals,
            global_count: globals,
            global_pending: super::pending::Pending::new(),
            global_written: Table::new(globals, false),
            captures: None,
            capture_locals: false,
            locals: frame::Frame::new(
                locals,
                globals,
                Binding {
                    value: Atom::Never.fact(),
                    missing: true,
                    owner: blocks::Owner::Function(function),
                },
            ),
            stack: Buffer::empty(),
            loops: Buffer::empty(),
            arguments: Buffer::empty(),
            addresses: Buffer::empty(),
            attempts: Buffer::empty(),
            raises: Buffer::empty(),
            texts: Buffer::empty(),
            widening: None,
            integer_thresholds: Buffer::empty(),
        })
    }

    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut state = Self {
            function: self.function,
            global_layout: self.global_layout.clone(),
            source_slots: self.source_slots.clone(),
            global_base: self.global_base,
            global_count: self.global_count,
            global_pending: self.global_pending.snapshot(ctx)?,
            global_written: self.global_written.snapshot(ctx)?,
            locals: self.locals.snapshot(ctx)?,
            capture_locals: self.capture_locals,
            captures: self
                .captures
                .as_ref()
                .map(|c| c.snapshot(ctx))
                .transpose()?,
            stack: Buffer::empty(),
            loops: Buffer::empty(),
            arguments: Buffer::empty(),
            addresses: Buffer::empty(),
            attempts: Buffer::empty(),
            raises: Buffer::empty(),
            texts: Buffer::empty(),
            widening: None,
            integer_thresholds: Buffer::empty(),
        };
        state.stack.extend(ctx, &self.stack.data)?;
        state.loops.extend(ctx, &self.loops.data)?;
        state.attempts.extend(ctx, &self.attempts.data)?;
        state.raises.extend(ctx, &self.raises.data)?;
        state.texts.extend(ctx, &self.texts.data)?;
        for pending in &self.arguments.data {
            ctx.charge(1)?;
            let arguments = pending.arguments.snapshot(ctx)?;
            state.arguments.push(
                ctx,
                Pending {
                    target: pending.target,
                    receiver: pending.receiver,
                    arguments,
                },
            )?;
        }
        for address in &self.addresses.data {
            let address = address.snapshot(ctx)?;
            state.addresses.push(ctx, address)?;
        }
        Ok(state)
    }

    /// Renames folded objects throughout the state and merges their heap entries.
    fn fold(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        renamer: &mut super::heaps::Renamer<'_>,
    ) -> Result<()> {
        if !renamer.active() {
            return Ok(());
        }
        for slot in 0..self.locals.len() {
            let binding = self.locals.get(ctx, slot)?;
            let mut value = renamer.fact(ctx, facts, binding.value)?;
            if slot >= self.global_base {
                value = renamer.merge(ctx, facts, slot - self.global_base, value)?;
            }
            if value != binding.value {
                self.locals.set(ctx, slot, Binding { value, ..binding })?;
            }
        }
        let rename = &mut |ctx: &mut CallContext, fact| renamer.fact(ctx, facts, fact);
        if let Some(captures) = &mut self.captures {
            captures.rename(ctx, rename)?;
        }
        self.global_pending.rename(ctx, rename)?;
        for operand in &mut self.stack.data {
            *operand = operand.rename(ctx, rename)?;
        }
        for current in &mut self.loops.data {
            current.source = rename(ctx, current.source)?;
            current.repeat = rename(ctx, current.repeat)?;
            current.last = rename(ctx, current.last)?;
            current.result = rename(ctx, current.result)?;
        }
        for pending in &mut self.arguments.data {
            pending.target = pending.target.rename(ctx, rename)?;
            if let Some(receiver) = &mut pending.receiver {
                *receiver = rename(ctx, *receiver)?;
            }
            pending.arguments.rename(ctx, rename)?;
        }
        for address in &mut self.addresses.data {
            address.rename(ctx, rename)?;
        }
        for attempt in &mut self.attempts.data {
            if let Some(transfer) = attempt.pending {
                attempt.pending = Some(transfer.rename(ctx, rename)?);
            }
        }
        for text in &mut self.texts.data {
            *text = rename(ctx, *text)?;
        }
        Ok(())
    }

    /// Finds the objects `other` allocated beyond this state's heaps at a backedge.
    fn heap_folds(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
    ) -> Result<Buffer<super::heaps::Fold>> {
        let (base, other_base) = (self.global_base, other.global_base);
        super::heaps::folds(
            ctx,
            facts,
            &self.global_layout,
            None,
            |ctx, heap| Ok(Some(self.locals.get(ctx, base + heap)?.value)),
            |ctx, heap| {
                Ok(if heap < other.global_count {
                    Some(other.locals.get(ctx, other_base + heap)?.value)
                } else {
                    None
                })
            },
        )
    }

    fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        backedge: bool,
        program: &Program,
    ) -> Result<bool> {
        self.join_folding(ctx, facts, other, backedge, program)
            .map(|(changed, _)| changed)
    }

    /// Joins `other` into this state. At a backedge, objects `other` allocated beyond this
    /// state's heaps fold into summary entries; the folds are returned so callers can rename
    /// the facts they keep beside the state.
    fn join_folding(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        backedge: bool,
        program: &Program,
    ) -> Result<(bool, Buffer<super::heaps::Fold>)> {
        let mut changed = self.expand(ctx, &other.global_layout)?;
        let mut normalized;
        let mut other = if other.global_layout.same(&self.global_layout) {
            other
        } else {
            normalized = other.snapshot(ctx)?;
            normalized.expand(ctx, &self.global_layout)?;
            &normalized
        };
        let folds = if backedge {
            self.heap_folds(ctx, facts, other)?
        } else {
            Buffer::empty()
        };
        let mut folded;
        if !folds.data.is_empty() {
            folded = other.snapshot(ctx)?;
            let mut renamer = super::heaps::Renamer::new(
                &folds.data,
                &self.global_layout,
                program,
                self.function.source,
            );
            folded.fold(ctx, facts, &mut renamer)?;
            other = &folded;
        }
        let layout = self.global_layout.clone();
        // Freeze precision at the first backedge, including existing values and declared contracts.
        // Later recursive growth becomes gradual beyond that depth; script limits are unchanged.
        let depth = if backedge {
            if self.widening.is_none() {
                self.integer_thresholds = facts.integer_thresholds(ctx, program)?;
            }
            Some(*self.widening.get_or_insert_with(|| facts.max_depth()))
        } else {
            None
        };
        let thresholds = &self.integer_thresholds.data;
        let global_base = self.global_base;
        changed |= self
            .locals
            .merge_indexed(ctx, &other.locals, |ctx, slot, a, b| {
                // Heaps of different lengths or with summaries join entry by entry and keep
                // object positions.
                if slot >= global_base && super::heaps::is_heap(ctx, &layout, slot - global_base)? {
                    if let Some(value) = super::heaps::join(ctx, facts, a.value, b.value, depth)? {
                        return Ok(Binding {
                            value,
                            missing: a.missing || b.missing,
                            owner: a.owner.join(a.value, b.owner, b.value),
                        });
                    }
                }
                let numeric = if backedge {
                    facts.widen_integer_thresholds(ctx, a.value, b.value, thresholds)?
                } else {
                    None
                };
                let value = match numeric {
                    Some(value) => value,
                    None if backedge => facts.joined(ctx, a.value, b.value, depth)?,
                    None => {
                        let value = facts.joined(ctx, a.value, b.value, depth)?;
                        facts.bound_literals(ctx, value)?
                    }
                };
                Ok(Binding {
                    value,
                    missing: a.missing || b.missing,
                    owner: a.owner.join(a.value, b.owner, b.value),
                })
            })?;
        changed |= self
            .global_pending
            .join(ctx, facts, &other.global_pending, depth)?;
        changed |=
            self.global_written
                .merge(ctx, &other.global_written, |_, _, a, b| Ok(a || b))?;
        if let (Some(a), Some(b)) = (&mut self.captures, &other.captures) {
            changed |= a.join(ctx, facts, b, depth)?;
        }
        assert_eq!(self.capture_locals, other.capture_locals);
        assert_eq!(self.stack.data.len(), other.stack.data.len());
        assert_eq!(self.loops.data.len(), other.loops.data.len());
        assert_eq!(self.arguments.data.len(), other.arguments.data.len());
        assert_eq!(self.addresses.data.len(), other.addresses.data.len());
        assert_eq!(self.texts.data.len(), other.texts.data.len());
        for (a, b) in self.texts.data.iter_mut().zip(&other.texts.data) {
            ctx.charge(1)?;
            let next = facts.joined(ctx, *a, *b, depth)?;
            changed |= *a != next;
            *a = next;
        }
        for (a, b) in self.attempts.data.iter_mut().zip(&other.attempts.data) {
            ctx.charge(1)?;
            let error = a.error | b.error;
            changed |= error != a.error;
            a.error = error;
            if let (Some(a), Some(b)) = (&mut a.pending, b.pending) {
                changed |= a.join(ctx, facts, b, depth)?;
            }
        }
        for (a, b) in self.raises.data.iter_mut().zip(&other.raises.data) {
            ctx.charge(1)?;
            changed |= *a != *a | *b;
            *a |= *b;
        }
        for (a, b) in self.addresses.data.iter_mut().zip(&other.addresses.data) {
            ctx.charge(1)?;
            changed |= a.join(ctx, facts, b, depth)?;
        }
        for (a, b) in self.arguments.data.iter_mut().zip(&other.arguments.data) {
            ctx.charge(1)?;
            if a.target != b.target {
                changed |= a.target != Target::Unsupported;
                a.target = Target::Unsupported;
            }
            match (&mut a.receiver, b.receiver) {
                (Some(a), Some(b)) => {
                    let value = facts.joined(ctx, *a, b, depth)?;
                    changed |= *a != value;
                    *a = value;
                }
                (None, None) => (),
                _ => unreachable!("forwarded receiver presence is fixed by bytecode"),
            }
            changed |= a.arguments.join(ctx, facts, &b.arguments)?;
        }
        for (a, b) in self.stack.data.iter_mut().zip(&other.stack.data) {
            ctx.charge(1)?;
            let next = a.join(ctx, facts, *b, depth)?;
            changed |= *a != next;
            *a = next;
        }
        for (a, b) in self.loops.data.iter_mut().zip(&other.loops.data) {
            ctx.charge(1)?;
            assert!(
                a.base == b.base
                    && a.argument_base == b.argument_base
                    && a.address_base == b.address_base
                    && a.attempt_base == b.attempt_base
                    && a.raise_base == b.raise_base
                    && a.text_base == b.text_base
                    && a.expression == b.expression
            );
            let last = facts.joined(ctx, a.last, b.last, depth)?;
            let result = facts.joined(ctx, a.result, b.result, depth)?;
            let source = facts.joined(ctx, a.source, b.source, depth)?;
            let repeat = facts.joined(ctx, a.repeat, b.repeat, depth)?;
            changed |=
                a.last != last || a.result != result || a.source != source || a.repeat != repeat;
            a.last = last;
            a.result = result;
            a.source = source;
            a.repeat = repeat;
        }
        Ok((changed, folds))
    }

    fn store(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
    ) -> Result<()> {
        let before = self.locals.get(ctx, slot)?;
        let own = blocks::Owner::Function(self.function);
        let owner = if !before.missing {
            before.owner
        } else if before.value == Atom::Never.fact() {
            own
        } else {
            before.owner.join(before.value, own, value)
        };
        if self.capture_locals && slot < self.global_base {
            self.captures
                .as_mut()
                .unwrap()
                .store(ctx, facts, slot, value)?;
        }
        if slot >= self.global_base {
            self.global_written
                .set(ctx, slot - self.global_base, true)?;
        }
        self.locals.set(
            ctx,
            slot,
            Binding {
                value,
                missing: false,
                owner,
            },
        )?;
        // An older operand can survive an assignment in its right-hand expression.
        for operand in &mut self.stack.data {
            ctx.charge(1)?;
            operand.invalidate(slot);
        }
        for attempt in &mut self.attempts.data {
            ctx.charge(1)?;
            if let Some(Transfer::Value(operand)) = &mut attempt.pending {
                operand.invalidate(slot);
            }
        }
        Ok(())
    }

    fn refine(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
    ) -> Result<()> {
        let binding = self.locals.get(ctx, slot)?;
        self.locals.set(ctx, slot, Binding { value, ..binding })?;
        for operand in &mut self.stack.data {
            ctx.charge(1)?;
            if operand.origin == Some(slot) {
                operand.value = value;
            }
        }
        for attempt in &mut self.attempts.data {
            ctx.charge(1)?;
            if let Some(Transfer::Value(operand)) = &mut attempt.pending {
                if operand.origin == Some(slot) {
                    operand.value = value;
                }
            }
        }
        if self.capture_locals && slot < self.global_base {
            self.captures
                .as_mut()
                .unwrap()
                .refine(ctx, facts, slot, value)?;
        }
        self.refresh_globals(ctx, facts, slot, value, &Change::Refine)?;
        for address in &mut self.addresses.data {
            ctx.charge(1)?;
            if address.root == Some(slot) {
                address.refresh(ctx, facts, value, &Change::Refine)?;
            }
        }
        Ok(())
    }

    fn narrow(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        operand: Operand,
        test: Test,
        yes: bool,
    ) -> Result<bool> {
        if facts.filter(ctx, operand.value, test, yes)? == Atom::Never.fact() {
            return Ok(false);
        }
        let predicate = if test == Test::Nil {
            operand.origin.map(|slot| Predicate { slot, test, yes })
        } else {
            operand.predicate().map(|p| Predicate {
                yes: p.yes == yes,
                ..p
            })
        };
        if let Some(predicate) = predicate {
            let binding = self.locals.get(ctx, predicate.slot)?;
            // Optional lookup can resolve a different binding when this one is absent.
            if !binding.missing {
                let value = facts.filter(ctx, binding.value, predicate.test, predicate.yes)?;
                if value == Atom::Never.fact() {
                    return Ok(false);
                }
                self.locals.set(
                    ctx,
                    predicate.slot,
                    Binding {
                        value,
                        missing: false,
                        ..binding
                    },
                )?;
                for operand in &mut self.stack.data {
                    ctx.charge(1)?;
                    if operand.origin == Some(predicate.slot) {
                        operand.value =
                            facts.filter(ctx, operand.value, predicate.test, predicate.yes)?;
                    }
                }
                for address in &mut self.addresses.data {
                    ctx.charge(1)?;
                    if address.origin() == Some(predicate.slot) {
                        address.value =
                            facts.filter(ctx, address.value, predicate.test, predicate.yes)?;
                    }
                }
            }
        }
        Ok(true)
    }
}

type Edges = [Option<(usize, State)>; 2];

#[cfg(test)]
pub(super) fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &Program,
    function: usize,
    contracts: &[Fact],
) -> Result<Report> {
    let inputs = super::arguments::general_inputs(
        ctx,
        facts,
        &program.functions[function].params,
        contracts,
    )?;
    analyze_body(
        ctx,
        facts,
        Body {
            scope: blocks::Scope::Invocation,
            ambient: None,
            general: false,
            receiver: None,
            constructor: false,
            program,
            function,
            contracts,
            inputs: &inputs.data,
            current_error: NO_ERROR,
            block: None,
            incoming: None,
            layouts: None,
            globals: None,
        },
        &mut super::calls::Unavailable,
    )
}

pub(super) struct Body<'a> {
    pub scope: blocks::Scope,
    pub ambient: Option<usize>,
    pub general: bool,
    pub receiver: Option<Fact>,
    pub constructor: bool,
    pub program: &'a Program,
    pub function: usize,
    pub contracts: &'a [Fact],
    pub inputs: &'a [Input],
    pub current_error: u16,
    pub block: Option<&'a blocks::Inputs<'a>>,
    pub incoming: Option<&'a blocks::Closure>,
    pub layouts: Option<&'a Layouts>,
    pub globals: Option<&'a Globals>,
}

pub(super) fn analyze_body(
    ctx: &mut CallContext,
    facts: &mut Facts,
    body: Body<'_>,
    calls: &mut dyn Calls,
) -> Result<Report> {
    let Body {
        scope,
        ambient,
        general,
        receiver,
        constructor,
        program,
        function,
        contracts,
        inputs,
        current_error,
        block,
        incoming,
        layouts,
        globals,
    } = body;
    let function_index = function;
    let function = &program.functions[function];
    let mut report = Report {
        returns: Atom::Never.fact(),
        normal_returns: Atom::Never.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        block_exits: Buffer::empty(),
    };
    ctx.checkpoint()?;
    ctx.charge(function.params.len() as u64)?;
    if (function.instance
        && !general
        && !receiver
            .is_some_and(|value| matches!(facts.node(value), super::facts::Node::Instance { .. })))
        || (block.is_none() && (function.name == "<block>" || !function.captures.is_empty()))
    {
        report.incomplete.push(ctx, 0)?;
        return Ok(report);
    }
    assert_eq!(inputs.len(), function.params.len());
    let roots = calls.roots(ctx, facts)?;
    for (slot, name) in function.local_names.iter().enumerate() {
        ctx.charge(function.params.len() as u64 + 1)?;
        if !function.params.iter().any(|param| param.slot == slot)
            && calls.global(ctx, name)?
            && roots::find(ctx, &roots.data, name)?.is_none()
        {
            report.incomplete.push(ctx, 0)?;
            return Ok(report);
        }
    }
    let graph = if program.file {
        Graph::file(ctx, &function.code)?
    } else {
        Graph::new(ctx, &function.code)?
    };
    let owned_layouts;
    let layouts = if let Some(layouts) = layouts {
        layouts
    } else {
        owned_layouts = Layouts::new(ctx, program, 0)?;
        &owned_layouts
    };
    let lexical = layouts.locals(ctx, program, function_index)?;
    let source = facts.source_id(ctx, layouts.source_owner)?;
    let locals = ambient::local_count(ctx, layouts, program, function_index, ambient)?;
    let owned_globals;
    let globals = if let Some(globals) = globals {
        globals
    } else {
        let mut storage = super::globals::layout::Storage::new();
        let layout = storage.prepare(
            ctx,
            facts,
            super::globals::layout::Definition {
                source,
                owner: layouts.source_owner,
                program,
                files: &layouts.files,
                roots: &roots.data,
                receiving: None,
            },
        )?;
        owned_globals = Globals::initial(ctx, &layout)?;
        &owned_globals
    };
    let mut initial = State::new(
        ctx,
        locals,
        source.callable(function_index),
        &globals.layout,
    )?;
    initial.global_pending = globals.pending.snapshot(ctx)?;
    let bindings = globals.bindings.snapshot(ctx)?;
    initial.locals.replace_globals(bindings);
    if let Some(incoming) = incoming.filter(|_| block.is_none()) {
        let mut captures = Buffer::empty();
        for link in &incoming.captures.data {
            ctx.charge(1)?;
            captures.push(
                ctx,
                blocks::Capture {
                    slot: link.slot,
                    value: link.value,
                    missing: link.missing,
                    owner: link.owner,
                },
            )?;
        }
        let extent = incoming.extent(ctx)?;
        let mut values = blocks::Captures::new(ctx, extent, &captures.data)?;
        values.pending = incoming.pending.snapshot(ctx)?;
        initial.captures = Some(values);
    }
    if let Some(block) = block {
        assert!(function.name == "<block>" || function.initializer);
        for capture in block.captures {
            ctx.charge(1)?;
            if capture.slot >= locals {
                continue;
            }
            assert!(
                capture.slot >= lexical
                    || layouts
                        .capture(ctx, program, function_index, capture.slot)?
                        .is_some()
            );
            initial.locals.set(
                ctx,
                capture.slot,
                Binding {
                    value: capture.value,
                    missing: capture.missing,
                    owner: capture.owner,
                },
            )?;
        }
        let extent = blocks::extent(ctx, locals, block.inherited)?;
        let mut values = blocks::Captures::new(ctx, extent, block.captures)?;
        values.pending = block.pending.snapshot(ctx)?;
        initial.captures = Some(values);
        initial.capture_locals = true;
    }
    if initial.captures.is_none() && initial.global_count > 0 {
        initial.captures = Some(blocks::Captures::new(ctx, 0, &[])?);
    }
    let receiver = if general && function.instance {
        let fields = if constructor {
            None
        } else {
            calls.receiver_fields(ctx, facts, source, function.namespace.unwrap(), globals)?
        };
        if fields == Some(Atom::Never.fact()) {
            return Ok(report);
        }
        let kind = if constructor {
            super::facts::InstanceKind::Concrete
        } else {
            super::facts::InstanceKind::Symbolic
        };
        let module = function.namespace.unwrap();
        let fields = general::Model {
            initial: fields,
            program,
            layouts,
            contracts,
        }
        .fields(ctx, facts, module, kind)?;
        let class = super::namespaces::value(ctx, facts, program, layouts.source_owner, module)?;
        let super::facts::Node::TypeValue(class) = *facts.node(class) else {
            unreachable!()
        };
        let root = initial.global_base + initial.source_slots.namespace(module) + 2;
        let value = general::allocate(ctx, facts, &mut initial, root, class, fields, kind)?;
        if value.is_none() {
            report.incomplete.push(ctx, 0)?;
            return Ok(report);
        }
        value
    } else {
        receiver
    };
    if !function.binds_parameters {
        for (index, parameter) in function.params.iter().enumerate() {
            ctx.charge(1)?;
            let Input::Supplied(value) = inputs[index] else {
                unreachable!()
            };
            initial.store(ctx, facts, parameter.slot, value)?;
        }
    }
    let mut entries = Buffer::with_capacity(ctx, graph.blocks.data.len())?;
    let mut queue = super::graph::Worklist::new(&graph);
    for _ in &graph.blocks.data {
        ctx.charge(1)?;
        entries.data.push(Buffer::empty());
    }
    entries.data[0].push(
        ctx,
        handlers::Entry {
            state: initial,
            polarity: 0,
            queued: true,
        },
    )?;
    queue.push(ctx, (0, 0))?;
    let mut walker = Walker {
        source,
        scope,
        ambient,
        general,
        receiver,
        constructor,
        ctx,
        facts,
        program,
        function,
        function_index,
        layouts,
        contracts,
        inputs,
        current_error,
        block_inputs: block.filter(|_| !function.initializer),
        incoming,
        calls,
        roots: &roots.data,
        report: None,
        exits: blocks::ExitIndex::new(),
        extra: Buffer::empty(),
        native_results: None,
        native_frame: None,
        resume: None,
    };
    walker.calls.track_created(walker.ctx, true)?;
    while let Some((index, polarity)) = queue.pop(walker.ctx)? {
        walker.ctx.charge(1)?;
        entries.data[index].data[polarity].queued = false;
        let edges = loop {
            let state = entries.data[index].data[polarity]
                .state
                .snapshot(walker.ctx)?;
            let edges = walker.block(&graph.blocks.data[index], state)?;
            if !walker.calls.unsettled() {
                walker.resume = None;
                break edges;
            }
            // Calls to contexts created by this walk read provisional summaries. Discard its
            // states, summarize those contexts and walk the block again.
            drop(edges);
            walker.extra.data.clear();
            walker.calls.settle(walker.ctx, walker.facts)?;
        };
        for (pc, state) in edges.into_iter().flatten() {
            walker.extra.push(walker.ctx, (pc, state))?;
        }
        while let Some((pc, state)) = walker.extra.data.pop() {
            let backedge = pc <= graph.blocks.data[index].start;
            let index = graph.at(walker.ctx, pc)?;
            let polarity = state.polarity(walker.ctx, walker.facts)?;
            let mut selected = None;
            for (i, entry) in entries.data[index].data.iter().enumerate() {
                walker.ctx.charge(1)?;
                if entry.polarity == polarity && entry.state.compatible(walker.ctx, &state)? {
                    selected = Some(i);
                    break;
                }
            }
            let (selected, changed) = if let Some(selected) = selected {
                let changed = entries.data[index].data[selected].state.join(
                    walker.ctx,
                    walker.facts,
                    &state,
                    backedge,
                    program,
                )?;
                (selected, changed)
            } else {
                let selected = entries.data[index].data.len();
                entries.data[index].push(
                    walker.ctx,
                    handlers::Entry {
                        state,
                        polarity,
                        queued: false,
                    },
                )?;
                (selected, true)
            };
            if changed && !entries.data[index].data[selected].queued {
                queue.push(walker.ctx, (index, selected))?;
                entries.data[index].data[selected].queued = true;
            }
        }
    }
    walker.calls.track_created(walker.ctx, false)?;
    // Diagnostics use converged inputs, never provisional branch or loop states.
    walker.report = Some(&mut report);
    for (index, entry) in entries.data.into_iter().enumerate() {
        walker.ctx.charge(1)?;
        for entry in entry.data {
            walker.block(&graph.blocks.data[index], entry.state)?;
            walker.extra.data.clear();
        }
    }
    Ok(report)
}

struct Walker<'a> {
    source: SourceId,
    scope: blocks::Scope,
    ambient: Option<usize>,
    general: bool,
    receiver: Option<Fact>,
    constructor: bool,
    ctx: &'a mut CallContext,
    facts: &'a mut Facts,
    program: &'a Program,
    function: &'a Function,
    function_index: usize,
    layouts: &'a Layouts,
    contracts: &'a [Fact],
    inputs: &'a [Input],
    current_error: u16,
    block_inputs: Option<&'a blocks::Inputs<'a>>,
    incoming: Option<&'a blocks::Closure>,
    calls: &'a mut dyn Calls,
    roots: &'a [Root],
    report: Option<&'a mut Report>,
    exits: blocks::ExitIndex,
    extra: Buffer<(usize, State)>,
    native_results: Option<Buffer<State>>,
    native_frame: Option<native::NativeFrame>,
    resume: Option<collection_blocks::Resume>,
}

impl Walker<'_> {
    /// Reports a range endpoint that cannot convert and returns its known bound.
    /// Finite floats truncate toward zero; other known floats cannot convert.
    fn range_endpoint(&mut self, state: &State, pc: usize, value: Fact) -> Result<Option<i64>> {
        let numeric = self
            .facts
            .union(self.ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
        let mut rejected = self.facts.relation(self.ctx, value, numeric)? == Relation::Rejected;
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            if let super::facts::Node::Float(bits) = self.facts.node(self.facts.arm(value, i)) {
                rejected |= crate::range::truncate(f64::from_bits(*bits)).is_none();
            }
        }
        if rejected {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            self.issue(pc, IssueKind::Range { value })?;
        }
        Ok(match self.facts.node(value) {
            super::facts::Node::Integer(n) => Some(*n),
            super::facts::Node::Float(bits) => crate::range::truncate(f64::from_bits(*bits)),
            _ => None,
        })
    }
    fn case_compare(
        &mut self,
        state: &mut State,
        pc: usize,
        target: Option<Operand>,
        matcher: Operand,
        splat: bool,
    ) -> Result<Option<Edges>> {
        let result =
            self.facts
                .case_result(self.ctx, target.map(|t| t.value), matcher.value, splat)?;
        if result.rejected {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            self.issue(
                pc,
                IssueKind::CaseSplat {
                    value: matcher.value,
                },
            )?;
        }
        if result.value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        let predicate = if let Some(target) = target {
            target.origin.map(|slot| Predicate {
                slot,
                test: Test::Case {
                    matcher: matcher.value,
                    splat,
                },
                yes: true,
            })
        } else if !splat {
            matcher.predicate()
        } else {
            None
        };
        state.stack.push(
            self.ctx,
            Operand {
                predicate,
                ..Operand::new(result.value)
            },
        )?;
        Ok(None)
    }
    fn invoke(
        &mut self,
        state: &mut State,
        pc: usize,
        mut target: Target,
        mut args: Arguments,
    ) -> Result<Option<Edges>> {
        if args.uncertain() {
            match target {
                // Script and host bindings account for every argument shape.
                Target::Function(_)
                | Target::Method { .. }
                | Target::Block(_)
                | Target::Host(_)
                | Target::NonCallable
                | Target::Unbound { .. }
                | Target::Undefined
                | Target::Unsupported
                | Target::Value(_)
                | Target::Deferred(_) => (),
                // A module path that a splat spells is not a static name.
                Target::Builtin(crate::builtin::Builtin::Require) => {
                    return self.incomplete(pc).map(Some);
                }
                // Native arities vary by member, so an uncertain shape is a gradual call.
                Target::Builtin(_)
                | Target::Offset(_)
                | Target::Helper { .. }
                | Target::Dynamic => {
                    return self.dynamic_call(state, pc, args);
                }
            }
        }
        if target == Target::Dynamic {
            return self.dynamic_call(state, pc, args);
        }
        if target == Target::Builtin(crate::builtin::Builtin::Require) {
            self.require_file(state, pc, args)?;
            return Ok(Some([None, None]));
        }
        if let Target::Builtin(
            builtin @ (crate::builtin::Builtin::Output(_) | crate::builtin::Builtin::Format(_)),
        ) = target
        {
            return self.render_call(state, pc, builtin, args);
        }
        if let Target::Helper {
            receiver,
            name,
            implicit,
        } = target
        {
            if crate::members::introspection::supported(name)
                || matches!(name, "send" | "public_send" | "tap" | "yield_self")
            {
                return self.namespace_helper(state, pc, receiver, name, implicit, args);
            }
        }
        if let Target::Method {
            function,
            receiver,
            constructor: true,
        } = target
        {
            let Some(module) = self.namespace(state, receiver)? else {
                return self.incomplete(pc).map(Some);
            };
            let Some(instance) = self.construct(state, pc, receiver)? else {
                return Ok(Some([None, None]));
            };
            if !module.program().namespaces[module.index]
                .constructor
                .unwrap()
                .1
            {
                args = Arguments::new();
            }
            target = Target::Method {
                function,
                receiver: instance,
                constructor: true,
            };
        }
        if let Target::Host(index) = target {
            if args.block.is_some() {
                self.host_block(state, pc, index, args)?;
                return Ok(Some([None, None]));
            }
        }
        if target == Target::Builtin(crate::builtin::Builtin::Loop) {
            self.native_loop(state, pc, args)?;
            return Ok(Some([None, None]));
        }
        if args.block.is_some() {
            use crate::builtin::Builtin;
            match target {
                Target::Builtin(
                    Builtin::Assert
                    | Builtin::Time(_)
                    | Builtin::Now
                    | Builtin::DurationBuild
                    | Builtin::DurationParse
                    | Builtin::Money
                    | Builtin::MoneyCents,
                ) => args.block = None,
                Target::Builtin(Builtin::Output(_) | Builtin::Format(_) | Builtin::Require) => {
                    return self.incomplete(pc).map(Some);
                }
                Target::Builtin(_) | Target::Offset(_) => {
                    self.issue(
                        pc,
                        IssueKind::Call {
                            target,
                            failure: Failure::BuiltinBlock,
                        },
                    )?;
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    return Ok(Some([None, None]));
                }
                _ => (),
            }
        }
        let current_error = state.current_error(self.ctx, self.current_error)?;
        if let Some(block) = &mut args.block {
            self.prepare_callback(state, block)?;
        }
        let attached = args
            .block
            .as_ref()
            .map(|b| b.snapshot(self.ctx))
            .transpose()?;
        let result = match target {
            Target::Builtin(builtin) if attached.is_none() => {
                builtins::invoke(self.ctx, self.facts, builtin, &args)?
            }
            Target::Offset(value) if attached.is_none() => {
                builtins::protected::invoke(self.ctx, self.facts, value, &args)?
            }
            _ => {
                let mut args = Some(args);
                loop {
                    let input = if matches!(target, Target::Host(_)) {
                        args.as_ref().unwrap().snapshot(self.ctx)?
                    } else {
                        args.take().unwrap()
                    };
                    let globals = state.global_call(self.ctx)?;
                    let result = self.calls.invoke(
                        self.ctx,
                        self.facts,
                        target,
                        input,
                        current_error,
                        &globals,
                    )?;
                    let Some(root) = result.pending else {
                        break result;
                    };
                    self.call_effects(state, pc, target, &result)?;
                    let slot = state.global_base + state.source_slots.roots.data[root];
                    let Some(alternatives) = self.import_root_branches(state, pc, slot)? else {
                        return Ok(Some([None, None]));
                    };
                    for mut next in alternatives.data {
                        let input = args.as_ref().unwrap().snapshot(self.ctx)?;
                        let edges = self.invoke(&mut next, pc, target, input)?;
                        self.member_edges(pc, next, edges)?;
                    }
                }
            }
        };
        self.fold_state(state, &result.folds)?;
        self.call_effects(state, pc, target, &result)?;
        if result.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if let Some(attached) = attached {
            self.call_exits(state, pc, &attached, result.exits)?;
            return Ok(Some([None, None]));
        }
        if !result.exits.data.is_empty() {
            return Ok(if self.global_exits(state, pc, result.exits)? {
                None
            } else {
                Some([None, None])
            });
        }
        if result.value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        state.stack.push(self.ctx, Operand::new(result.value))?;
        Ok(None)
    }

    /// Renames the caller's references to objects a recursive summary folded.
    fn fold_state(&mut self, state: &mut State, folds: &Buffer<super::heaps::Fold>) -> Result<()> {
        if folds.data.is_empty() {
            return Ok(());
        }
        let layout = state.global_layout.clone();
        let mut renamer =
            super::heaps::Renamer::new(&folds.data, &layout, self.program, self.source);
        state.fold(self.ctx, self.facts, &mut renamer)
    }

    fn call_effects(
        &mut self,
        state: &State,
        pc: usize,
        target: Target,
        result: &super::calls::Outcome,
    ) -> Result<()> {
        let mut classes = result.throws;
        for &failure in &result.failures.data {
            self.ctx.charge(1)?;
            classes |= handlers::bit(match failure {
                Failure::Require { class, .. } => class,
                Failure::Type { .. }
                | Failure::DetachedValue(_)
                | Failure::NonCallable
                | Failure::Unbound
                | Failure::Undefined
                | Failure::HostArity
                | Failure::HostKeywords
                | Failure::HostBlock
                | Failure::HostGrant
                | Failure::HostResult { .. }
                | Failure::HostTypeBinding { .. }
                | Failure::BuiltinArity
                | Failure::BuiltinBlock
                | Failure::BuiltinKeywords
                | Failure::BuiltinKeyword(_)
                | Failure::BuiltinKeywordType { .. }
                | Failure::BuiltinValue
                | Failure::TypeLiteral(_)
                | Failure::BuiltinDomain(_)
                | Failure::JsonValue(_) => ErrorClass::Runtime,
                _ => ErrorClass::Argument,
            });
            self.issue(pc, IssueKind::Call { target, failure })?;
        }
        self.emit_error(state, pc, classes)
    }

    #[allow(clippy::too_many_arguments)]
    fn target(
        &mut self,
        state: &State,
        pc: usize,
        slot: usize,
        name: usize,
        named: bool,
        ambient: bool,
        file: bool,
    ) -> Result<Option<Target>> {
        if slot != usize::MAX {
            let binding = state.locals.get(self.ctx, slot)?;
            if binding.value != Atom::Never.fact() {
                if binding.missing {
                    return Ok(Some(Target::Unsupported));
                }
                return self.value_target(binding.value).map(Some);
            }
        }
        let index = name;
        let name = &self.program.members[name];
        if let Some(field) = self.namespace_constant(state, name, named)? {
            return if field.missing || field.incomplete {
                Ok(Some(Target::Unsupported))
            } else {
                self.value_target(field.value).map(Some)
            };
        }
        if let Some((_, binding)) = self.ambient_binding(state, name)?.filter(|_| ambient) {
            return if binding.missing {
                Ok(Some(Target::Unsupported))
            } else {
                self.value_target(binding.value).map(Some)
            };
        }
        let target = if !file {
            self.root_target_past_file(state, name)?
        } else if let Some(target) = self.file_target(state, name)? {
            target
        } else {
            self.root_target(state, name)?
        };
        if target == Target::Undefined && self.unknown_exports(state)? {
            return Ok(Some(Target::Dynamic));
        }
        if target == Target::Undefined && self.function.namespace.is_some() {
            return self.implicit_namespace_target(state, pc, index);
        }
        Ok(Some(target))
    }

    fn value_target(&mut self, value: Fact) -> Result<Target> {
        if let super::facts::Node::Callable { owner, target } = *self.facts.node(value) {
            return self.calls.attached(self.ctx, owner, target);
        }
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            if matches!(
                self.facts.node(self.facts.arm(value, i)),
                super::facts::Node::Offset(_)
            ) {
                return Ok(Target::Offset(value));
            }
        }
        if let super::facts::Node::Builtin(builtin) = self.facts.node(value) {
            return Ok(Target::Builtin(*builtin));
        }
        if self.facts.known_non_callable(self.ctx, value)? {
            Ok(Target::NonCallable)
        } else if matches!(
            self.facts.node(value),
            super::facts::Node::Atom(Atom::Any | Atom::Unknown)
        ) {
            Ok(Target::Dynamic)
        } else {
            Ok(Target::Unsupported)
        }
    }
    fn issue(&mut self, pc: usize, kind: IssueKind) -> Result<()> {
        if let Some(report) = self.report.as_mut() {
            report.issues.push(self.ctx, Issue { pc, kind })?;
        }
        Ok(())
    }

    fn incomplete(&mut self, pc: usize) -> Result<Edges> {
        if let Some(report) = self.report.as_mut() {
            report.incomplete.push(self.ctx, pc)?;
        }
        Ok([None, None])
    }

    /// Emits the runtime error edge of a write to a possibly protected receiver.
    ///
    /// The edge exists only because the receiver may be a protected object, so
    /// when the receiver is an unsplit local its rescue state sees the admitted
    /// protected alternatives rather than the whole contract domain. Ordinary
    /// write failures emit their own edges with the unnarrowed state.
    fn protected_error(&mut self, state: &State, pc: usize, address: &Address) -> Result<()> {
        let classes = handlers::bit(ErrorClass::Runtime);
        let Some(slot) = address.root else {
            return self.emit_error(state, pc, classes);
        };
        let Some(value) = address.protected_root(self.ctx, self.facts)? else {
            return self.emit_error(state, pc, classes);
        };
        let current = state.locals.get(self.ctx, slot)?.value;
        if value == current {
            return self.emit_error(state, pc, classes);
        }
        let mut narrowed = state.snapshot(self.ctx)?;
        narrowed.refine(self.ctx, self.facts, slot, value)?;
        self.emit_error(&narrowed, pc, classes)
    }

    fn branch(
        &mut self,
        state: State,
        operand: Operand,
        test: Test,
        yes: bool,
        target: usize,
        next: usize,
    ) -> Result<Edges> {
        let mut taken = state.snapshot(self.ctx)?;
        let mut other = state;
        let taken = taken
            .narrow(self.ctx, self.facts, operand, test, yes)?
            .then_some((target, taken));
        let other = other
            .narrow(self.ctx, self.facts, operand, test, !yes)?
            .then_some((next, other));
        Ok([taken, other])
    }

    fn store(&mut self, state: &mut State, pc: usize, slot: usize, operand: Operand) -> Result<()> {
        let value = operand.value;
        let before = state.locals.get(self.ctx, slot)?.value;
        if slot < state.global_base && self.facts.reassignment_conflicts(self.ctx, before, value)? {
            self.issue(
                pc,
                IssueKind::Reassignment {
                    slot,
                    before,
                    after: value,
                },
            )?;
        }
        state.store(self.ctx, self.facts, slot, value)?;
        let change = Change::Store {
            same: operand.origin == Some(slot),
            fresh: operand.fresh,
        };
        if state.capture_locals && slot < state.global_base {
            state
                .captures
                .as_mut()
                .unwrap()
                .refresh(self.ctx, self.facts, slot, value, &change)?;
        }
        state.refresh_globals(self.ctx, self.facts, slot, value, &change)?;
        for address in &mut state.addresses.data {
            self.ctx.charge(1)?;
            if address.root == Some(slot) {
                address.refresh(self.ctx, self.facts, value, &change)?;
            }
        }
        Ok(())
    }

    fn mutate(
        &mut self,
        state: &mut State,
        pc: usize,
        site: impl Into<MemberSite>,
        args: &[Fact],
        address_result: bool,
        fresh: bool,
    ) -> Result<Option<Edges>> {
        let site = site.into();
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        let receiver = state.addresses.data.last().unwrap().value;
        let mut attached = false;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            attached |= matches!(
                self.facts.node(self.facts.arm(receiver, i)),
                super::facts::Node::Callable { .. }
            );
        }
        if attached {
            if self.facts.arm_count(receiver) == 1 {
                self.export_value(state, pc, receiver, false)?;
                return Ok(Some([None, None]));
            }
            for i in 0..self.facts.arm_count(receiver) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(receiver, i);
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = arm;
                let edges = self.mutate(&mut next, pc, site, args, address_result, fresh)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if let Some(variants) = self.member_variants(receiver, name, site.scope)? {
            for receiver in variants.data {
                self.ctx.charge(1)?;
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = receiver;
                let edges = self.mutate(&mut next, pc, site, args, address_result, fresh)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if matches!(self.facts.atom(receiver), Some(Atom::Unknown | Atom::Any)) {
            let mut arguments = Arguments::new();
            arguments.positional.extend(self.ctx, args)?;
            return self.dynamic_mutation(state, pc, site, arguments, address_result);
        }
        if self.namespace_receiver(receiver)? {
            let mut arguments = Arguments::new();
            arguments.positional.extend(self.ctx, args)?;
            return self.namespace_address_call(state, pc, site, arguments, address_result);
        }
        if matches!(name, "delete_if" | "keep_if") {
            let mut arguments = Arguments::new();
            arguments.positional.extend(self.ctx, args)?;
            self.mutable_block(state, pc, site, &arguments)?;
            return Ok(Some([None, None]));
        }
        match super::objects::select(self.ctx, self.facts, receiver, site.call, name)? {
            Some(super::objects::Selection::Field(_) | super::objects::Selection::Missing) => {
                state.addresses.data.pop().unwrap();
                let mut arguments = Arguments::new();
                arguments.positional.extend(self.ctx, args)?;
                let edges = self.member(state, pc, receiver, site, arguments)?;
                if edges.is_none() && address_result {
                    let value = state.stack.data.pop().unwrap().value;
                    state.addresses.push(self.ctx, Address::new(None, value))?;
                }
                return Ok(edges);
            }
            Some(super::objects::Selection::Uncertain(field)) => {
                let mut present = state.snapshot(self.ctx)?;
                present.addresses.data.pop().unwrap();
                let mut arguments = Arguments::new();
                arguments.positional.extend(self.ctx, args)?;
                let edges = self.member_field(&mut present, pc, field, site, arguments, false)?;
                if edges.is_none() && address_result {
                    let value = present.stack.data.pop().unwrap().value;
                    present
                        .addresses
                        .push(self.ctx, Address::new(None, value))?;
                }
                self.member_edges(pc, present, edges)?;
                if !super::objects::absent_is_native(site.call, name) {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    return Ok(Some([None, None]));
                }
            }
            Some(super::objects::Selection::Native) | None => (),
        }
        let address = state.addresses.data.pop().unwrap();
        let protection = address.protection(self.ctx, self.facts)?;
        if protection.readonly != Attached::No {
            self.protected_error(state, pc, &address)?;
            if protection.report {
                let arguments = self.facts.tuple(self.ctx, args)?;
                self.issue(
                    pc,
                    IssueKind::Member {
                        name: site.name,
                        receiver: address.value,
                        arguments,
                    },
                )?;
            }
            if protection.readonly == Attached::Yes {
                return Ok(Some([None, None]));
            }
        }
        if builtins::namespace_call(self.ctx, self.facts, address.value, name)?
            || builtins::value_member(self.ctx, self.facts, address.value, name)?
        {
            let mut arguments = Arguments::new();
            arguments.positional.extend(self.ctx, args)?;
            let edges = self.member(state, pc, address.value, site, arguments)?;
            if edges.is_none() && address_result {
                let value = state.stack.data.pop().unwrap().value;
                state.addresses.push(self.ctx, Address::new(None, value))?;
            }
            return Ok(edges);
        }
        if !address.supported {
            return self.incomplete(pc).map(Some);
        }
        let result = self.facts.collection_mutation_member(
            self.ctx,
            address.value,
            site.call,
            name,
            args,
        )?;
        if result.rejected || result.throws {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        if result.rejected {
            let arguments = self.facts.tuple(self.ctx, args)?;
            self.issue(
                pc,
                IssueKind::Member {
                    name: site.name,
                    receiver: address.value,
                    arguments,
                },
            )?;
        }
        if result.unsupported {
            return self.incomplete(pc).map(Some);
        }
        if result.value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        let mut pure = true;
        let mut returns_receiver = true;
        for i in 0..self.facts.arm_count(address.value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(address.value, i);
            let array = matches!(
                self.facts.node(arm),
                super::facts::Node::Array(_) | super::facts::Node::Tuple(_)
            );
            let hash = matches!(
                self.facts.node(arm),
                super::facts::Node::Hash(..) | super::facts::Node::Shape(..)
            );
            pure &= self.facts.atom(arm) == Some(Atom::String)
                || (hash && !crate::members::hash_builtin(name));
            returns_receiver &= (array
                && matches!(
                    site.method,
                    Some(
                        Method::Push
                            | Method::Prepend
                            | Method::Insert
                            | Method::Clear
                            | Method::Fill
                    )
                ))
                || (hash && matches!(site.method, Some(Method::Clear | Method::Replace)));
        }
        let change = if pure {
            Change::Store {
                same: true,
                fresh: false,
            }
        } else {
            Change::Mutation {
                address: &address,
                method: site.method,
                args,
                fresh,
            }
        };
        let output = if address_result {
            MutationOutput::Address(result.value)
        } else {
            let origin = if returns_receiver {
                address.origin()
            } else {
                None
            };
            MutationOutput::Value(Operand {
                origin,
                ..Operand::new(result.value)
            })
        };
        self.publish_result(state, pc, &address, result.receiver, change, output)
    }

    fn index_outcome(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        args: &[Fact],
        result: &super::scalar::Operation,
    ) -> Result<Option<Edges>> {
        if result.rejected || result.throws {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        if result.rejected {
            let arguments = self.facts.tuple(self.ctx, args)?;
            self.issue(
                pc,
                IssueKind::Index {
                    receiver,
                    arguments,
                },
            )?;
        }
        if result.unsupported {
            return self.incomplete(pc).map(Some);
        }
        Ok((result.value == Atom::Never.fact()).then_some([None, None]))
    }

    fn block(&mut self, block: &Block, state: State) -> Result<Edges> {
        let mut pending = Buffer::empty();
        let edges = self.block_segment(block, state, &mut pending)?;
        while let Some((start, state)) = pending.data.pop() {
            self.ctx.charge(1)?;
            let suffix = Block {
                start,
                end: block.end,
                exit: block.exit,
            };
            for edge in self
                .block_segment(&suffix, state, &mut pending)?
                .into_iter()
                .flatten()
            {
                self.extra.push(self.ctx, edge)?;
            }
        }
        Ok(edges)
    }

    fn block_segment(
        &mut self,
        block: &Block,
        state: State,
        pending: &mut Buffer<(usize, State)>,
    ) -> Result<Edges> {
        let mut index = self.extra.data.len();
        let edges = self.block_instructions(block, state, pending)?;
        while index < self.extra.data.len() {
            self.ctx.charge(1)?;
            let pc = self.extra.data[index].0;
            if pc > block.start && pc < block.end {
                let edge = self.extra.data.swap_remove(index);
                pending.push(self.ctx, edge)?;
            } else {
                index += 1;
            }
        }
        Ok(edges)
    }

    fn block_instructions(
        &mut self,
        block: &Block,
        mut state: State,
        pending: &mut Buffer<(usize, State)>,
    ) -> Result<Edges> {
        for pc in block.start..block.end {
            self.ctx.charge(1)?;
            if let Some(variants) = self.file_variants(&state, self.function.code[pc])? {
                for variant in variants {
                    pending.push(self.ctx, (pc, variant))?;
                }
                return Ok([None, None]);
            }
            if !self.export_stack(&mut state, pc, self.function.code[pc])? {
                return Ok([None, None]);
            }
            let Some(op) = self.ambient_op(&state, self.function.code[pc])? else {
                return self.incomplete(pc);
            };
            let Some(op) = self.file_op(&state, op)? else {
                continue;
            };
            let Some(op) = self.root_op(&state, op)? else {
                return self.incomplete(pc);
            };
            if !matches!(op, Op::ResolveCall(..) | Op::CallName(..)) {
                if let Some(slot) = self.root_read_slot(&state, op, true)? {
                    let Some(alternatives) = self.import_root_branches(&mut state, pc, slot)?
                    else {
                        return Ok([None, None]);
                    };
                    for alternative in alternatives.data {
                        pending.push(self.ctx, (pc, alternative))?;
                    }
                }
            }
            let errors = self.potential_errors(&state, op)?;
            self.emit_error(&state, pc, errors)?;
            match op {
                Op::Integer(..) => state.stack.push(self.ctx, Operand::new(Atom::Int.fact()))?,
                Op::Nil => state.stack.push(self.ctx, Operand::new(Atom::Nil.fact()))?,
                Op::TextStart => {
                    let empty = self.facts.string(self.ctx, b"")?;
                    state.texts.push(self.ctx, empty)?;
                }
                Op::TextPart => return self.text_part(state, pc),
                Op::TextEnd(symbol) => self.text_end(&mut state, symbol)?,
                Op::Constant(index) => {
                    let value = match &self.program.constants[index].0 {
                        Kind::Nil => Atom::Nil.fact(),
                        Kind::Bool(value) => self.facts.boolean(self.ctx, *value)?,
                        Kind::Int(value) => self.facts.integer(self.ctx, *value)?,
                        Kind::Big(_) => Atom::Int.fact(),
                        Kind::Float(value) => self.facts.float(self.ctx, *value)?,
                        Kind::Bytes(value) => self.facts.string(self.ctx, &value.data)?,
                        Kind::Symbol(value) => self.facts.symbol(self.ctx, &value.data)?,
                        Kind::Duration(_) => Atom::Duration.fact(),
                        Kind::Time(_) | Kind::Zoned(_) => Atom::Time.fact(),
                        Kind::Money(_) => Atom::Money.fact(),
                        Kind::Shape(shape) => {
                            let ty =
                                self.facts
                                    .annotation(self.ctx, &shape.definition.ty, |_, _| Ok(None))?;
                            if self.facts.unresolved(ty) {
                                return self.incomplete(pc);
                            }
                            self.facts.type_value(self.ctx, ty)?
                        }
                        _ => return self.incomplete(pc),
                    };
                    self.facts.mark_constant(value);
                    state.stack.push(
                        self.ctx,
                        Operand {
                            literal: Some(index),
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::Declaration(index) => {
                    let value = &self.program.declarations[index];
                    let name = match &value.0 {
                        Kind::Enum(enumeration) => &enumeration.definition.name,
                        Kind::Namespace(namespace) => &namespace.definition.name,
                        _ => return self.incomplete(pc),
                    };
                    if let Some(index) = if self.program.file {
                        None
                    } else {
                        self.root_index(&state, name)?
                    } {
                        if !self.read_global(&mut state, pc, index, Receiving::Value)? {
                            return Ok([None, None]);
                        }
                        continue;
                    }
                    if !self.program.file && self.calls.global(self.ctx, name)? {
                        return self.incomplete(pc);
                    }
                    let value = self.load_declaration(&mut state, pc, index)?;
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::NamespaceSelf(module) => {
                    let value = self.self_value(module)?;
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::BindIvar(name, local) => {
                    if !self.bind_ivar(&mut state, pc, name, local)? {
                        return Ok([None, None]);
                    }
                }
                Op::NamespaceConstant(name, next) => {
                    return self.namespace_constant_edges(state, pc, name, next, false);
                }
                Op::NamespaceConstantAddress(name, next) => {
                    return self.namespace_constant_edges(state, pc, name, next, true);
                }
                Op::NamespaceVariable(name, optional) => {
                    if !self.namespace_variable(&mut state, pc, name, optional)? {
                        return Ok([None, None]);
                    }
                }
                Op::NamespaceStore(name) => {
                    if let Some(edges) = self.namespace_store(&mut state, pc, name)? {
                        return Ok(edges);
                    }
                }
                Op::NamespaceAddress(name, optional) => {
                    if let Some(edges) = self.namespace_address(&mut state, pc, name, optional)? {
                        return Ok(edges);
                    }
                }
                Op::InitNamespace(module) => {
                    if let Some(edges) = self.initialize_namespace(&mut state, pc, module)? {
                        return Ok(edges);
                    }
                }
                Op::ImplicitAddress(name, next) => {
                    return self.implicit_address(state, pc, name, next);
                }
                Op::AmbientValue(name, next) | Op::AmbientAddress(name, next) => {
                    return self.ambient_edges(
                        state,
                        pc,
                        name,
                        next,
                        matches!(op, Op::AmbientAddress(..)),
                    );
                }
                Op::FileValue(name, next, receiving) => {
                    return self.file_edges(state, pc, name, next, false, receiving);
                }
                Op::FileAddress(name, next) => {
                    return self.file_edges(state, pc, name, next, true, Receiving::Value);
                }
                Op::Global(index) | Op::GlobalReceiver(index, _) => {
                    let receiving = match op {
                        Op::GlobalReceiver(_, receiving) => receiving,
                        _ => Receiving::Value,
                    };
                    if self.program.file {
                        let name = self.program.globals[index].0.name();
                        if self.file_binding(&state, name)?.is_none()
                            && !self.calls.global(self.ctx, name)?
                        {
                            if let Some(readable) =
                                self.read_receiving(&mut state, pc, name, receiving)?
                            {
                                if !readable {
                                    return Ok([None, None]);
                                }
                                continue;
                            }
                        }
                    }
                    let Some(index) = self.global_index(&state, index)? else {
                        return self.incomplete(pc);
                    };
                    if !self.read_global(&mut state, pc, index, receiving)? {
                        return Ok([None, None]);
                    }
                }
                Op::StoreDeclaration(index) => {
                    let name = match &self.program.declarations[index].0 {
                        Kind::Enum(enumeration) => &enumeration.definition.name,
                        Kind::Namespace(namespace) => &namespace.definition.name,
                        _ => return self.incomplete(pc),
                    };
                    let slot = if self.program.file {
                        self.file_slot(&state, name)?.unwrap()
                    } else if let Some(index) = self.root_index(&state, name)? {
                        state.global_base + index
                    } else if self.calls.global(self.ctx, name)? {
                        return self.incomplete(pc);
                    } else {
                        self.declaration_slot(&state, index)
                    };
                    let operand = *state.stack.data.last().unwrap();
                    self.store(&mut state, pc, slot, operand)?;
                }
                Op::StoreGlobal(index) => {
                    let slot = if self.program.file {
                        self.file_slot(&state, self.program.globals[index].0.name())?
                            .unwrap()
                    } else {
                        let Some(index) = self.global_index(&state, index)? else {
                            return self.incomplete(pc);
                        };
                        state.global_base + index
                    };
                    let operand = *state.stack.data.last().unwrap();
                    self.store(&mut state, pc, slot, operand)?;
                }
                Op::ResolveGlobalCall(index) => {
                    let name = self.program.globals[index].0.name();
                    let target = if let Some(index) = self.global_index(&state, index)? {
                        Target::Value(state.locals.get(self.ctx, state.global_base + index)?.value)
                    } else {
                        self.calls.resolve(self.ctx, name)?
                    };
                    state.arguments.push(
                        self.ctx,
                        Pending {
                            target,
                            receiver: None,
                            arguments: Arguments::new(),
                        },
                    )?;
                    if let Some(edges) = self.resolve_value_target(&mut state, pc)? {
                        return Ok(edges);
                    }
                }
                Op::TypeShadowed(guard, target) => {
                    let (certain, possible) = self.type_shadowed(&state, guard)?;
                    return Ok(if certain {
                        [Some((target, state)), None]
                    } else if possible {
                        [
                            Some((target, state.snapshot(self.ctx)?)),
                            Some((pc + 1, state)),
                        ]
                    } else {
                        [Some((pc + 1, state)), None]
                    });
                }
                Op::LoadOptional(slot, name, receiving) => {
                    if !self.load_optional(&mut state, pc, slot, name, receiving)? {
                        return Ok([None, None]);
                    }
                }
                Op::Load(slot) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    let value = if binding.missing {
                        self.facts
                            .union(self.ctx, &[binding.value, Atom::Nil.fact()])?
                    } else {
                        binding.value
                    };
                    if !self.read_value(&mut state, pc, value, Some(slot))? {
                        return Ok([None, None]);
                    }
                }
                Op::Unbound(name, receiving) => {
                    if self.function.namespace.is_some() || self.program.file {
                        if !self.read_fallback(&mut state, pc, name, receiving)? {
                            return Ok([None, None]);
                        }
                        continue;
                    }
                    if let Some(index) = self.root_index(&state, &self.program.members[name])? {
                        if !self.read_global(&mut state, pc, index, receiving)? {
                            return Ok([None, None]);
                        }
                        continue;
                    }
                    let target = self.calls.resolve(self.ctx, &self.program.members[name])?;
                    if target != Target::Undefined {
                        return self.incomplete(pc);
                    }
                    if self.unknown_exports(&state)? {
                        if let Some(edges) = self.dynamic_call(&mut state, pc, Arguments::new())? {
                            return Ok(edges);
                        }
                        continue;
                    }
                    self.issue(
                        pc,
                        IssueKind::Call {
                            target,
                            failure: Failure::Undefined,
                        },
                    )?;
                    self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                    return Ok([None, None]);
                }
                Op::ReceiverBound(slot, target) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    let mut bound = state.snapshot(self.ctx)?;
                    bound
                        .stack
                        .push(self.ctx, Operand::local(binding.value, slot))?;
                    bound.locals.set(
                        self.ctx,
                        slot,
                        Binding {
                            missing: false,
                            ..binding
                        },
                    )?;
                    let bound = (binding.value != Atom::Never.fact()).then_some((target, bound));
                    state.locals.set(
                        self.ctx,
                        slot,
                        Binding {
                            value: Atom::Never.fact(),
                            missing: true,
                            ..binding
                        },
                    )?;
                    return Ok([bound, binding.missing.then_some((pc + 1, state))]);
                }
                Op::Declare(slot) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    let global = slot.checked_sub(state.global_base);
                    let file_slot =
                        global.is_some_and(|index| state.source_slots.files.contains(&index));
                    let builtin = match global {
                        Some(index) => state.source_slots.original(self.ctx, index)?.is_some(),
                        None => false,
                    };
                    if binding.missing && (slot < state.global_base || builtin || file_slot) {
                        let value = self
                            .facts
                            .union(self.ctx, &[binding.value, Atom::Nil.fact()])?;
                        self.store(&mut state, pc, slot, Operand::local(value, slot))?;
                    }
                }
                Op::Shadow(slot) => {
                    if state.capture_locals {
                        state.captures.as_mut().unwrap().shadow(self.ctx, slot)?;
                    }
                    state.locals.set(
                        self.ctx,
                        slot,
                        Binding {
                            value: Atom::Never.fact(),
                            missing: true,
                            owner: blocks::Owner::Function(
                                self.source.callable(self.function_index),
                            ),
                        },
                    )?;
                    state.store(self.ctx, self.facts, slot, Atom::Nil.fact())?;
                }
                Op::BlockArg(index, autosplat) => {
                    let Some(inputs) = self.block_inputs else {
                        return self.incomplete(pc);
                    };
                    let value =
                        blocks::argument(self.ctx, self.facts, inputs.arguments, index, autosplat)?;
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::BlockGiven(arguments, block) => {
                    if arguments || block {
                        self.issue(pc, IssueKind::BlockGivenArguments)?;
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        return Ok([None, None]);
                    }
                    let given = self.given();
                    let value = self.facts.boolean(self.ctx, given)?;
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::CheckBlock => {
                    if !self.given() {
                        if self.scope == blocks::Scope::Invocation {
                            self.issue(pc, IssueKind::MissingBlock)?;
                        }
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::LocalJump))?;
                        return Ok([None, None]);
                    }
                }
                Op::Attach(function) => {
                    if !self.attach(&mut state, function)? {
                        return self.incomplete(pc);
                    }
                }
                Op::Yield(count) => return self.yield_block(state, pc, count),
                Op::Store(slot) => {
                    let operand = *state.stack.data.last().unwrap();
                    self.store(&mut state, pc, slot, operand)?;
                }
                Op::Bind(index, target) => {
                    let input = self.inputs[index];
                    let supplied = match input {
                        Input::Supplied(actual) | Input::Either(actual) => {
                            let supplied = state.snapshot(self.ctx)?;
                            self.bind_supplied(supplied, pc, index, target, actual)?
                        }
                        Input::Default => None,
                    };
                    return Ok([
                        supplied.map(|state| (target, state)),
                        matches!(input, Input::Default | Input::Either(_))
                            .then_some((pc + 1, state)),
                    ]);
                }
                Op::BindEnd => (),
                Op::Normalize(ty, _) => {
                    let actual = state.stack.data.last().unwrap().value;
                    let normalized = self.normalization_contract(&mut state, pc, ty)?;
                    for alternative in normalized.alternatives.data {
                        pending.push(self.ctx, (pc, alternative))?;
                    }
                    let Some(expected) = normalized.value else {
                        return Ok([None, None]);
                    };
                    let relation = self.facts.relation(self.ctx, actual, expected)?;
                    if relation != Relation::Accepted {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                    }
                    if relation == Relation::Rejected {
                        self.issue(pc, IssueKind::Default { actual, expected })?;
                        if !self.facts.overlaps(self.ctx, actual, expected)? {
                            return Ok([None, None]);
                        }
                    }
                    let value = self.facts.normalized(self.ctx, actual, expected)?;
                    if value == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    *state.stack.data.last_mut().unwrap() = Operand::new(value);
                }
                Op::Pop => {
                    state.stack.data.pop().unwrap();
                }
                Op::Dup => state
                    .stack
                    .push(self.ctx, *state.stack.data.last().unwrap())?,
                Op::Array(count) => {
                    let base = state.stack.data.len() - count;
                    let mut elements = Buffer::empty();
                    for operand in &state.stack.data[base..] {
                        self.ctx.charge(1)?;
                        elements.push(self.ctx, operand.value)?;
                    }
                    let value = self.facts.tuple(self.ctx, &elements.data)?;
                    state.stack.data.truncate(base);
                    state.stack.push(
                        self.ctx,
                        Operand {
                            fresh: true,
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::Hash(count) => {
                    let base = state.stack.data.len() - count * 2;
                    let mut fields = Buffer::empty();
                    for pair in state.stack.data[base..].chunks_exact(2) {
                        self.ctx.charge(1)?;
                        let Some(index) = pair[0].literal else {
                            return self.incomplete(pc);
                        };
                        let Some(name) = self.program.constants[index].as_bytes() else {
                            return self.incomplete(pc);
                        };
                        fields.push(self.ctx, (name, pair[1].value, false))?;
                    }
                    let value = self.facts.shape(self.ctx, &fields.data, false)?;
                    state.stack.data.truncate(base);
                    state.stack.push(
                        self.ctx,
                        Operand {
                            fresh: true,
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::RangeStart => {
                    let start = state.stack.data.pop().unwrap().value;
                    // A converted start is an integer on every continuing path.
                    let value = match self.range_endpoint(&state, pc, start)? {
                        Some(n) => self.facts.integer(self.ctx, n)?,
                        None => Atom::Int.fact(),
                    };
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::Range(start, end, exclusive) => {
                    let end = end.then(|| state.stack.data.pop().unwrap().value);
                    let start = start.then(|| state.stack.data.pop().unwrap().value);
                    let mut bounds = [None, None];
                    let mut known = true;
                    for (bound, value) in bounds.iter_mut().zip([start, end]) {
                        if let Some(value) = value {
                            *bound = self.range_endpoint(&state, pc, value)?;
                            known &= bound.is_some();
                        }
                    }
                    let value = if known {
                        self.facts
                            .range(self.ctx, bounds[0], bounds[1], exclusive)?
                    } else {
                        Atom::Range.fact()
                    };
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::Regex(pattern, flags) => {
                    let regex = crate::regex::value::Regex::compile(
                        self.ctx,
                        self.program.constants[pattern].clone(),
                        flags,
                        "regex literal",
                    );
                    let value = match regex {
                        Ok(value) => self.facts.regex(self.ctx, value)?,
                        Err(_) => {
                            self.ctx.checkpoint()?;
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                            self.issue(pc, IssueKind::Regex { pattern })?;
                            return Ok([None, None]);
                        }
                    };
                    self.facts.mark_constant(value);
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::CaseCompare(target, splat) => {
                    let matcher = state.stack.data.pop().unwrap();
                    let target = target.then(|| state.stack.data.pop().unwrap());
                    if let Some(edges) =
                        self.case_compare(&mut state, pc, target, matcher, splat)?
                    {
                        return Ok(edges);
                    }
                }
                Op::Binary(_)
                | Op::AddStore(_)
                | Op::Index(_)
                | Op::Shovel(_)
                | Op::AddressIndex(_)
                | Op::AddressTarget(..)
                | Op::AddressStore => {
                    if let Some(edges) = self.operator_instruction(&mut state, pc, op)? {
                        return Ok(edges);
                    }
                }
                Op::TryBegin(spec) => {
                    state.attempts.push(
                        self.ctx,
                        Attempt {
                            spec,
                            stack: state.stack.data.len(),
                            arguments: state.arguments.data.len(),
                            addresses: state.addresses.data.len(),
                            loops: state.loops.data.len(),
                            raises: state.raises.data.len(),
                            texts: state.texts.data.len(),
                            phase: Phase::Body,
                            error: 0,
                            pending: None,
                        },
                    )?;
                }
                Op::TryBody | Op::TryEnd => {
                    return self.normal_attempt(state, pc, matches!(op, Op::TryBody));
                }
                Op::EnsureEnd => return self.end_ensure(state, pc),
                Op::Retry => {
                    self.ctx.charge(state.attempts.data.len() as u64)?;
                    if let Some(index) = state
                        .attempts
                        .data
                        .iter()
                        .rposition(|h| matches!(h.phase, Phase::Rescue(_)))
                    {
                        return self.transfer(state, pc, Transfer::Retry(index));
                    }
                    if self.current_error & NO_ERROR != 0 {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                    }
                    return if self.current_error as u8 != 0 {
                        self.transfer(state, pc, Transfer::InvalidJump)
                    } else {
                        Ok([None, None])
                    };
                }
                Op::RaiseStart(named, target) => {
                    let class = if let Some((name, slot)) = named {
                        let name = &self.program.members[name];
                        self.ctx.work_bytes(name.len())?;
                        let mut bound = slot
                            .map(|slot| state.locals.get(self.ctx, slot))
                            .transpose()?
                            .is_some_and(|b| !b.missing)
                            || self.calls.global(self.ctx, name)?
                            || self.program.names.contains_key(name)
                            || self.program.declaration_names.contains_key(name);
                        if let Some((_, binding)) = self.ambient_binding(&state, name)? {
                            if binding.missing {
                                return self.incomplete(pc);
                            }
                            bound = true;
                        }
                        if !bound {
                            for host in &self.program.hosts {
                                self.ctx.work_bytes(host.len().max(name.len()))?;
                                if host == name {
                                    bound = true;
                                    break;
                                }
                            }
                        }
                        if bound {
                            None
                        } else {
                            ErrorClass::from_name(name)
                        }
                    } else {
                        None
                    };
                    state
                        .raises
                        .push(self.ctx, class.map_or(0, |c| u16::from(handlers::bit(c))))?;
                    return Ok([
                        Some((if class.is_some() { target } else { pc + 1 }, state)),
                        None,
                    ]);
                }
                Op::RaiseValue => {
                    let value = state.stack.data.pop().unwrap().value;
                    let classes =
                        if matches!(self.facts.atom(value), Some(Atom::Unknown | Atom::Any)) {
                            511
                        } else {
                            256
                        };
                    *state.raises.data.last_mut().unwrap() = classes;
                }
                Op::Raise(count) => {
                    let mut classes = 0;
                    if count == 0 {
                        let current = state.current_error(self.ctx, self.current_error)?;
                        classes = current as u8;
                        if current & NO_ERROR != 0 {
                            classes |= handlers::bit(ErrorClass::Runtime);
                        }
                    } else {
                        let value = state.stack.data.pop().unwrap().value;
                        let target = if count == 2 {
                            state.raises.data.pop().unwrap()
                        } else {
                            u16::from(handlers::bit(ErrorClass::Runtime))
                        };
                        let mut invalid = false;
                        for i in 0..self.facts.arm_count(value) {
                            self.ctx.charge(1)?;
                            match self.facts.atom(self.facts.arm(value, i)) {
                                Some(Atom::Never) => (),
                                Some(Atom::String) => {
                                    classes |= target as u8;
                                    if target & INVALID_CLASS != 0 {
                                        classes |= handlers::bit(ErrorClass::Type);
                                        invalid = true;
                                    }
                                }
                                Some(Atom::Unknown | Atom::Any) => {
                                    classes |= target as u8 | handlers::bit(ErrorClass::Type)
                                }
                                _ => {
                                    classes |= handlers::bit(ErrorClass::Type);
                                    invalid = true;
                                }
                            }
                        }
                        if invalid {
                            self.issue(pc, IssueKind::Raise { value })?;
                        }
                    }
                    self.emit_error(&state, pc, classes)?;
                    return Ok([None, None]);
                }
                Op::AddressLocal(slot) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    let value = if binding.missing {
                        self.facts
                            .union(self.ctx, &[binding.value, Atom::Nil.fact()])?
                    } else {
                        binding.value
                    };
                    state
                        .addresses
                        .push(self.ctx, Address::new(Some(slot), value))?;
                }
                Op::AddressBound(slot, target) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    let mut bound = state.snapshot(self.ctx)?;
                    bound.locals.set(
                        self.ctx,
                        slot,
                        Binding {
                            missing: false,
                            ..binding
                        },
                    )?;
                    bound
                        .addresses
                        .push(self.ctx, Address::new(Some(slot), binding.value))?;
                    state.locals.set(
                        self.ctx,
                        slot,
                        Binding {
                            value: Atom::Never.fact(),
                            missing: true,
                            ..binding
                        },
                    )?;
                    return Ok([
                        (binding.value != Atom::Never.fact()).then_some((target, bound)),
                        binding.missing.then_some((pc + 1, state)),
                    ]);
                }
                Op::RootAddress(name, target) => {
                    if self.program.file {
                        return self.file_edges(state, pc, name, target, true, Receiving::Value);
                    }
                    let name = &self.program.members[name];
                    if let Some((_, binding)) = self.ambient_binding(&state, name)? {
                        if binding.missing {
                            return self.incomplete(pc);
                        }
                        continue;
                    }
                    if let Some(index) = self.root_index(&state, name)? {
                        let slot = state.global_base + index;
                        return self.address_binding(state, pc, slot, target);
                    }
                    if self.calls.global(self.ctx, name)? {
                        return self.incomplete(pc);
                    }
                    for root in &self.program.functions[0].local_names {
                        self.ctx.work_bytes(root.len().max(name.len()))?;
                        if root == name {
                            return self.incomplete(pc);
                        }
                    }
                }
                Op::AddressValue => {
                    let value = state.stack.data.pop().unwrap().value;
                    state.addresses.push(self.ctx, Address::new(None, value))?;
                }
                Op::AddressGlobal(index) => {
                    let Some(index) = self.global_index(&state, index)? else {
                        return self.incomplete(pc);
                    };
                    let Some(address) = self.global_address(&state, pc, index)? else {
                        return Ok([None, None]);
                    };
                    state.addresses.push(self.ctx, address)?;
                }
                Op::AddressMemberTarget(site, read) => {
                    let receiver = state.addresses.data.last().unwrap().value;
                    if self.namespace_receiver(receiver)? {
                        if let Some(edges) =
                            self.namespace_member_target(&mut state, pc, site, read)?
                        {
                            return Ok(edges);
                        }
                        continue;
                    }
                    let name = &self.program.members[site.name];
                    let key = self.facts.string(self.ctx, name.as_bytes())?;
                    let address = state.addresses.data.last_mut().unwrap();
                    address.member = Some(site.name);
                    address.selectors.push(self.ctx, key)?;
                    let receiver = address.value;
                    if read {
                        let result =
                            self.facts
                                .collection_member(self.ctx, receiver, site, name, &[])?;
                        if result.unsupported {
                            // Gradual and object receivers read through ordinary member dispatch.
                            if let Some(edges) =
                                self.member(&mut state, pc, receiver, site, Arguments::new())?
                            {
                                return Ok(edges);
                            }
                            continue;
                        }
                        if result.rejected || result.throws {
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        }
                        if result.rejected {
                            let arguments = self.facts.tuple(self.ctx, &[])?;
                            self.issue(
                                pc,
                                IssueKind::Member {
                                    name: site.name,
                                    receiver,
                                    arguments,
                                },
                            )?;
                        }
                        if result.value == Atom::Never.fact() {
                            return Ok([None, None]);
                        }
                        state.stack.push(self.ctx, Operand::new(result.value))?;
                    }
                }
                Op::AddressDrop => {
                    state.addresses.data.pop().unwrap();
                }
                Op::PrepareMember(site, true) => {
                    let receiver = state.addresses.data.last().unwrap().value;
                    let result = self.facts.prepare_collection_member(
                        self.ctx,
                        receiver,
                        &self.program.members[site.name],
                    )?;
                    if result.rejected || result.throws {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                    }
                    if result.rejected {
                        let arguments = self.facts.tuple(self.ctx, &[])?;
                        self.issue(
                            pc,
                            IssueKind::Member {
                                name: site.name,
                                receiver,
                                arguments,
                            },
                        )?;
                    }
                    if result.unsupported {
                        return self.incomplete(pc);
                    }
                    if result.value == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    state.addresses.data.last_mut().unwrap().value = result.value;
                }
                Op::Mutate(site, count) => {
                    let base = state.stack.data.len() - count;
                    let fresh = matches!(site.method, Some(Method::Store))
                        && count == 2
                        && state.stack.data.last().unwrap().fresh;
                    let mut args = Buffer::empty();
                    for operand in &state.stack.data[base..] {
                        args.push(self.ctx, operand.value)?;
                    }
                    state.stack.data.truncate(base);
                    if let Some(edges) =
                        self.mutate(&mut state, pc, site, &args.data, false, fresh)?
                    {
                        return Ok(edges);
                    }
                }
                Op::Invoke(Invocation::Member(site, true)) => {
                    let pending = state.arguments.data.pop().unwrap();
                    // A forwarded name or native arity needs exact arguments, so an uncertain
                    // shape mutates gradually unless the receiver dispatches to script methods.
                    if pending.arguments.uncertain()
                        && (pending.receiver.is_some()
                            || !self
                                .namespace_receiver(state.addresses.data.last().unwrap().value)?)
                    {
                        if let Some(edges) = self.dynamic_mutation(
                            &mut state,
                            pc,
                            site.into(),
                            pending.arguments,
                            false,
                        )? {
                            return Ok(edges);
                        }
                        continue;
                    }
                    if let Some(receiver) = pending.receiver {
                        self.forwarded_member(
                            &state,
                            pc,
                            receiver,
                            site.into(),
                            &pending.arguments,
                        )?;
                        return Ok([None, None]);
                    }
                    let args = pending.arguments;
                    if matches!(
                        self.facts.atom(state.addresses.data.last().unwrap().value),
                        Some(Atom::Unknown | Atom::Any)
                    ) {
                        if let Some(edges) =
                            self.dynamic_mutation(&mut state, pc, site.into(), args, false)?
                        {
                            return Ok(edges);
                        }
                        continue;
                    }
                    if self.namespace_receiver(state.addresses.data.last().unwrap().value)? {
                        if let Some(edges) =
                            self.namespace_address_call(&mut state, pc, site.into(), args, false)?
                        {
                            return Ok(edges);
                        }
                        continue;
                    }
                    if args.block.is_some()
                        || super::objects::contains(
                            self.ctx,
                            self.facts,
                            state.addresses.data.last().unwrap().value,
                        )?
                    {
                        self.mutable_block(&state, pc, site, &args)?;
                        return Ok([None, None]);
                    }
                    let receiver = state.addresses.data.last().unwrap().value;
                    if !args.keywords.data.is_empty() {
                        let address = state.addresses.data.last().unwrap();
                        let protection = address.protection(self.ctx, self.facts)?;
                        if protection.readonly != Attached::No {
                            self.protected_error(&state, pc, address)?;
                            if protection.report {
                                let arguments =
                                    self.facts.tuple(self.ctx, &args.positional.data)?;
                                self.issue(
                                    pc,
                                    IssueKind::Member {
                                        name: site.name,
                                        receiver,
                                        arguments,
                                    },
                                )?;
                            }
                            if protection.readonly == Attached::Yes {
                                return Ok([None, None]);
                            }
                        }
                    }
                    if builtins::namespace_call(
                        self.ctx,
                        self.facts,
                        receiver,
                        &self.program.members[site.name],
                    )? || builtins::value_member(
                        self.ctx,
                        self.facts,
                        receiver,
                        &self.program.members[site.name],
                    )? {
                        state.addresses.data.pop().unwrap();
                        if let Some(edges) = self.member(&mut state, pc, receiver, site, args)? {
                            return Ok(edges);
                        }
                        continue;
                    }
                    if !args.keywords.data.is_empty() {
                        // Mutators that do not refuse keywords ignore them.
                        let name = &self.program.members[site.name];
                        let count = args.positional.data.len();
                        let (mut refused, mut accepted) = (false, false);
                        for i in 0..self.facts.arm_count(receiver) {
                            self.ctx.charge(1)?;
                            let arm = self.facts.arm(receiver, i);
                            if arm == Atom::Never.fact() {
                                continue;
                            }
                            if self
                                .facts
                                .refuses_call_shape(arm, site, name, count, true, false)
                            {
                                refused = true;
                            } else {
                                accepted = true;
                            }
                        }
                        if refused {
                            self.collection_error(
                                &state,
                                pc,
                                receiver,
                                site.into(),
                                &args,
                                ErrorClass::Runtime,
                            )?;
                            if !accepted {
                                return Ok([None, None]);
                            }
                        }
                    }
                    if let Some(edges) =
                        self.mutate(&mut state, pc, site, &args.positional.data, false, false)?
                    {
                        return Ok(edges);
                    }
                }
                Op::AddressMember(site) | Op::AddressNamespaceField(site) => {
                    if let Some(edges) = self.address_member(
                        &mut state,
                        pc,
                        site,
                        matches!(op, Op::AddressNamespaceField(_)),
                    )? {
                        return Ok(edges);
                    }
                }
                Op::AddressJumpNil(target, value_result) => {
                    let address = state.addresses.data.last().unwrap();
                    let operand = Operand {
                        origin: address.origin(),
                        ..Operand::new(address.value)
                    };
                    let mut edges = self.branch(state, operand, Test::Nil, true, target, pc + 1)?;
                    if let Some((_, nil)) = &mut edges[0] {
                        if value_result {
                            nil.addresses.data.pop();
                            nil.stack.push(self.ctx, Operand::new(Atom::Nil.fact()))?;
                        } else {
                            *nil.addresses.data.last_mut().unwrap() =
                                Address::new(None, Atom::Nil.fact());
                        }
                    }
                    if let Some((_, non_nil)) = &mut edges[1] {
                        let address = non_nil.addresses.data.last_mut().unwrap();
                        address.value =
                            self.facts
                                .filter(self.ctx, address.value, Test::Nil, false)?;
                    }
                    return Ok(edges);
                }
                Op::RootCall(name, _) => {
                    let target = self.root_target(&state, &self.program.members[name])?;
                    state.arguments.push(
                        self.ctx,
                        Pending {
                            target,
                            receiver: None,
                            arguments: Arguments::new(),
                        },
                    )?;
                    if let Some(edges) = self.resolve_value_target(&mut state, pc)? {
                        return Ok(edges);
                    }
                }
                Op::ForwardArguments => {
                    state.arguments.push(
                        self.ctx,
                        Pending {
                            target: Target::Unsupported,
                            receiver: Some(state.addresses.data.last().unwrap().value),
                            arguments: Arguments::new(),
                        },
                    )?;
                }
                Op::Arguments => state.arguments.push(
                    self.ctx,
                    Pending {
                        target: Target::Unsupported,
                        receiver: None,
                        arguments: Arguments::new(),
                    },
                )?,
                // Same-name calls consult the lexical assignment ranges instead.
                Op::Bypass(_) | Op::BypassEnd(_) => (),
                Op::ResolveCall(..) | Op::CallName(..) => {
                    if let Some(edges) = self.resolve_name(&mut state, pc, op)? {
                        return Ok(edges);
                    }
                }
                Op::CallMember(site) => {
                    let receiver = state.stack.data.pop().unwrap().value;
                    if let Some(edges) = self.call_member(&mut state, pc, receiver, site)? {
                        return Ok(edges);
                    }
                }
                Op::CallValue => {
                    let operand = state.stack.data.pop().unwrap();
                    if let Some(edges) = self.set_call_target(&mut state, pc, operand.value)? {
                        return Ok(edges);
                    }
                }
                Op::Call(_, count) | Op::Host(_, count) | Op::NonCallable(count) => {
                    let mut pending = state.arguments.data.pop().unwrap();
                    let base = state.stack.data.len() - count;
                    for operand in &state.stack.data[base..] {
                        self.ctx.charge(1)?;
                        pending.arguments.positional.push(self.ctx, operand.value)?;
                    }
                    state.stack.data.truncate(base);
                    if let Some(edges) =
                        self.invoke(&mut state, pc, pending.target, pending.arguments)?
                    {
                        return Ok(edges);
                    }
                }
                Op::AutoCall(function, receiving) => {
                    let name = &self.program.functions[function].name;
                    if let Some(index) = self.root_index(&state, name)? {
                        if !self.read_global(&mut state, pc, index, receiving)? {
                            return Ok([None, None]);
                        }
                        continue;
                    }
                    if self.calls.global(self.ctx, name)? {
                        return self.incomplete(pc);
                    }
                    let function = self.source.callable(function);
                    let dynamic = self.program.file;
                    if !self.receive_function(&mut state, pc, function, receiving, dynamic)? {
                        return Ok([None, None]);
                    }
                }
                Op::HostValue(host, receiving) => {
                    let name = &self.program.hosts[host];
                    if let Some(index) = self.root_index(&state, name)? {
                        if !self.read_global(&mut state, pc, index, receiving)? {
                            return Ok([None, None]);
                        }
                    } else if self.calls.global(self.ctx, name)? {
                        return self.incomplete(pc);
                    } else {
                        let target = Target::Host(self.source.callable(host));
                        self.receive_host(&state, pc, target, receiving)?;
                        return Ok([None, None]);
                    }
                }
                Op::Argument(kind) => {
                    let operand = state.stack.data.pop().unwrap();
                    let pending = state.arguments.data.last_mut().unwrap();
                    match kind {
                        ArgumentOp::Positional => {
                            pending.arguments.positional.push(self.ctx, operand.value)?
                        }
                        ArgumentOp::Keyword(name) => {
                            let name = self
                                .facts
                                .symbol(self.ctx, self.program.members[name].as_bytes())?;
                            pending.arguments.keyword(self.ctx, name, operand.value)?;
                        }
                        ArgumentOp::Splat | ArgumentOp::KeywordSplat => {
                            let keyword = matches!(kind, ArgumentOp::KeywordSplat);
                            let splat = super::arguments::split_splat(
                                self.ctx,
                                self.facts,
                                operand.value,
                                keyword,
                            )?;
                            let admitted = splat.admitted != Atom::Never.fact();
                            // Any alternative of the wrong kind fails before the call.
                            if splat.rejected != Atom::Never.fact() {
                                if admitted {
                                    let mut failed = state.snapshot(self.ctx)?;
                                    self.narrow_splat(&mut failed, operand, keyword, false)?;
                                    self.emit_error(
                                        &failed,
                                        pc,
                                        handlers::bit(ErrorClass::Runtime),
                                    )?;
                                    self.narrow_splat(&mut state, operand, keyword, true)?;
                                } else {
                                    self.emit_error(
                                        &state,
                                        pc,
                                        handlers::bit(ErrorClass::Runtime),
                                    )?;
                                }
                                self.issue(
                                    pc,
                                    IssueKind::Splat {
                                        actual: operand.value,
                                        keyword,
                                    },
                                )?;
                            }
                            if !admitted {
                                return Ok([None, None]);
                            }
                            let pending = &mut state.arguments.data.last_mut().unwrap().arguments;
                            if keyword {
                                pending.keyword_splat(self.ctx, self.facts, splat.admitted)?;
                            } else {
                                pending.splat(self.ctx, self.facts, splat.admitted)?;
                            }
                        }
                    }
                }
                Op::InvokeRoot(_) | Op::Invoke(Invocation::Resolved) => {
                    let pending = state.arguments.data.pop().unwrap();
                    if let Some(edges) =
                        self.invoke(&mut state, pc, pending.target, pending.arguments)?
                    {
                        return Ok(edges);
                    }
                }
                Op::Unary("!") => {
                    let operand = state.stack.data.pop().unwrap();
                    let test = self
                        .facts
                        .test_result(self.ctx, operand.value, Test::Truth)?;
                    let value = match self.facts.node(test) {
                        super::facts::Node::Boolean(yes) => self.facts.boolean(self.ctx, !yes)?,
                        _ => Atom::Bool.fact(),
                    };
                    let predicate = operand.predicate().map(|p| Predicate { yes: !p.yes, ..p });
                    state.stack.push(
                        self.ctx,
                        Operand {
                            predicate,
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::Unary(op) => {
                    let operand = state.stack.data.pop().unwrap();
                    let result = self.facts.scalar_unary(self.ctx, op, operand.value)?;
                    if result.unsupported {
                        return self.incomplete(pc);
                    }
                    if result.rejected {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        self.issue(
                            pc,
                            IssueKind::Unary {
                                op,
                                value: operand.value,
                            },
                        )?;
                    }
                    state.stack.push(self.ctx, Operand::new(result.value))?;
                }
                Op::PrepareMember(site, false)
                    if !site.scope && matches!(site.method, Some(Method::IsNil)) => {}
                Op::Method(site, 0)
                    if !site.scope
                        && matches!(site.method, Some(Method::IsNil))
                        && self.standard_nil_receiver(
                            &state,
                            state.stack.data.last().unwrap().value,
                        )? =>
                {
                    let operand = state.stack.data.pop().unwrap();
                    let value = self.facts.test_result(self.ctx, operand.value, Test::Nil)?;
                    let predicate = operand.origin.map(|slot| Predicate {
                        slot,
                        test: Test::Nil,
                        yes: true,
                    });
                    state.stack.push(
                        self.ctx,
                        Operand {
                            predicate,
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::PrepareMember(site, false) => {
                    let receiver = state.stack.data.last().unwrap().value;
                    let result = self.facts.prepare_collection_member(
                        self.ctx,
                        receiver,
                        &self.program.members[site.name],
                    )?;
                    if result.rejected || result.throws {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                    }
                    if result.rejected {
                        let arguments = self.facts.tuple(self.ctx, &[])?;
                        self.issue(
                            pc,
                            IssueKind::Member {
                                name: site.name,
                                receiver,
                                arguments,
                            },
                        )?;
                    }
                    if result.unsupported {
                        return self.incomplete(pc);
                    }
                    if result.value == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    state.stack.data.last_mut().unwrap().value = result.value;
                }
                Op::Method(site, 0)
                    if !site.scope && matches!(site.method, Some(Method::IsNil)) =>
                {
                    let operand = state.stack.data.pop().unwrap();
                    if !matches!(
                        self.facts.node(operand.value),
                        super::facts::Node::TypeValue(_)
                    ) && !self.namespace_receiver(operand.value)?
                        && !super::objects::contains(self.ctx, self.facts, operand.value)?
                    {
                        if let Some(edges) =
                            self.member(&mut state, pc, operand.value, site, Arguments::new())?
                        {
                            return Ok(edges);
                        }
                        continue;
                    }
                    for nil in [true, false] {
                        let mut next = state.snapshot(self.ctx)?;
                        if !next.narrow(self.ctx, self.facts, operand, Test::Nil, nil)? {
                            continue;
                        }
                        let receiver =
                            self.facts.filter(self.ctx, operand.value, Test::Nil, nil)?;
                        let edges = self.member(&mut next, pc, receiver, site, Arguments::new())?;
                        self.member_edges(pc, next, edges)?;
                    }
                    return Ok([None, None]);
                }
                Op::Method(site, count) => {
                    let base = state.stack.data.len() - count - 1;
                    let receiver = state.stack.data[base].value;
                    let mut args = Arguments::new();
                    for operand in &state.stack.data[base + 1..] {
                        args.positional.push(self.ctx, operand.value)?;
                    }
                    state.stack.data.truncate(base);
                    if let Some(edges) = self.member(&mut state, pc, receiver, site, args)? {
                        return Ok(edges);
                    }
                }
                Op::Invoke(Invocation::Member(site, false)) => {
                    let args = state.arguments.data.pop().unwrap().arguments;
                    let receiver = state.stack.data.pop().unwrap().value;
                    if let Some(edges) = self.member(&mut state, pc, receiver, site, args)? {
                        return Ok(edges);
                    }
                }
                Op::Jump(target) => return Ok([Some((target, state)), None]),
                Op::JumpFalse(target) | Op::JumpTrue(target) | Op::JumpNil(target) => {
                    let nil = matches!(op, Op::JumpNil(_));
                    let operand = if nil {
                        *state.stack.data.last().unwrap()
                    } else {
                        state.stack.data.pop().unwrap()
                    };
                    let test = if nil { Test::Nil } else { Test::Truth };
                    let yes = !matches!(op, Op::JumpFalse(_));
                    let mut edges = self.branch(state, operand, test, yes, target, pc + 1)?;
                    // Only the bytecode's retained operand or an explicit Dup proves identity.
                    if nil
                        || matches!(
                            pc.checked_sub(1).map(|pc| self.function.code[pc]),
                            Some(Op::Dup)
                        )
                    {
                        for (index, edge) in edges.iter_mut().enumerate() {
                            if let Some((_, state)) = edge {
                                state.stack.data.last_mut().unwrap().value = self.facts.filter(
                                    self.ctx,
                                    operand.value,
                                    test,
                                    if index == 0 { yes } else { !yes },
                                )?;
                            }
                        }
                    }
                    return Ok(edges);
                }
                Op::LoopStart {
                    iterable,
                    expression,
                    next,
                    end,
                } => {
                    let iteration = if iterable {
                        let value = state.stack.data.pop().unwrap().value;
                        let iteration = self.facts.for_iteration(self.ctx, value)?;
                        if iteration.rejected {
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                            self.issue(pc, IssueKind::Iterate { value })?;
                        }
                        if iteration.unsupported {
                            return self.incomplete(pc);
                        }
                        Some(iteration)
                    } else {
                        None
                    };
                    state.loops.push(
                        self.ctx,
                        Loop {
                            end,
                            base: state.stack.data.len(),
                            argument_base: state.arguments.data.len(),
                            address_base: state.addresses.data.len(),
                            attempt_base: state.attempts.data.len(),
                            raise_base: state.raises.data.len(),
                            text_base: state.texts.data.len(),
                            expression,
                            source: iteration.as_ref().map_or(Atom::Nil.fact(), |i| i.source),
                            repeat: iteration.as_ref().map_or(Atom::Never.fact(), |i| i.repeat),
                            last: Atom::Nil.fact(),
                            result: Atom::Never.fact(),
                        },
                    )?;
                    if let Some(iteration) = iteration {
                        let empty = if iteration.empty != Atom::Never.fact() {
                            let mut empty = state.snapshot(self.ctx)?;
                            empty.loops.data.last_mut().unwrap().result = if expression {
                                iteration.empty
                            } else {
                                Atom::Nil.fact()
                            };
                            Some((end, empty))
                        } else {
                            None
                        };
                        // Keep zero iterations separate from the first body. The backedge then
                        // carries only states that actually passed through an iteration.
                        let body = if iteration.item != Atom::Never.fact() {
                            state.stack.push(self.ctx, Operand::new(iteration.item))?;
                            Some((next + 1, state))
                        } else {
                            None
                        };
                        return Ok([empty, body]);
                    }
                }
                Op::Extract(selection) => {
                    let source = state.stack.data.last().unwrap().value;
                    let value = self.facts.extract(self.ctx, source, selection)?;
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::IterNext => {
                    let Exit::Branch(end) = block.exit else {
                        unreachable!()
                    };
                    let current = *state.loops.data.last().unwrap();
                    let mut done = state.snapshot(self.ctx)?;
                    done.loops.data.last_mut().unwrap().result = if current.expression {
                        current.source
                    } else {
                        current.last
                    };
                    let body = if current.repeat != Atom::Never.fact() {
                        state.stack.push(self.ctx, Operand::new(current.repeat))?;
                        Some((pc + 1, state))
                    } else {
                        None
                    };
                    return Ok([Some((end, done)), body]);
                }
                Op::LoopTest => {
                    let Exit::Branch(target) = block.exit else {
                        unreachable!()
                    };
                    let condition = state.stack.data.pop().unwrap();
                    let mut edges =
                        self.branch(state, condition, Test::Truth, false, target, pc + 1)?;
                    if let Some((_, state)) = &mut edges[0] {
                        let current = state.loops.data.last_mut().unwrap();
                        current.result = if current.expression {
                            Atom::Nil.fact()
                        } else {
                            current.last
                        };
                    }
                    return Ok(edges);
                }
                Op::LoopGuard(breaking) => {
                    if state.loops.data.is_empty() && self.block_inputs.is_none() {
                        self.loop_guard(&state, pc, breaking)?;
                    }
                }
                Op::LoopBody | Op::Next(_) | Op::Break(_) => {
                    if state.loops.data.is_empty() && !matches!(op, Op::LoopBody) {
                        if self.block_inputs.is_some() {
                            let supplied = matches!(op, Op::Next(true) | Op::Break(true));
                            let value = if supplied {
                                state.stack.data.pop().unwrap().value
                            } else {
                                Atom::Nil.fact()
                            };
                            let transfer = if matches!(op, Op::Next(_)) {
                                Transfer::Return { pc, value }
                            } else {
                                Transfer::Block {
                                    pc,
                                    completion: blocks::Completion::Break(supplied),
                                    value,
                                }
                            };
                            return self.transfer(state, pc, transfer);
                        }
                        return self.frame_loop_control(state, pc, op);
                    }
                    let current = state.loops.data.last_mut().unwrap();
                    let mut value = Atom::Never.fact();
                    match op {
                        Op::LoopBody => current.last = state.stack.data.pop().unwrap().value,
                        Op::Next(true) => {
                            state.stack.data.pop().unwrap();
                        }
                        Op::Break(true) => value = state.stack.data.pop().unwrap().value,
                        Op::Break(false) => {
                            value = if current.expression {
                                Atom::Nil.fact()
                            } else {
                                current.last
                            }
                        }
                        _ => (),
                    }
                    let Exit::Jump(target) = block.exit else {
                        unreachable!()
                    };
                    let index = state.loops.data.len() - 1;
                    return self.transfer(
                        state,
                        pc,
                        Transfer::Jump {
                            target,
                            index,
                            breaking: matches!(op, Op::Break(_)),
                            value,
                        },
                    );
                }
                Op::LoopEnd => {
                    let current = state.loops.data.pop().unwrap();
                    state.stack.data.truncate(current.base);
                    state.arguments.data.truncate(current.argument_base);
                    state.addresses.data.truncate(current.address_base);
                    state.raises.data.truncate(current.raise_base);
                    state.texts.data.truncate(current.text_base);
                    state.stack.push(self.ctx, Operand::new(current.result))?;
                }
                Op::Return | Op::Finish => {
                    let actual = state.stack.data.pop().unwrap().value;
                    let transfer = if self.block_inputs.is_some() && matches!(op, Op::Return) {
                        if self
                            .layouts
                            .return_home(self.ctx, self.program, self.function_index)?
                        {
                            Transfer::Block {
                                pc,
                                completion: blocks::Completion::Return(0),
                                value: actual,
                            }
                        } else {
                            Transfer::InvalidJump
                        }
                    } else {
                        Transfer::Return { pc, value: actual }
                    };
                    return self.transfer(state, pc, transfer);
                }
                _ => return self.incomplete(pc),
            }
        }
        Ok([Some((block.end, state)), None])
    }
}
