use super::{
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
    bytecode::{ArgumentOp, Function, Invocation, Method, Op, Program},
    value::Kind,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IssueKind {
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
}

impl Operand {
    fn new(value: Fact) -> Self {
        Self {
            value,
            origin: None,
            predicate: None,
            literal: None,
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

    fn join(self, ctx: &mut CallContext, facts: &mut Facts, other: Self) -> Result<Self> {
        Ok(Self {
            value: facts.union(ctx, &[self.value, other.value])?,
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
    expression: bool,
    last: Fact,
    result: Fact,
}

#[derive(Debug)]
struct State {
    locals: Slots<Binding>,
    stack: Buffer<Operand>,
    loops: Buffer<Loop>,
    arguments: Buffer<Pending>,
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
        }
    }

    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut state = Self {
            locals: self.locals.snapshot(ctx)?,
            stack: Buffer::empty(),
            loops: Buffer::empty(),
            arguments: Buffer::empty(),
        };
        state.stack.extend(ctx, &self.stack.data)?;
        state.loops.extend(ctx, &self.loops.data)?;
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
        Ok(state)
    }

    fn join(&mut self, ctx: &mut CallContext, facts: &mut Facts, other: &Self) -> Result<bool> {
        let mut changed = self.locals.merge(ctx, &other.locals, |ctx, a, b| {
            Ok(Binding {
                value: facts.union(ctx, &[a.value, b.value])?,
                missing: a.missing || b.missing,
            })
        })?;
        assert_eq!(self.stack.data.len(), other.stack.data.len());
        assert_eq!(self.loops.data.len(), other.loops.data.len());
        assert_eq!(self.arguments.data.len(), other.arguments.data.len());
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
            let next = a.join(ctx, facts, *b)?;
            changed |= *a != next;
            *a = next;
        }
        for (a, b) in self.loops.data.iter_mut().zip(&other.loops.data) {
            ctx.charge(1)?;
            assert!(
                a.base == b.base
                    && a.argument_base == b.argument_base
                    && a.expression == b.expression
            );
            let last = facts.union(ctx, &[a.last, b.last])?;
            let result = facts.union(ctx, &[a.result, b.result])?;
            changed |= a.last != last || a.result != result;
            a.last = last;
            a.result = result;
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
            let index = graph.at(walker.ctx, pc)?;
            let changed = if let Some(entry) = &mut entries.data[index] {
                entry.join(walker.ctx, walker.facts, &state)?
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
        if self.facts.known_primitive(self.ctx, value)? {
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

    fn store(&mut self, state: &mut State, pc: usize, slot: usize, value: Fact) -> Result<()> {
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
        state.store(self.ctx, slot, value)
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
                        Kind::Int(_) | Kind::Big(_) => Atom::Int.fact(),
                        Kind::Float(_) => Atom::Float.fact(),
                        Kind::Bytes(_) => Atom::String.fact(),
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
                        state.store(self.ctx, slot, value)?;
                    }
                }
                Op::Shadow(slot) => state.store(self.ctx, slot, Atom::Nil.fact())?,
                Op::Store(slot) => {
                    let value = state.stack.data.last().unwrap().value;
                    self.store(&mut state, pc, slot, value)?;
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
                    state.stack.push(self.ctx, Operand::new(value))?;
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
                    state.stack.push(self.ctx, Operand::new(value))?;
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
                            if let super::facts::Node::Shape(fields, false, _) =
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
                        self.store(&mut state, pc, slot, result.value)?;
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
                    iterable: false,
                    expression,
                    ..
                } => {
                    state.loops.push(
                        self.ctx,
                        Loop {
                            base: state.stack.data.len(),
                            argument_base: state.arguments.data.len(),
                            expression,
                            last: Atom::Nil.fact(),
                            result: Atom::Never.fact(),
                        },
                    )?;
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
                    let Exit::Jump(target) = block.exit else {
                        unreachable!()
                    };
                    return Ok([Some((target, state)), None]);
                }
                Op::LoopEnd => {
                    let current = state.loops.data.pop().unwrap();
                    state.stack.data.truncate(current.base);
                    state.arguments.data.truncate(current.argument_base);
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
