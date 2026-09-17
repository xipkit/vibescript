use super::{
    addresses::{Address, Attached, Change},
    arguments::{self, Arguments, Failure, Input},
    blocks, builtins,
    calls::{Calls, Target},
    facts::{Atom, Fact, Facts},
    graph::{Block, Exit, Graph},
    lexical::Layouts,
    relation::Relation,
    scalar::Test,
    slots::Slots,
};
use crate::{
    CallContext, ErrorClass, Result,
    budget::Buffer,
    bytecode::{ArgumentOp, CallSite, Function, Invocation, Method, Op, Program},
    value::Kind,
};

mod callbacks;
mod effects;
mod handlers;
mod native;
use handlers::{Phase, Transfer};

// Absence must survive joins with inherited rescued errors.
pub(super) const NO_ERROR: u16 = 1 << 8;
const INVALID_CLASS: u16 = 1 << 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IssueKind {
    MissingBlock,
    BlockGivenArguments,
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
    pub throws: u8,
    pub issues: Buffer<Issue>,
    pub incomplete: Buffer<usize>,
    pub block_exits: Buffer<blocks::Exit>,
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
    phase: Phase,
    error: u8,
    pending: Option<Transfer>,
}

#[derive(Debug)]
struct State {
    locals: Slots<Binding>,
    captures: Option<blocks::Captures>,
    incoming: Option<blocks::Captures>,
    stack: Buffer<Operand>,
    loops: Buffer<Loop>,
    arguments: Buffer<Pending>,
    addresses: Buffer<Address>,
    attempts: Buffer<Attempt>,
    raises: Buffer<u16>,
    widening: Option<usize>,
}

#[derive(Debug)]
struct Pending {
    target: Target,
    arguments: Arguments,
}

impl State {
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
    fn new(locals: usize) -> Self {
        Self {
            captures: None,
            incoming: None,
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
            raises: Buffer::empty(),
            widening: None,
        }
    }

    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut state = Self {
            locals: self.locals.snapshot(ctx)?,
            incoming: self
                .incoming
                .as_ref()
                .map(|c| c.snapshot(ctx))
                .transpose()?,
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
            widening: None,
        };
        state.stack.extend(ctx, &self.stack.data)?;
        state.loops.extend(ctx, &self.loops.data)?;
        state.attempts.extend(ctx, &self.attempts.data)?;
        state.raises.extend(ctx, &self.raises.data)?;
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
        if let (Some(a), Some(b)) = (&mut self.captures, &other.captures) {
            changed |= a.join(ctx, facts, b, depth)?;
        }
        if let (Some(a), Some(b)) = (&mut self.incoming, &other.incoming) {
            changed |= a.join(ctx, facts, b, depth)?;
        }
        assert_eq!(self.stack.data.len(), other.stack.data.len());
        assert_eq!(self.loops.data.len(), other.loops.data.len());
        assert_eq!(self.arguments.data.len(), other.arguments.data.len());
        assert_eq!(self.addresses.data.len(), other.addresses.data.len());
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

    fn store(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
    ) -> Result<()> {
        if let Some(captures) = &mut self.captures {
            captures.store(ctx, facts, slot, value)?;
        }
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
            current_error: NO_ERROR,
            block: None,
            incoming: None,
            layouts: None,
        },
        &mut super::calls::Unavailable,
    )
}

pub(super) struct Body<'a> {
    pub program: &'a Program,
    pub function: usize,
    pub contracts: &'a [Fact],
    pub inputs: &'a [Input],
    pub current_error: u16,
    pub block: Option<&'a blocks::Inputs<'a>>,
    pub incoming: Option<&'a blocks::Closure>,
    pub layouts: Option<&'a Layouts>,
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
        current_error,
        block,
        incoming,
        layouts,
    } = body;
    let function_index = function;
    let function = &program.functions[function];
    let mut report = Report {
        returns: Atom::Never.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        block_exits: Buffer::empty(),
    };
    ctx.checkpoint()?;
    ctx.charge(function.params.len() as u64)?;
    if function_index == 0
        || program.file
        || function.namespace.is_some()
        || (block.is_none() && (function.name == "<block>" || !function.captures.is_empty()))
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
    let owned_layouts;
    let layouts = if let Some(layouts) = layouts {
        layouts
    } else {
        owned_layouts = Layouts::new(ctx, program)?;
        &owned_layouts
    };
    let locals = layouts.locals(ctx, program, function_index)?;
    let mut initial = State::new(locals);
    if let Some(incoming) = incoming {
        let mut captures = Buffer::empty();
        for link in &incoming.captures.data {
            ctx.charge(1)?;
            captures.push(
                ctx,
                blocks::Capture {
                    slot: link.slot,
                    value: link.value,
                },
            )?;
        }
        let incoming_locals = layouts.locals(ctx, program, incoming.function)?;
        initial.incoming = Some(blocks::Captures::new(ctx, incoming_locals, &captures.data)?);
    }
    if let Some(block) = block {
        assert_eq!(function.name, "<block>");
        for capture in block.captures {
            ctx.charge(1)?;
            assert!(
                layouts
                    .capture(ctx, program, function_index, capture.slot)?
                    .is_some()
            );
            initial.store(ctx, facts, capture.slot, capture.value)?;
        }
        initial.captures = Some(blocks::Captures::new(ctx, locals, block.captures)?);
    }
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
    let mut queue = Buffer::empty();
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
        ctx,
        facts,
        program,
        function,
        function_index,
        layouts,
        contracts,
        inputs,
        current_error,
        block_inputs: block,
        incoming,
        calls,
        report: None,
        extra: Buffer::empty(),
    };
    while let Some((index, polarity)) = queue.data.pop() {
        walker.ctx.charge(1)?;
        entries.data[index].data[polarity].queued = false;
        let state = entries.data[index].data[polarity]
            .state
            .snapshot(walker.ctx)?;
        let edges = walker.block(&graph.blocks.data[index], state)?;
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
    report: Option<&'a mut Report>,
    extra: Buffer<(usize, State)>,
}

impl Walker<'_> {
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
        target: Target,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        let current_error = state.current_error(self.ctx, self.current_error)?;
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
            _ => self
                .calls
                .invoke(self.ctx, self.facts, target, args, current_error)?,
        };
        let mut classes = result.throws;
        for failure in result.failures.data {
            classes |= handlers::bit(match failure {
                Failure::Type { .. }
                | Failure::NonCallable
                | Failure::Undefined
                | Failure::HostArity
                | Failure::HostKeywords
                | Failure::BuiltinArity
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
        self.emit_error(state, pc, classes)?;
        if result.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if let Some(attached) = attached {
            self.call_exits(state, pc, &attached, result.exits)?;
            return Ok(Some([None, None]));
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
        state.store(self.ctx, self.facts, slot, value)?;
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
        state.store(self.ctx, self.facts, slot, updated)?;
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
        let name = &self.program.members[site.name];
        let protection = address.protection(self.ctx, self.facts)?;
        if protection != Attached::No {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            let arguments = self.facts.tuple(self.ctx, args)?;
            self.issue(
                pc,
                IssueKind::Member {
                    name: site.name,
                    receiver: address.value,
                    arguments,
                },
            )?;
            if protection == Attached::Yes {
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
        let result =
            self.facts
                .collection_mutation_member(self.ctx, address.value, site, name, args)?;
        if result.rejected {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
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
        state: &State,
        pc: usize,
        receiver: Fact,
        args: &[Fact],
        result: &super::scalar::Operation,
    ) -> Result<Option<Edges>> {
        if result.rejected {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
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

    fn block(&mut self, block: &Block, mut state: State) -> Result<Edges> {
        for pc in block.start..block.end {
            self.ctx.charge(1)?;
            let op = self.function.code[pc];
            let errors = self.potential_errors(&state, op)?;
            self.emit_error(&state, pc, errors)?;
            match op {
                Op::Integer(..) => state.stack.push(self.ctx, Operand::new(Atom::Int.fact()))?,
                Op::Nil => state.stack.push(self.ctx, Operand::new(Atom::Nil.fact()))?,
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
                    state.stack.push(
                        self.ctx,
                        Operand {
                            literal: Some(index),
                            ..Operand::new(value)
                        },
                    )?;
                }
                Op::Global(index) | Op::GlobalReceiver(index, _) => {
                    let (global, value) = &self.program.globals[index];
                    if self.calls.global(self.ctx, global.name())? {
                        return self.incomplete(pc);
                    }
                    if let Kind::Builtin(builtin) = value.0 {
                        let read = !matches!(op, Op::GlobalReceiver(_, false)) || !builtin.auto();
                        if read {
                            if !builtin.auto() {
                                self.issue(
                                    pc,
                                    IssueKind::Call {
                                        target: Target::Builtin(builtin),
                                        failure: Failure::BuiltinValue,
                                    },
                                )?;
                                self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                                return Ok([None, None]);
                            }
                            if let Some(edges) = self.invoke(
                                &mut state,
                                pc,
                                Target::Builtin(builtin),
                                Arguments::new(),
                            )? {
                                return Ok(edges);
                            }
                            continue;
                        }
                    }
                    let value = builtins::global(self.ctx, self.facts, value)?;
                    state.stack.push(self.ctx, Operand::new(value))?;
                }
                Op::ResolveGlobalCall(index) => {
                    let name = self.program.globals[index].0.name();
                    let target = self.calls.resolve(self.ctx, name)?;
                    state.arguments.push(
                        self.ctx,
                        Pending {
                            target,
                            arguments: Arguments::new(),
                        },
                    )?;
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
                    let mut readable = Buffer::empty();
                    for index in 0..self.facts.arm_count(value) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(value, index);
                        if matches!(self.facts.node(arm), super::facts::Node::Offset(_)) {
                            let target = Target::Offset(arm);
                            self.issue(
                                pc,
                                IssueKind::Call {
                                    target,
                                    failure: Failure::BuiltinValue,
                                },
                            )?;
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        } else {
                            readable.push(self.ctx, arm)?;
                        }
                    }
                    let value = self.facts.union(self.ctx, &readable.data)?;
                    if value == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    state.stack.push(self.ctx, Operand::local(value, slot))?;
                }
                Op::Unbound(name) => {
                    let target = self.calls.resolve(self.ctx, &self.program.members[name])?;
                    if target != Target::Undefined {
                        return self.incomplete(pc);
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
                    if let Some(captures) = &mut state.captures {
                        captures.shadow(self.ctx, slot)?;
                    }
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
                        self.issue(pc, IssueKind::MissingBlock)?;
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
                    let parameter = &self.function.params[index];
                    let mut supplied = state.snapshot(self.ctx)?;
                    let input = self.inputs[index];
                    let value = match input {
                        Input::Supplied(value) | Input::Either(value) => Some(value),
                        Input::Default => None,
                    };
                    if let Some(value) = value {
                        supplied.store(self.ctx, self.facts, parameter.slot, value)?;
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
                Op::Range(start, end, exclusive) => {
                    let end = end.then(|| state.stack.data.pop().unwrap().value);
                    let start = start.then(|| state.stack.data.pop().unwrap().value);
                    let mut known = true;
                    for value in start.into_iter().chain(end) {
                        known &= matches!(self.facts.node(value), super::facts::Node::Integer(_));
                        if self.facts.relation(self.ctx, value, Atom::Int.fact())?
                            == Relation::Rejected
                        {
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                            self.issue(pc, IssueKind::Range { value })?;
                        }
                    }
                    let value = if known {
                        let endpoint = |value: Option<Fact>| {
                            value.map(|value| {
                                let super::facts::Node::Integer(n) = self.facts.node(value) else {
                                    unreachable!()
                                };
                                *n
                            })
                        };
                        self.facts
                            .range(self.ctx, endpoint(start), endpoint(end), exclusive)?
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
                Op::Binary("===") => {
                    let target = state.stack.data.pop().unwrap();
                    let matcher = state.stack.data.pop().unwrap();
                    if let Some(edges) =
                        self.case_compare(&mut state, pc, Some(target), matcher, false)?
                    {
                        return Ok(edges);
                    }
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        let arguments = self.facts.tuple(self.ctx, &args.data)?;
                        self.issue(
                            pc,
                            IssueKind::Index {
                                receiver,
                                arguments,
                            },
                        )?;
                    }
                    if result.unsupported
                        || (self.facts.detached_builtin(result.value)
                            && !matches!(self.function.code.get(pc + 1), Some(Op::CallValue)))
                    {
                        return self.incomplete(pc);
                    }
                    if result.value == Atom::Never.fact() {
                        return Ok([None, None]);
                    }
                    state.stack.data.truncate(base);
                    state.stack.push(self.ctx, Operand::new(result.value))?;
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
                        self.transfer(state, pc, Transfer::InvalidRetry)
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                Op::AddressGlobal(index) => {
                    let (global, value) = &self.program.globals[index];
                    if self.calls.global(self.ctx, global.name())?
                        || !matches!(value.0, Kind::Hash(_))
                    {
                        return self.incomplete(pc);
                    }
                    let value = builtins::global(self.ctx, self.facts, value)?;
                    let mut address = Address::new(None, value);
                    address.supported = false;
                    state.addresses.push(self.ctx, address)?;
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
                            self.index_outcome(&state, pc, receiver, &args.data, &result)?
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
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                    let protection = address.protection(self.ctx, self.facts)?;
                    if protection != Attached::No {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        let selectors = self.facts.tuple(self.ctx, &address.selectors.data)?;
                        self.issue(
                            pc,
                            IssueKind::Write {
                                receiver: address.value,
                                selectors,
                                value: value.value,
                            },
                        )?;
                        if protection == Attached::Yes {
                            return Ok([None, None]);
                        }
                    }
                    if !address.supported {
                        return self.incomplete(pc);
                    }
                    let selectors = self.facts.tuple(self.ctx, &address.selectors.data)?;
                    let [key] = address.selectors.data.as_slice() else {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                    if args.block.is_some() {
                        return self.incomplete(pc);
                    }
                    let receiver = state.addresses.data.last().unwrap().value;
                    if !args.keywords.data.is_empty() {
                        let protection = state
                            .addresses
                            .data
                            .last()
                            .unwrap()
                            .protection(self.ctx, self.facts)?;
                        if protection != Attached::No {
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                            let arguments = self.facts.tuple(self.ctx, &args.positional.data)?;
                            self.issue(
                                pc,
                                IssueKind::Member {
                                    name: site.name,
                                    receiver,
                                    arguments,
                                },
                            )?;
                            if protection == Attached::Yes {
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
                    let mut protected_field = true;
                    for i in 0..self.facts.arm_count(receiver) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(receiver, i);
                        let super::facts::Node::Protected(shape, _) = self.facts.node(arm) else {
                            protected_field = false;
                            break;
                        };
                        if !self
                            .facts
                            .selected_field(self.ctx, *shape, name.as_bytes())?
                            .is_some_and(|(_, optional)| !optional)
                        {
                            protected_field = false;
                            break;
                        }
                    }
                    if protected_field {
                        let key = self.facts.string(self.ctx, name.as_bytes())?;
                        let result = state.addresses.data.last_mut().unwrap().index(
                            self.ctx,
                            self.facts,
                            &[key],
                        )?;
                        if let Some(edges) =
                            self.index_outcome(&state, pc, receiver, &[key], &result)?
                        {
                            return Ok(edges);
                        }
                        continue;
                    }
                    if builtins::value_member(self.ctx, self.facts, receiver, name)? {
                        state.addresses.data.pop().unwrap();
                        if let Some(edges) =
                            self.member(&mut state, pc, receiver, site, Arguments::new())?
                        {
                            return Ok(edges);
                        }
                        let value = state.stack.data.pop().unwrap().value;
                        state.addresses.push(self.ctx, Address::new(None, value))?;
                        continue;
                    }
                    let (mut fields, mut absent) = (false, false);
                    for i in 0..self.facts.arm_count(receiver) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(receiver, i);
                        match self.facts.node(arm) {
                            super::facts::Node::Protected(shape, _) => {
                                let selected =
                                    self.facts
                                        .selected_field(self.ctx, *shape, name.as_bytes())?;
                                fields |= selected.is_some();
                                absent |= selected.is_none();
                            }
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
                        if let Some(edges) =
                            self.index_outcome(&state, pc, receiver, &[key], &result)?
                        {
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
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                Op::CallMember(site) => {
                    let receiver = state.stack.data.pop().unwrap().value;
                    let name = &self.program.members[site.name];
                    if matches!(self.facts.node(receiver), super::facts::Node::Offset(_)) {
                        let target = Target::Offset(receiver);
                        self.issue(
                            pc,
                            IssueKind::Call {
                                target,
                                failure: Failure::BuiltinValue,
                            },
                        )?;
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        return Ok([None, None]);
                    }
                    if let super::facts::Node::Builtin(builtin) = self.facts.node(receiver) {
                        self.issue(
                            pc,
                            IssueKind::Call {
                                target: Target::Builtin(*builtin),
                                failure: Failure::BuiltinValue,
                            },
                        )?;
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        return Ok([None, None]);
                    }
                    let fields = if let super::facts::Node::Protected(shape, _) =
                        self.facts.node(receiver)
                    {
                        *shape
                    } else {
                        receiver
                    };
                    let protected = fields != receiver;
                    let field = if protected
                        || matches!(
                            self.facts.node(fields),
                            super::facts::Node::Shape(_, false, _, false)
                        ) {
                        self.facts
                            .selected_field(self.ctx, fields, name.as_bytes())?
                            .and_then(|(value, optional)| (!optional).then_some(value))
                    } else {
                        return self.incomplete(pc);
                    };
                    let Some(field) = field else {
                        if !site.scope
                            && (crate::members::hash_builtin(name)
                                || crate::members::names::universal(name))
                        {
                            return self.incomplete(pc);
                        }
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        self.issue(
                            pc,
                            IssueKind::Call {
                                target: Target::Undefined,
                                failure: Failure::Undefined,
                            },
                        )?;
                        return Ok([None, None]);
                    };
                    state.arguments.data.last_mut().unwrap().target = self.value_target(field)?;
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
                                    self.emit_error(
                                        &state,
                                        pc,
                                        handlers::bit(ErrorClass::Runtime),
                                    )?;
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
                            let value = if let super::facts::Node::Protected(shape, _) =
                                self.facts.node(operand.value)
                            {
                                *shape
                            } else {
                                operand.value
                            };
                            if let super::facts::Node::Shape(fields, false, _, _) =
                                self.facts.node(value)
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
                                    self.emit_error(
                                        &state,
                                        pc,
                                        handlers::bit(ErrorClass::Runtime),
                                    )?;
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
                    let (errors, stops) =
                        self.binary_errors(op, left.value, right.value, result.rejected)?;
                    self.emit_error(&state, pc, errors)?;
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
                    if stops {
                        return Ok([None, None]);
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
                    if !site.scope
                        && matches!(site.method, Some(Method::IsNil))
                        && !matches!(
                            self.facts.node(state.stack.data.last().unwrap().value),
                            super::facts::Node::TypeValue(_)
                        )
                        && !builtins::namespace(
                            self.ctx,
                            self.facts,
                            state.stack.data.last().unwrap().value,
                        )? =>
                {
                    let operand = state.stack.data.pop().unwrap();
                    // Nominal receivers may implement their own method; that dispatch is unfinished.
                    if !self.facts.known_nil_receiver(self.ctx, operand.value)? {
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
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
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
                        let iteration = self.facts.iteration(self.ctx, value)?;
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
                        if self.block_inputs.is_some() && !matches!(op, Op::LoopBody) {
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
                        return self.incomplete(pc);
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
                    state.stack.push(self.ctx, Operand::new(current.result))?;
                }
                Op::Return | Op::Finish => {
                    let actual = state.stack.data.pop().unwrap().value;
                    let transfer = if self.block_inputs.is_some() && matches!(op, Op::Return) {
                        Transfer::Block {
                            pc,
                            completion: blocks::Completion::Return,
                            value: actual,
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
