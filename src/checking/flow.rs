use super::{
    addresses::{Address, Attached, Change},
    arguments::{self, Arguments, Failure, Input},
    calls::{Calls, Target},
    facts::{Atom, Fact, Facts},
    graph::{Block, Exit, Graph},
    relation::Relation,
    scalar::Test,
    slots::Slots,
};
use crate::{
    CallContext, Result,
    budget::Buffer,
    bytecode::{ArgumentOp, CallSite, Function, Invocation, Method, Op, Program},
    value::Kind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IssueKind {
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
    Reassignment {
        slot: usize,
        before: Fact,
        after: Fact,
    },
    Unary {
        op: &'static str,
        value: Fact,
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
    pub issues: Buffer<Issue>,
    pub incomplete: Buffer<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    value: Fact,
    missing: bool,
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
    base: usize,
    argument_base: usize,
    address_base: usize,
    attempt_base: usize,
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
}

#[derive(Debug)]
struct State {
    locals: Slots<Binding>,
    stack: Buffer<Operand>,
    loops: Buffer<Loop>,
    arguments: Buffer<Pending>,
    addresses: Buffer<Address>,
    attempts: Buffer<Attempt>,
    widening: Option<usize>,
}

#[derive(Debug)]
struct Pending {
    target: Target,
    arguments: Arguments,
}

impl State {
    fn new(locals: usize) -> Self {
        Self {
            locals: Slots::new(
                locals,
                Binding {
                    value: Atom::Never.fact(),
                    missing: true,
                },
            ),
            stack: Buffer::empty(),
            loops: Buffer::empty(),
            arguments: Buffer::empty(),
            addresses: Buffer::empty(),
            attempts: Buffer::empty(),
            widening: None,
        }
    }

    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut state = Self {
            locals: self.locals.snapshot(ctx)?,
            stack: Buffer::empty(),
            loops: Buffer::empty(),
            arguments: Buffer::empty(),
            addresses: Buffer::empty(),
            attempts: Buffer::empty(),
            widening: None,
        };
        state.stack.extend(ctx, &self.stack.data)?;
        state.loops.extend(ctx, &self.loops.data)?;
        state.attempts.extend(ctx, &self.attempts.data)?;
        for pending in &self.arguments.data {
            ctx.charge(1)?;
            let arguments = pending.arguments.snapshot(ctx)?;
            state.arguments.push(
                ctx,
                Pending {
                    target: pending.target,
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

    fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        backedge: bool,
    ) -> Result<bool> {
        // Freeze precision at the first backedge, including existing values and declared contracts.
        // Later recursive growth becomes gradual beyond that depth; script limits are unchanged.
        let depth = if backedge {
            Some(*self.widening.get_or_insert_with(|| facts.max_depth()))
        } else {
            None
        };
        let mut changed = self.locals.merge(ctx, &other.locals, |ctx, a, b| {
            Ok(Binding {
                value: facts.joined(ctx, a.value, b.value, depth)?,
                missing: a.missing || b.missing,
            })
        })?;
        assert_eq!(self.stack.data.len(), other.stack.data.len());
        assert_eq!(self.loops.data.len(), other.loops.data.len());
        assert_eq!(self.arguments.data.len(), other.arguments.data.len());
        assert_eq!(self.addresses.data.len(), other.addresses.data.len());
        ctx.charge(self.attempts.data.len() as u64)?;
        assert_eq!(self.attempts.data, other.attempts.data);
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
        Ok(changed)
    }

    fn store(&mut self, ctx: &mut CallContext, slot: usize, value: Fact) -> Result<()> {
        self.locals.set(
            ctx,
            slot,
            Binding {
                value,
                missing: false,
            },
        )?;
        // An older operand can survive an assignment in its right-hand expression.
        for operand in &mut self.stack.data {
            ctx.charge(1)?;
            if operand.origin == Some(slot) {
                operand.origin = None;
            }
            if operand
                .predicate
                .is_some_and(|predicate| predicate.slot == slot)
            {
                operand.predicate = None;
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

pub(super) fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &Program,
    function: usize,
    contracts: &[Fact],
) -> Result<Report> {
    let inputs =
        arguments::general_inputs(ctx, facts, &program.functions[function].params, contracts)?;
    analyze_body(
        ctx,
        facts,
        Body {
            program,
            function,
            contracts,
            inputs: &inputs.data,
        },
        &mut super::calls::Unavailable,
    )
}

pub(super) struct Body<'a> {
    pub program: &'a Program,
    pub function: usize,
    pub contracts: &'a [Fact],
    pub inputs: &'a [Input],
}

pub(super) fn analyze_body(
    ctx: &mut CallContext,
    facts: &mut Facts,
    body: Body<'_>,
    calls: &mut dyn Calls,
) -> Result<Report> {
    let Body {
        program,
        function,
        contracts,
        inputs,
    } = body;
    let function_index = function;
    let function = &program.functions[function];
    let mut report = Report {
        returns: Atom::Never.fact(),
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
    };
    ctx.checkpoint()?;
    ctx.charge(function.params.len() as u64)?;
    if function_index == 0
        || program.file
        || function.namespace.is_some()
        || function.name == "<block>"
        || !function.captures.is_empty()
    {
        report.incomplete.push(ctx, 0)?;
        return Ok(report);
    }
    assert_eq!(inputs.len(), function.params.len());
    for ty in function
        .params
        .iter()
        .filter_map(|param| param.ty)
        .chain(function.return_type)
    {
        ctx.charge(1)?;
        if facts.unresolved(contracts[ty]) {
            report.incomplete.push(ctx, 0)?;
            return Ok(report);
        }
    }
    for (slot, name) in function.local_names.iter().enumerate() {
        ctx.charge(function.params.len() as u64 + 1)?;
        if !function.params.iter().any(|param| param.slot == slot) && calls.global(ctx, name)? {
            report.incomplete.push(ctx, 0)?;
            return Ok(report);
        }
    }
    let graph = Graph::new(ctx, &function.code)?;
    let mut initial = State::new(function.locals);
    if !function.binds_parameters {
        for (index, parameter) in function.params.iter().enumerate() {
            ctx.charge(1)?;
            let Input::Supplied(value) = inputs[index] else {
                unreachable!()
            };
            initial.store(ctx, parameter.slot, value)?;
        }
    }
    let mut entries = Buffer::with_capacity(ctx, graph.blocks.data.len())?;
    let mut queued = Buffer::with_capacity(ctx, graph.blocks.data.len())?;
    let mut queue = Buffer::empty();
    for _ in &graph.blocks.data {
        ctx.charge(1)?;
        entries.data.push(None);
        queued.data.push(false);
    }
    entries.data[0] = Some(initial);
    queued.data[0] = true;
    queue.push(ctx, 0)?;
    let mut walker = Walker {
        ctx,
        facts,
        program,
        function,
        contracts,
        inputs,
        calls,
        report: None,
    };
    while let Some(index) = queue.data.pop() {
        walker.ctx.charge(1)?;
        queued.data[index] = false;
        let state = entries.data[index].as_ref().unwrap().snapshot(walker.ctx)?;
        let edges = walker.block(&graph.blocks.data[index], state)?;
        for (pc, state) in edges.into_iter().flatten() {
            let backedge = pc <= graph.blocks.data[index].start;
            let index = graph.at(walker.ctx, pc)?;
            let changed = if let Some(entry) = &mut entries.data[index] {
                entry.join(walker.ctx, walker.facts, &state, backedge)?
            } else {
                entries.data[index] = Some(state);
                true
            };
            if changed && !queued.data[index] {
                queue.push(walker.ctx, index)?;
                queued.data[index] = true;
            }
        }
    }
    // Diagnostics use converged inputs, never provisional branch or loop states.
    walker.report = Some(&mut report);
    for (index, entry) in entries.data.into_iter().enumerate() {
        walker.ctx.charge(1)?;
        if let Some(entry) = entry {
            walker.block(&graph.blocks.data[index], entry)?;
        }
    }
    Ok(report)
}

struct Walker<'a> {
    ctx: &'a mut CallContext,
    facts: &'a mut Facts,
    program: &'a Program,
    function: &'a Function,
    contracts: &'a [Fact],
    inputs: &'a [Input],
    calls: &'a mut dyn Calls,
    report: Option<&'a mut Report>,
}

impl Walker<'_> {
    fn invoke(
        &mut self,
        state: &mut State,
        pc: usize,
        target: Target,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        let result = self.calls.invoke(self.ctx, self.facts, target, args)?;
        for failure in result.failures.data {
            self.issue(pc, IssueKind::Call { target, failure })?;
        }
        if result.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if result.value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        state.stack.push(self.ctx, Operand::new(result.value))?;
        Ok(None)
    }

    fn target(&mut self, state: &State, slot: usize, name: usize) -> Result<Target> {
        if slot != usize::MAX {
            let binding = state.locals.get(self.ctx, slot)?;
            if binding.value != Atom::Never.fact() {
                if binding.missing {
                    return Ok(Target::Unsupported);
                }
                return self.value_target(binding.value);
            }
        }
        self.calls.resolve(self.ctx, &self.program.members[name])
    }

    fn value_target(&mut self, value: Fact) -> Result<Target> {
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
        if self.facts.reassignment_conflicts(self.ctx, before, value)? {
            self.issue(
                pc,
                IssueKind::Reassignment {
                    slot,
                    before,
                    after: value,
                },
            )?;
        }
        state.store(self.ctx, slot, value)?;
        let change = Change::Store {
            same: operand.origin == Some(slot),
            fresh: operand.fresh,
        };
        for address in &mut state.addresses.data {
            self.ctx.charge(1)?;
            if address.root == Some(slot) {
                address.refresh(self.ctx, self.facts, value, &change)?;
            }
        }
        Ok(())
    }

    fn publish(
        &mut self,
        state: &mut State,
        pc: usize,
        address: &Address,
        receiver: Fact,
        change: Change<'_>,
    ) -> Result<Option<Edges>> {
        if !address.supported {
            return self.incomplete(pc).map(Some);
        }
        if address.attached == Attached::No {
            return Ok(None);
        }
        let Some(slot) = address.root else {
            return self.incomplete(pc).map(Some);
        };
        let result = address.rebuild(self.ctx, self.facts, receiver)?;
        if result.unsupported {
            return self.incomplete(pc).map(Some);
        }
        let mut updated = result.value;
        if address.attached == Attached::Maybe {
            let current = state.locals.get(self.ctx, slot)?.value;
            updated = self.facts.union(self.ctx, &[current, updated])?;
        }
        if updated == Atom::Never.fact() {
            return self.incomplete(pc).map(Some);
        }
        state.store(self.ctx, slot, updated)?;
        for pending in &mut state.addresses.data {
            self.ctx.charge(1)?;
            if pending.root == Some(slot) {
                pending.refresh(self.ctx, self.facts, updated, &change)?;
            }
        }
        Ok(None)
    }

    fn mutate(
        &mut self,
        state: &mut State,
        pc: usize,
        site: CallSite,
        args: &[Fact],
        address_result: bool,
        fresh: bool,
    ) -> Result<Option<Edges>> {
        let address = state.addresses.data.pop().unwrap();
        if !address.supported {
            return self.incomplete(pc).map(Some);
        }
        let name = &self.program.members[site.name];
        let result =
            self.facts
                .collection_mutation_member(self.ctx, address.value, site, name, args)?;
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
            let hash = self.facts.plain_hash(arm);
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
        if let Some(edges) = self.publish(state, pc, &address, result.receiver, change)? {
            return Ok(Some(edges));
        }
        if address_result {
            state
                .addresses
                .push(self.ctx, Address::new(None, result.value))?;
        } else {
            let origin = if returns_receiver {
                address.origin()
            } else {
                None
            };
            state.stack.push(
                self.ctx,
                Operand {
                    origin,
                    ..Operand::new(result.value)
                },
            )?;
        }
        Ok(None)
    }

    fn index_outcome(
        &mut self,
        pc: usize,
        receiver: Fact,
        args: &[Fact],
        result: &super::scalar::Operation,
    ) -> Result<Option<Edges>> {
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

    fn declare_attempt(&mut self, state: &mut State, pc: usize, spec: usize) -> Result<()> {
        for &slot in &self.program.handlers[spec].body_locals {
            self.ctx.charge(1)?;
            let binding = state.locals.get(self.ctx, slot)?;
            if binding.missing {
                let value = self
                    .facts
                    .union(self.ctx, &[binding.value, Atom::Nil.fact()])?;
                self.store(state, pc, slot, Operand::local(value, slot))?;
            }
        }
        Ok(())
    }

    fn block(&mut self, block: &Block, mut state: State) -> Result<Edges> {
        for pc in block.start..block.end {
            self.ctx.charge(1)?;
            let op = self.function.code[pc];
            match op {
                Op::Integer(..) => state.stack.push(self.ctx, Operand::new(Atom::Int.fact()))?,
                Op::Nil => state.stack.push(self.ctx, Operand::new(Atom::Nil.fact()))?,
                Op::Constant(index) => {
                    let value = match &self.program.constants[index].0 {
                        Kind::Nil => Atom::Nil.fact(),
                        Kind::Bool(value) => self.facts.boolean(self.ctx, *value)?,
                        Kind::Int(value) => self.facts.integer(self.ctx, *value)?,
                        Kind::Big(_) => Atom::Int.fact(),
                        Kind::Float(_) => Atom::Float.fact(),
                        Kind::Bytes(value) => self.facts.string(self.ctx, &value.data)?,
                        Kind::Symbol(value) => self.facts.symbol(self.ctx, &value.data)?,
                        Kind::Duration(_) => Atom::Duration.fact(),
                        Kind::Time(_) | Kind::Zoned(_) => Atom::Time.fact(),
                        Kind::Money(_) => Atom::Money.fact(),
                        _ => return self.incomplete(pc),
                    };
                    state.stack.push(
                        self.ctx,
                        Operand {
                            literal: Some(index),
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::Load(slot) | Op::LoadOptional(slot, _) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    if binding.missing && matches!(op, Op::LoadOptional(..)) {
                        return self.incomplete(pc);
                    }
                    let value = if binding.missing {
                        self.facts
                            .union(self.ctx, &[binding.value, Atom::Nil.fact()])?
                    } else {
                        binding.value
                    };
                    state.stack.push(self.ctx, Operand::local(value, slot))?;
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
                        },
                    )?;
                    return Ok([bound, binding.missing.then_some((pc + 1, state))]);
                }
                Op::Declare(slot) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    if binding.missing {
                        let value = self
                            .facts
                            .union(self.ctx, &[binding.value, Atom::Nil.fact()])?;
                        self.store(&mut state, pc, slot, Operand::local(value, slot))?;
                    }
                }
                Op::Shadow(slot) => {
                    self.store(&mut state, pc, slot, Operand::new(Atom::Nil.fact()))?
                }
                Op::Store(slot) => {
                    let operand = *state.stack.data.last().unwrap();
                    self.store(&mut state, pc, slot, operand)?;
                }
                Op::Bind(index, target) => {
                    let parameter = &self.function.params[index];
                    let mut supplied = state.snapshot(self.ctx)?;
                    let input = self.inputs[index];
                    let value = match input {
                        Input::Supplied(value) | Input::Either(value) => Some(value),
                        Input::Default => None,
                    };
                    if let Some(value) = value {
                        supplied.store(self.ctx, parameter.slot, value)?;
                    }
                    return Ok([
                        value.is_some().then_some((target, supplied)),
                        matches!(input, Input::Default | Input::Either(_))
                            .then_some((pc + 1, state)),
                    ]);
                }
                Op::BindEnd => (),
                Op::Normalize(ty, _) => {
                    let operand = state.stack.data.last_mut().unwrap();
                    let expected = self.contracts[ty];
                    if self.facts.relation(self.ctx, operand.value, expected)? == Relation::Rejected
                    {
                        self.issue(
                            pc,
                            IssueKind::Default {
                                actual: operand.value,
                                expected,
                            },
                        )?;
                    }
                    *operand =
                        Operand::new(self.facts.normalized(self.ctx, operand.value, expected)?);
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
                Op::Range(start, end, _) => {
                    for _ in 0..usize::from(start) + usize::from(end) {
                        let value = state.stack.data.pop().unwrap().value;
                        if self.facts.relation(self.ctx, value, Atom::Int.fact())?
                            == Relation::Rejected
                        {
                            self.issue(pc, IssueKind::Range { value })?;
                        }
                    }
                    state
                        .stack
                        .push(self.ctx, Operand::new(Atom::Range.fact()))?;
                }
                Op::Index(count) => {
                    let base = state.stack.data.len() - count - 1;
                    let receiver = state.stack.data[base].value;
                    let mut args = Buffer::empty();
                    for operand in &state.stack.data[base + 1..] {
                        args.push(self.ctx, operand.value)?;
                    }
                    let result = self
                        .facts
                        .collection_index(self.ctx, receiver, &args.data)?;
                    if result.rejected {
                        let arguments = self.facts.tuple(self.ctx, &args.data)?;
                        self.issue(
                            pc,
                            IssueKind::Index {
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
                    state.stack.data.truncate(base);
                    state.stack.push(self.ctx, Operand::new(result.value))?;
                }
                Op::TryBegin(spec) => {
                    let handler = &self.program.handlers[spec];
                    if !handler.rescues.is_empty()
                        || handler.alternate.is_some()
                        || handler.ensure.is_some()
                    {
                        return self.incomplete(pc);
                    }
                    state.attempts.push(
                        self.ctx,
                        Attempt {
                            spec,
                            stack: state.stack.data.len(),
                            arguments: state.arguments.data.len(),
                            addresses: state.addresses.data.len(),
                        },
                    )?;
                }
                Op::TryBody => {
                    let attempt = state.attempts.data.pop().unwrap();
                    let operand = state.stack.data.pop().unwrap();
                    self.declare_attempt(&mut state, pc, attempt.spec)?;
                    state.stack.data.truncate(attempt.stack);
                    state.arguments.data.truncate(attempt.arguments);
                    state.addresses.data.truncate(attempt.addresses);
                    state.stack.push(self.ctx, operand)?;
                }
                Op::Shovel(site) => {
                    let value = state.stack.data.pop().unwrap().value;
                    let receiver = state.addresses.data.last().unwrap().value;
                    let mut allowed = Buffer::empty();
                    let mut rejected = false;
                    for i in 0..self.facts.arm_count(receiver) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(receiver, i);
                        match self.facts.node(arm) {
                            super::facts::Node::Array(_)
                            | super::facts::Node::Tuple(_)
                            | super::facts::Node::Atom(Atom::Unknown | Atom::Any) => {
                                allowed.push(self.ctx, arm)?
                            }
                            super::facts::Node::Named(_) | super::facts::Node::Nominal { .. } => {
                                return self.incomplete(pc);
                            }
                            super::facts::Node::Atom(Atom::Never) => (),
                            _ => rejected = true,
                        }
                    }
                    if rejected {
                        self.issue(
                            pc,
                            IssueKind::Binary {
                                op: "<<",
                                left: receiver,
                                right: value,
                            },
                        )?;
                    }
                    let receiver = self.facts.union(self.ctx, &allowed.data)?;
                    if receiver == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    state.addresses.data.last_mut().unwrap().value = receiver;
                    if let Some(edges) =
                        self.mutate(&mut state, pc, site, &[value], false, false)?
                    {
                        return Ok(edges);
                    }
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
                        },
                    )?;
                    return Ok([
                        (binding.value != Atom::Never.fact()).then_some((target, bound)),
                        binding.missing.then_some((pc + 1, state)),
                    ]);
                }
                Op::RootAddress(name, _) => {
                    let name = &self.program.members[name];
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
                Op::AddressIndex(count) | Op::AddressTarget(count, _) => {
                    let base = state.stack.data.len() - count;
                    let mut args = Buffer::empty();
                    for operand in &state.stack.data[base..] {
                        args.push(self.ctx, operand.value)?;
                    }
                    state.stack.data.truncate(base);
                    let address = state.addresses.data.last_mut().unwrap();
                    let receiver = address.value;
                    let result = match op {
                        Op::AddressIndex(_) => {
                            Some(address.index(self.ctx, self.facts, &args.data)?)
                        }
                        Op::AddressTarget(_, read) => {
                            address.target(self.ctx, self.facts, &args.data, read)?
                        }
                        _ => unreachable!(),
                    };
                    if let Some(result) = result {
                        if let Some(edges) =
                            self.index_outcome(pc, receiver, &args.data, &result)?
                        {
                            return Ok(edges);
                        }
                        if matches!(op, Op::AddressTarget(..)) {
                            state.stack.push(self.ctx, Operand::new(result.value))?;
                        }
                    }
                }
                Op::AddressMemberTarget(site, read) => {
                    let name = &self.program.members[site.name];
                    let key = self.facts.string(self.ctx, name.as_bytes())?;
                    let address = state.addresses.data.last_mut().unwrap();
                    address.selectors.push(self.ctx, key)?;
                    let receiver = address.value;
                    if read {
                        let result =
                            self.facts
                                .collection_member(self.ctx, receiver, site, name, &[])?;
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
                        state.stack.push(self.ctx, Operand::new(result.value))?;
                    }
                }
                Op::AddressStore => {
                    let value = state.stack.data.pop().unwrap();
                    let address = state.addresses.data.pop().unwrap();
                    if !address.supported {
                        return self.incomplete(pc);
                    }
                    let selectors = self.facts.tuple(self.ctx, &address.selectors.data)?;
                    let [key] = address.selectors.data.as_slice() else {
                        self.issue(
                            pc,
                            IssueKind::Write {
                                receiver: address.value,
                                selectors,
                                value: value.value,
                            },
                        )?;
                        return Ok([None, None]);
                    };
                    let result =
                        self.facts
                            .collection_write(self.ctx, address.value, *key, value.value)?;
                    if result.rejected {
                        self.issue(
                            pc,
                            IssueKind::Write {
                                receiver: address.value,
                                selectors,
                                value: value.value,
                            },
                        )?;
                    }
                    if result.unsupported {
                        return self.incomplete(pc);
                    }
                    if result.value == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    let change = Change::Mutation {
                        address: &address,
                        method: None,
                        args: &[],
                        fresh: value.fresh,
                    };
                    if let Some(edges) =
                        self.publish(&mut state, pc, &address, result.receiver, change)?
                    {
                        return Ok(edges);
                    }
                    let mut value = value;
                    if address.attached != Attached::No {
                        if value.origin == address.root {
                            value.origin = None;
                        }
                        if value
                            .predicate
                            .is_some_and(|predicate| Some(predicate.slot) == address.root)
                        {
                            value.predicate = None;
                        }
                    }
                    state.stack.push(self.ctx, value)?;
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
                    let args = state.arguments.data.pop().unwrap().arguments;
                    if !args.keywords.data.is_empty() {
                        return self.incomplete(pc);
                    }
                    if let Some(edges) =
                        self.mutate(&mut state, pc, site, &args.positional.data, false, false)?
                    {
                        return Ok(edges);
                    }
                }
                Op::AddressMember(site) | Op::AddressNamespaceField(site) => {
                    let name = &self.program.members[site.name];
                    let receiver = state.addresses.data.last().unwrap().value;
                    let (mut fields, mut absent) = (false, false);
                    for i in 0..self.facts.arm_count(receiver) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(receiver, i);
                        match self.facts.node(arm) {
                            super::facts::Node::Shape(_, open, _, _)
                                if matches!(op, Op::AddressMember(_)) =>
                            {
                                let selected =
                                    self.facts.selected_field(self.ctx, arm, name.as_bytes())?;
                                if *open
                                    || selected.is_some_and(|(_, optional)| {
                                        optional && crate::members::hash_builtin(name)
                                    })
                                {
                                    return self.incomplete(pc);
                                }
                                fields |= selected.is_some();
                                absent |= selected.is_none();
                            }
                            super::facts::Node::Hash(..) if matches!(op, Op::AddressMember(_)) => {
                                return self.incomplete(pc);
                            }
                            _ => absent = true,
                        }
                    }
                    if fields && absent {
                        return self.incomplete(pc);
                    }
                    if fields {
                        let key = self.facts.string(self.ctx, name.as_bytes())?;
                        let result = state.addresses.data.last_mut().unwrap().index(
                            self.ctx,
                            self.facts,
                            &[key],
                        )?;
                        if let Some(edges) = self.index_outcome(pc, receiver, &[key], &result)? {
                            return Ok(edges);
                        }
                    } else if crate::bytecode::mutating_member(name)
                        && matches!(op, Op::AddressMember(_))
                    {
                        if let Some(edges) = self.mutate(&mut state, pc, site, &[], true, false)? {
                            return Ok(edges);
                        }
                    } else {
                        let result =
                            self.facts
                                .collection_member(self.ctx, receiver, site, name, &[])?;
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
                        *state.addresses.data.last_mut().unwrap() =
                            Address::new(None, result.value);
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
                    let target = self.calls.resolve(self.ctx, &self.program.members[name])?;
                    state.arguments.push(
                        self.ctx,
                        Pending {
                            target,
                            arguments: Arguments::new(),
                        },
                    )?;
                }
                Op::Arguments => state.arguments.push(
                    self.ctx,
                    Pending {
                        target: Target::Unsupported,
                        arguments: Arguments::new(),
                    },
                )?,
                Op::ResolveCall(slot, name, _) => {
                    let target = self.target(&state, slot, name)?;
                    if target == Target::Undefined {
                        self.issue(
                            pc,
                            IssueKind::Call {
                                target,
                                failure: Failure::Undefined,
                            },
                        )?;
                        return Ok([None, None]);
                    }
                    state.arguments.push(
                        self.ctx,
                        Pending {
                            target,
                            arguments: Arguments::new(),
                        },
                    )?;
                }
                Op::CallName(slot, name) => {
                    let target = self.target(&state, slot, name)?;
                    if target == Target::Undefined {
                        self.issue(
                            pc,
                            IssueKind::Call {
                                target,
                                failure: Failure::Undefined,
                            },
                        )?;
                        return Ok([None, None]);
                    }
                    state.arguments.data.last_mut().unwrap().target = target;
                }
                Op::CallValue => {
                    let operand = state.stack.data.pop().unwrap();
                    state.arguments.data.last_mut().unwrap().target =
                        self.value_target(operand.value)?;
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
                Op::AutoCall(function) => {
                    let name = &self.program.functions[function].name;
                    // A root override is read as a value here, not called implicitly.
                    if self.calls.global(self.ctx, name)? {
                        return self.incomplete(pc);
                    }
                    let target = self.calls.resolve(self.ctx, name)?;
                    if let Some(edges) = self.invoke(&mut state, pc, target, Arguments::new())? {
                        return Ok(edges);
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
                        ArgumentOp::Splat => {
                            if let super::facts::Node::Tuple(values) =
                                self.facts.node(operand.value)
                            {
                                pending
                                    .arguments
                                    .positional
                                    .extend(self.ctx, &values.data)?;
                            } else {
                                if self.facts.known_primitive(self.ctx, operand.value)? {
                                    self.issue(
                                        pc,
                                        IssueKind::Splat {
                                            actual: operand.value,
                                            keyword: false,
                                        },
                                    )?;
                                    return Ok([None, None]);
                                }
                                return self.incomplete(pc);
                            }
                        }
                        ArgumentOp::KeywordSplat => {
                            let mut keywords = Buffer::empty();
                            if let super::facts::Node::Shape(fields, false, _, _) =
                                self.facts.node(operand.value)
                            {
                                for field in &fields.data {
                                    self.ctx.charge(1)?;
                                    if field.optional {
                                        return self.incomplete(pc);
                                    }
                                    keywords.push(self.ctx, (field.name.clone(), field.value))?;
                                }
                            } else {
                                if self.facts.known_primitive(self.ctx, operand.value)? {
                                    self.issue(
                                        pc,
                                        IssueKind::Splat {
                                            actual: operand.value,
                                            keyword: true,
                                        },
                                    )?;
                                    return Ok([None, None]);
                                }
                                return self.incomplete(pc);
                            }
                            for (name, value) in &keywords.data {
                                let name = self.facts.symbol(self.ctx, name.as_bytes().unwrap())?;
                                pending.arguments.keyword(self.ctx, name, *value)?;
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
                Op::Binary(_) | Op::AddStore(_) => {
                    let op = match op {
                        Op::Binary(op) => op,
                        _ => "+",
                    };
                    let right = state.stack.data.pop().unwrap();
                    let left = state.stack.data.pop().unwrap();
                    let result = self
                        .facts
                        .scalar_binary(self.ctx, op, left.value, right.value)?;
                    if result.unsupported {
                        return self.incomplete(pc);
                    }
                    if result.rejected {
                        self.issue(
                            pc,
                            IssueKind::Binary {
                                op,
                                left: left.value,
                                right: right.value,
                            },
                        )?;
                    }
                    let predicate = if matches!(op, "==" | "!=")
                        && !result.rejected
                        && self.facts.known_primitive(self.ctx, left.value)?
                        && self.facts.known_primitive(self.ctx, right.value)?
                    {
                        let origin = if left.value == Atom::Nil.fact() {
                            right.origin
                        } else if right.value == Atom::Nil.fact() {
                            left.origin
                        } else {
                            None
                        };
                        origin.map(|slot| Predicate {
                            slot,
                            test: Test::Nil,
                            yes: op == "==",
                        })
                    } else {
                        None
                    };
                    if let Op::AddStore(slot) = self.function.code[pc] {
                        self.store(&mut state, pc, slot, Operand::new(result.value))?;
                    }
                    state.stack.push(
                        self.ctx,
                        Operand {
                            predicate,
                            ..Operand::new(result.value)
                        },
                    )?;
                }
                Op::PrepareMember(site, false)
                    if !site.scope && matches!(site.method, Some(Method::IsNil)) => {}
                Op::Method(site, 0)
                    if !site.scope && matches!(site.method, Some(Method::IsNil)) =>
                {
                    let operand = state.stack.data.pop().unwrap();
                    // Nominal receivers may implement their own method; that dispatch is unfinished.
                    if !self.facts.known_primitive(self.ctx, operand.value)? {
                        return self.incomplete(pc);
                    }
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
                Op::Method(site, count) => {
                    let base = state.stack.data.len() - count - 1;
                    let receiver = state.stack.data[base].value;
                    let mut args = Buffer::empty();
                    for operand in &state.stack.data[base + 1..] {
                        args.push(self.ctx, operand.value)?;
                    }
                    let result = self.facts.collection_member(
                        self.ctx,
                        receiver,
                        site,
                        &self.program.members[site.name],
                        &args.data,
                    )?;
                    if result.rejected {
                        let arguments = self.facts.tuple(self.ctx, &args.data)?;
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
                    state.stack.data.truncate(base);
                    state.stack.push(self.ctx, Operand::new(result.value))?;
                }
                Op::Invoke(Invocation::Member(site, false)) => {
                    let args = state.arguments.data.pop().unwrap().arguments;
                    if !args.keywords.data.is_empty() {
                        return self.incomplete(pc);
                    }
                    let receiver = state.stack.data.pop().unwrap().value;
                    let result = self.facts.collection_member(
                        self.ctx,
                        receiver,
                        site,
                        &self.program.members[site.name],
                        &args.positional.data,
                    )?;
                    if result.rejected {
                        let arguments = self.facts.tuple(self.ctx, &args.positional.data)?;
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
                    state.stack.push(self.ctx, Operand::new(result.value))?;
                }
                Op::Jump(target) => return Ok([Some((target, state)), None]),
                Op::JumpFalse(target) | Op::JumpTrue(target) | Op::JumpNil(target) => {
                    let nil = matches!(op, Op::JumpNil(_));
                    let operand = if nil {
                        *state.stack.data.last().unwrap()
                    } else {
                        state.stack.data.pop().unwrap()
                    };
                    return self.branch(
                        state,
                        operand,
                        if nil { Test::Nil } else { Test::Truth },
                        !matches!(op, Op::JumpFalse(_)),
                        target,
                        pc + 1,
                    );
                }
                Op::LoopStart {
                    iterable,
                    expression,
                    next,
                    end,
                } => {
                    let iteration = if iterable {
                        let value = state.stack.data.pop().unwrap().value;
                        let iteration = self.facts.iteration(self.ctx, value)?;
                        if iteration.rejected {
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
                            base: state.stack.data.len(),
                            argument_base: state.arguments.data.len(),
                            address_base: state.addresses.data.len(),
                            attempt_base: state.attempts.data.len(),
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
                Op::LoopBody | Op::Next(_) | Op::Break(_) => {
                    if state.loops.data.is_empty() {
                        return self.incomplete(pc);
                    }
                    let attempt_base = state.loops.data.last().unwrap().attempt_base;
                    while state.attempts.data.len() > attempt_base {
                        let attempt = state.attempts.data.pop().unwrap();
                        self.declare_attempt(&mut state, pc, attempt.spec)?;
                    }
                    let current = state.loops.data.last_mut().unwrap();
                    match op {
                        Op::LoopBody => current.last = state.stack.data.pop().unwrap().value,
                        Op::Next(true) => {
                            state.stack.data.pop().unwrap();
                        }
                        Op::Break(true) => current.result = state.stack.data.pop().unwrap().value,
                        Op::Break(false) => {
                            current.result = if current.expression {
                                Atom::Nil.fact()
                            } else {
                                current.last
                            }
                        }
                        _ => (),
                    }
                    state.stack.data.truncate(current.base);
                    state.arguments.data.truncate(current.argument_base);
                    state.addresses.data.truncate(current.address_base);
                    let Exit::Jump(target) = block.exit else {
                        unreachable!()
                    };
                    return Ok([Some((target, state)), None]);
                }
                Op::LoopEnd => {
                    let current = state.loops.data.pop().unwrap();
                    state.stack.data.truncate(current.base);
                    state.arguments.data.truncate(current.argument_base);
                    state.addresses.data.truncate(current.address_base);
                    state.stack.push(self.ctx, Operand::new(current.result))?;
                }
                Op::Return | Op::Finish => {
                    let actual = state.stack.data.pop().unwrap().value;
                    if let Some(ty) = self.function.return_type {
                        let expected = self.contracts[ty];
                        if self.facts.relation(self.ctx, actual, expected)? == Relation::Rejected {
                            self.issue(pc, IssueKind::Return { actual, expected })?;
                        }
                    }
                    if let Some(report) = self.report.as_mut() {
                        report.returns = self.facts.union(self.ctx, &[report.returns, actual])?;
                    }
                    return Ok([None, None]);
                }
                _ => return self.incomplete(pc),
            }
        }
        Ok([Some((block.end, state)), None])
    }
}
