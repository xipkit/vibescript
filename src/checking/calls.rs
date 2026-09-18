use super::{
    arguments::{Arguments, Failure, Input},
    blocks,
    facts::{Atom, Callable, Fact, Facts},
    flow::{self, Issue, Report},
    globals::Globals,
    lexical::Layouts,
    relation::Relation,
};
use crate::{CallContext, Result, Value, budget::Buffer, bytecode::Program};
use std::hash::{DefaultHasher, Hash, Hasher};

mod context;
mod hosts;
mod initializers;
mod whole;
use context::{Context, Kind};
pub(super) use whole::analyze as analyze_whole;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// An admitted root value or a value selected before call arguments run.
    Value(Fact),
    /// A supplied global whose value has not been read.
    Deferred(usize),
    Builtin(crate::builtin::Builtin),
    Offset(Fact),
    Function(usize),
    Helper {
        receiver: Fact,
        name: &'static str,
        implicit: bool,
    },
    Method {
        function: usize,
        receiver: Fact,
        constructor: bool,
    },
    Block(usize),
    Host(usize),
    NonCallable,
    Undefined,
    Dynamic,
    Unsupported,
}

pub(super) struct Outcome {
    pub value: Fact,
    pub throws: u8,
    pub failures: Buffer<Failure>,
    pub incomplete: bool,
    pub exits: Buffer<blocks::Exit>,
}

pub(super) enum HostBoundary<'a> {
    Arguments(&'a Arguments),
    Result(Option<Fact>),
}

impl Outcome {
    /// Creates a call summary with no known exits or diagnostics.
    pub fn empty() -> Self {
        Self {
            value: Atom::Never.fact(),
            throws: 0,
            failures: Buffer::empty(),
            incomplete: false,
            exits: Buffer::empty(),
        }
    }
}

pub(super) trait Calls {
    /// Summarizes constructor fields for whole-file declaration roots without execution.
    fn receiver_fields(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: usize,
        _: &Globals,
    ) -> Result<Option<Fact>> {
        ctx.checkpoint()?;
        Ok(None)
    }
    /// Analyzes a namespace body with its declaring bindings and without a script block.
    fn initialize(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: &blocks::Closure,
        _: u16,
        _: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        Ok(Outcome {
            incomplete: true,
            ..Outcome::empty()
        })
    }
    /// Reports configured writer presence without retaining or invoking the callback.
    fn writer(&mut self, ctx: &mut CallContext, _: crate::output::Kind) -> Result<Option<bool>> {
        ctx.charge(1)?;
        Ok(None)
    }
    /// Describes a supplied root only when execution would materialize it.
    fn load_root(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: usize,
    ) -> Result<super::inputs::Loaded> {
        ctx.checkpoint()?;
        Ok(super::inputs::Loaded::unavailable())
    }
    /// Reports whether the selected host implementation may invoke its block.
    fn host_uses_block(&mut self, ctx: &mut CallContext, _: usize) -> Result<bool> {
        ctx.checkpoint()?;
        Ok(true)
    }
    /// Checks one host boundary without invoking callbacks or attached blocks.
    fn host_boundary(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: usize,
        _: HostBoundary<'_>,
        _: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        Ok(Outcome {
            incomplete: true,
            ..Outcome::empty()
        })
    }
    /// Copies admitted root facts without evaluating host code.
    fn roots(&mut self, ctx: &mut CallContext, _: &mut Facts) -> Result<Buffer<Root>> {
        ctx.checkpoint()?;
        Ok(Buffer::empty())
    }
    /// Resolves a bound method only in the world that owns its declaration.
    fn attached(&mut self, ctx: &mut CallContext, _: usize, _: Callable) -> Result<Target> {
        ctx.checkpoint()?;
        Ok(Target::Unsupported)
    }
    /// Records host bindings that may replace source type declarations.
    fn type_bindings(
        &mut self,
        ctx: &mut CallContext,
        _: &mut super::type_bindings::Bindings,
        _: super::type_bindings::Scope,
    ) -> Result<bool> {
        ctx.checkpoint()?;
        Ok(false)
    }
    fn global(&mut self, ctx: &mut CallContext, name: &str) -> Result<bool>;
    fn resolve(&mut self, ctx: &mut CallContext, name: &str) -> Result<Target>;
    fn invoke(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: Target,
        args: Arguments,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome>;
}

#[cfg(test)]
pub(super) struct Unavailable;

pub(super) struct Root {
    pub name: Value,
    pub value: Fact,
    pub missing: bool,
}

#[cfg(test)]
impl Calls for Unavailable {
    fn global(&mut self, ctx: &mut CallContext, _: &str) -> Result<bool> {
        ctx.checkpoint()?;
        Ok(false)
    }
    fn resolve(&mut self, ctx: &mut CallContext, _: &str) -> Result<Target> {
        ctx.checkpoint()?;
        Ok(Target::Unsupported)
    }
    fn invoke(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: Target,
        _: Arguments,
        _: u16,
        _: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        Ok(Outcome {
            value: Atom::Never.fact(),
            throws: 0,
            failures: Buffer::empty(),
            incomplete: true,
            exits: Buffer::empty(),
        })
    }
}

#[derive(Debug)]
pub(super) struct Host<'a> {
    pub params: Buffer<Option<Fact>>,
    pub required: usize,
    pub result: Fact,
    pub constrained: bool,
    pub unresolved: bool,
    accepts_block: bool,
    source: Option<&'a crate::signature::Compiled>,
    blocks: HostBlocks,
    granted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostBlocks {
    Possible,
    Ignored,
    Rejected,
}

impl<'a> Host<'a> {
    /// Reads a compiled registration without constructing a grant or invoking host code.
    pub fn registered(
        ctx: &mut CallContext,
        facts: &mut Facts,
        value: &'a crate::capability::Registered,
    ) -> Result<Self> {
        match value {
            crate::capability::Registered::Callback(_) => {
                let mut host = Self::new(ctx, facts, None)?;
                host.blocks = HostBlocks::Ignored;
                Ok(host)
            }
            crate::capability::Registered::Method(method) => Self::method(
                ctx,
                facts,
                method.compiled_signature(),
                method.supports_block(),
            ),
        }
    }

    /// Describes a supplied descriptor, including whether its grant is reusable.
    pub fn bound(
        ctx: &mut CallContext,
        facts: &mut Facts,
        value: &'a crate::capability::BoundMethod,
    ) -> Result<Self> {
        let mut host = Self::method(ctx, facts, value.signature(), value.supports_block())?;
        host.granted = value.fresh_grant();
        Ok(host)
    }

    fn method(
        ctx: &mut CallContext,
        facts: &mut Facts,
        signature: Option<&'a crate::signature::Compiled>,
        supports_block: bool,
    ) -> Result<Self> {
        let mut host = Self::new(ctx, facts, signature)?;
        host.blocks = if supports_block {
            HostBlocks::Possible
        } else if signature.is_some() {
            HostBlocks::Ignored
        } else {
            HostBlocks::Rejected
        };
        Ok(host)
    }

    /// Borrows metadata so named contracts use each call's current root and source bindings.
    pub fn new(
        ctx: &mut CallContext,
        facts: &mut Facts,
        signature: Option<&'a crate::signature::Compiled>,
    ) -> Result<Self> {
        let mut host = Self::resolved(ctx, facts, signature, |_, _| Ok(None))?;
        host.source = signature;
        Ok(host)
    }

    /// Builds host type facts against an explicitly supplied binding snapshot.
    pub fn resolved(
        ctx: &mut CallContext,
        facts: &mut Facts,
        signature: Option<&crate::signature::Compiled>,
        mut resolve: impl FnMut(&mut CallContext, &str) -> Result<Option<Fact>>,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let mut host = Self {
            params: Buffer::empty(),
            required: 0,
            result: Atom::Unknown.fact(),
            constrained: signature.is_some(),
            unresolved: false,
            accepts_block: signature.is_some_and(|s| s.source.accepts_block),
            source: None,
            blocks: HostBlocks::Possible,
            granted: true,
        };
        if let Some(signature) = signature {
            host.required = signature.required;
            for param in &signature.params {
                ctx.charge(1)?;
                let fact = param
                    .as_ref()
                    .map(|ty| facts.annotation(ctx, ty, &mut resolve))
                    .transpose()?;
                host.unresolved |= fact.is_some_and(|fact| facts.unresolved(fact));
                host.params.push(ctx, fact)?;
            }
            if let Some(ty) = &signature.result {
                host.result = facts.annotation(ctx, ty, &mut resolve)?;
            }
            host.unresolved |= facts.unresolved(host.result);
        }
        Ok(host)
    }
}

pub(super) struct World<'a> {
    pub program: &'a Program,
    /// Identity owner used when constructing the source declaration contracts.
    pub source_owner: usize,
    pub contracts: &'a [Fact],
    pub hosts: &'a [Host<'a>],
    // Callers supply unique names and admitted facts or bound descriptors, never factories.
    pub globals: &'a [(Value, Target)],
    pub inputs: &'a [&'a Value],
}

#[derive(Debug)]
pub(super) struct LocatedIssue {
    pub function: usize,
    pub issue: Issue,
}

#[derive(Debug)]
pub(super) struct Analysis {
    #[cfg(test)]
    pub returns: Fact,
    #[cfg(test)]
    pub throws: u8,
    pub issues: Buffer<LocatedIssue>,
    pub incomplete: Buffer<(usize, usize)>,
    #[cfg(test)]
    pub contexts: usize,
}

struct Job {
    function: usize,
    error_key: u16,
    current_error: u16,
    // The cache key stays exact even when recursive analysis needs broader inputs.
    inputs: Buffer<Input>,
    widened: Option<Buffer<Input>>,
    context: Context,
    widened_context: Option<Context>,
    input_depth: Option<usize>,
    return_depth: Option<usize>,
    cyclic: bool,
    visited: usize,
    hash: u64,
    next: usize,
    parents: Buffer<usize>,
    dependencies: Buffer<usize>,
    queued: bool,
    returns: Fact,
    throws: u8,
    report: Option<Report>,
}

struct Solver<'a> {
    whole: bool,
    world: World<'a>,
    values: super::inputs::Values<'a>,
    layouts: &'a Layouts,
    jobs: Buffer<Job>,
    buckets: Buffer<usize>,
    queue: Buffer<usize>,
    current: usize,
    dependencies: Buffer<usize>,
    functions: Buffer<bool>,
    search: usize,
    entry_failures: Buffer<Failure>,
}

enum Ancestor<'a> {
    Function(usize, &'a Context),
    Expanding(usize, &'a Context),
    Job(usize),
}

const EMPTY: usize = usize::MAX;

#[cfg(test)]
pub(super) fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'_>,
    function: usize,
    inputs: &[Input],
) -> Result<Analysis> {
    analyze_with_values(
        ctx,
        facts,
        world,
        function,
        inputs,
        super::inputs::Values::new(),
        &[],
    )
}

/// Retains callable metadata discovered while importing concrete entry arguments.
pub(super) fn analyze_with_values<'a>(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'a>,
    function: usize,
    inputs: &[Input],
    values: super::inputs::Values<'a>,
    failures: &[Failure],
) -> Result<Analysis> {
    analyze_entry(
        ctx,
        facts,
        world,
        Entry {
            function,
            inputs,
            failures,
            general: false,
            constructor: false,
            scope: blocks::Scope::Invocation,
        },
        values,
    )
}

#[derive(Clone, Copy)]
pub(super) struct General {
    pub function: usize,
    pub constructor: bool,
    pub scope: blocks::Scope,
}

pub(super) fn analyze_general<'a>(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'a>,
    entry: General,
    values: super::inputs::Values<'a>,
) -> Result<Analysis> {
    let General {
        function,
        constructor,
        scope,
    } = entry;
    let inputs = super::arguments::general_inputs(
        ctx,
        facts,
        &world.program.functions[function].params,
        world.contracts,
    )?;
    analyze_entry(
        ctx,
        facts,
        world,
        Entry {
            function,
            inputs: &inputs.data,
            failures: &[],
            general: true,
            constructor,
            scope,
        },
        values,
    )
}

struct Entry<'a> {
    function: usize,
    inputs: &'a [Input],
    failures: &'a [Failure],
    general: bool,
    constructor: bool,
    scope: blocks::Scope,
}

fn analyze_entry<'a>(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'a>,
    entry: Entry<'_>,
    values: super::inputs::Values<'a>,
) -> Result<Analysis> {
    let Entry {
        function,
        inputs,
        failures,
        general,
        constructor,
        scope,
    } = entry;
    ctx.checkpoint()?;
    let mut admitted = Buffer::empty();
    let mut admission_issues = Buffer::empty();
    let mut rejected = false;
    for &input in inputs {
        ctx.charge(1)?;
        let mut next = input;
        if let Input::Supplied(value) | Input::Either(value) = input {
            if facts.escapes(value) {
                admission_issues.push(
                    ctx,
                    LocatedIssue {
                        function,
                        issue: Issue {
                            pc: 0,
                            kind: flow::IssueKind::DetachedValue(Target::Value(value)),
                        },
                    },
                )?;
                let value = facts.exported(ctx, value)?;
                next = match input {
                    Input::Supplied(_) => {
                        rejected |= value == Atom::Never.fact();
                        Input::Supplied(value)
                    }
                    Input::Either(_) if value == Atom::Never.fact() => Input::Default,
                    Input::Either(_) => Input::Either(value),
                    Input::Default => unreachable!(),
                };
            }
        }
        admitted.push(ctx, next)?;
    }
    #[cfg(test)]
    let admission_throws = if admission_issues.data.is_empty() {
        0
    } else {
        1 << crate::ErrorClass::Runtime as u8
    };
    if rejected {
        return Ok(Analysis {
            #[cfg(test)]
            returns: Atom::Never.fact(),
            #[cfg(test)]
            throws: admission_throws,
            issues: admission_issues,
            incomplete: Buffer::empty(),
            #[cfg(test)]
            contexts: 0,
        });
    }
    let inputs = &admitted.data;
    let mut functions = Buffer::with_capacity(ctx, world.program.functions.len())?;
    ctx.charge(world.program.functions.len() as u64)?;
    functions.data.resize(world.program.functions.len(), false);
    let layouts = Layouts::new(ctx, world.program, world.source_owner)?;
    let mut solver = Solver {
        whole: false,
        world,
        values,
        layouts: &layouts,
        jobs: Buffer::empty(),
        buckets: Buffer::empty(),
        queue: Buffer::empty(),
        current: EMPTY,
        dependencies: Buffer::empty(),
        functions,
        search: 0,
        entry_failures: Buffer::empty(),
    };
    solver.entry_failures.extend(ctx, failures)?;
    let mut context = Context::plain();
    context.constructor = constructor;
    context.scope = scope;
    ctx.charge(solver.world.program.namespaces.len() as u64)?;
    if (function != 0 || solver.world.program.file)
        && solver
            .world
            .program
            .namespaces
            .iter()
            .any(|namespace| namespace.body.is_some())
    {
        context.kind = Kind::Entry { general };
    } else if general {
        context.kind = Kind::General;
    }
    context.globals = Globals::initial(ctx, facts, solver.world.program)?;
    let roots = solver.roots(ctx, facts)?;
    context.globals.roots(ctx, &roots.data)?;
    context
        .globals
        .namespaces(ctx, facts, solver.world.program, solver.world.source_owner)?;
    let entry = solver.request(ctx, facts, function, inputs, flow::NO_ERROR, &context)?;
    solver.solve(ctx, facts)?;
    let result = Analysis {
        #[cfg(test)]
        returns: solver.jobs.data[entry].returns,
        #[cfg(test)]
        throws: solver.jobs.data[entry].throws | admission_throws,
        issues: admission_issues,
        incomplete: Buffer::empty(),
        #[cfg(test)]
        contexts: solver.jobs.data.len(),
    };
    solver.collect(ctx, &[entry], result)
}

impl Solver<'_> {
    fn solve(&mut self, ctx: &mut CallContext, facts: &mut Facts) -> Result<()> {
        while let Some(index) = self.queue.data.pop() {
            ctx.charge(1)?;
            self.current = index;
            self.jobs.data[index].queued = false;
            self.dependencies = Buffer::empty();
            let mut inputs = Buffer::empty();
            let job = &self.jobs.data[index];
            inputs.extend(ctx, &job.widened.as_ref().unwrap_or(&job.inputs).data)?;
            let context = job
                .widened_context
                .as_ref()
                .unwrap_or(&job.context)
                .snapshot(ctx)?;
            let incoming = context.incoming(ctx)?;
            let block = if matches!(context.kind, Kind::Invoked { .. } | Kind::Initializing) {
                Some(blocks::Inputs {
                    arguments: &context.arguments.data,
                    captures: &context.captures.data,
                    pending: &context.pending,
                    given: matches!(context.kind, Kind::Invoked { given: true }),
                    inherited: &context.inherited.data,
                })
            } else {
                None
            };
            let function = self.jobs.data[index].function;
            let body = flow::Body {
                scope: context.scope,
                ambient: context.ambient,
                general: context.kind == Kind::General,
                receiver: context.receiver,
                constructor: context.constructor,
                program: self.world.program,
                contracts: self.world.contracts,
                function,
                inputs: &inputs.data,
                current_error: self.jobs.data[index].current_error,
                block: block.as_ref(),
                incoming: incoming.as_ref(),
                layouts: Some(self.layouts),
                globals: Some(&context.globals),
            };
            let mut report = if let Kind::Entry { general } = context.kind {
                self.initialize_entry(ctx, facts, function, &inputs.data, &context, general)?
            } else {
                flow::analyze_body(ctx, facts, body, self)?
            };
            let mut returns = report.normal_returns;
            let previous = self.jobs.data[index].returns;
            if self.jobs.data[index].cyclic && previous != Atom::Never.fact() && previous != returns
            {
                let depth = *self.jobs.data[index]
                    .return_depth
                    .get_or_insert(facts.max_depth());
                returns = facts.widen(ctx, previous, returns, depth)?;
            }
            let job = &mut self.jobs.data[index];
            if job.cyclic {
                if let Some(previous) = &job.report {
                    let depth = *job.return_depth.get_or_insert(facts.max_depth());
                    for exit in &mut report.block_exits.data {
                        for before in &previous.block_exits.data {
                            ctx.charge(1)?;
                            if exit.pc == before.pc && exit.completion == before.completion {
                                exit.widen(ctx, facts, before, depth)?;
                            }
                        }
                    }
                }
            }
            let mut changed = job.returns != returns || job.throws != report.throws;
            if let Some(previous) = &job.report {
                changed |= previous.block_exits.data.len() != report.block_exits.data.len();
                for (a, b) in previous
                    .block_exits
                    .data
                    .iter()
                    .zip(&report.block_exits.data)
                {
                    changed |= !a.equal(ctx, b)?;
                }
            } else {
                changed |= !report.block_exits.data.is_empty();
            }
            job.returns = returns;
            job.throws = report.throws;
            job.report = Some(report);
            job.dependencies = std::mem::replace(&mut self.dependencies, Buffer::empty());
            if changed {
                for parent in 0..self.jobs.data[index].parents.data.len() {
                    ctx.charge(1)?;
                    let parent = self.jobs.data[index].parents.data[parent];
                    self.enqueue(ctx, parent)?;
                }
            }
        }
        self.current = EMPTY;
        self.dependencies = Buffer::empty();
        Ok(())
    }

    fn collect(
        &mut self,
        ctx: &mut CallContext,
        entries: &[usize],
        mut result: Analysis,
    ) -> Result<Analysis> {
        let mut reached = Buffer::with_capacity(ctx, self.jobs.data.len())?;
        for _ in &self.jobs.data {
            ctx.charge(1)?;
            reached.data.push(false);
        }
        self.queue.extend(ctx, entries)?;
        while let Some(index) = self.queue.data.pop() {
            ctx.charge(1)?;
            if reached.data[index] {
                continue;
            }
            reached.data[index] = true;
            let job = &self.jobs.data[index];
            self.queue.extend(ctx, &job.dependencies.data)?;
            let report = job.report.as_ref().unwrap();
            for &issue in &report.issues.data {
                ctx.charge(result.issues.data.len() as u64)?;
                if !result
                    .issues
                    .data
                    .iter()
                    .any(|old| old.function == job.function && old.issue == issue)
                {
                    result.issues.push(
                        ctx,
                        LocatedIssue {
                            function: job.function,
                            issue,
                        },
                    )?;
                }
            }
            for &pc in &report.incomplete.data {
                ctx.charge(result.incomplete.data.len() as u64)?;
                if !result.incomplete.data.contains(&(job.function, pc)) {
                    result.incomplete.push(ctx, (job.function, pc))?;
                }
            }
        }
        ctx.charge(
            result
                .issues
                .data
                .len()
                .saturating_mul(result.issues.data.len().max(1).ilog2() as usize + 1)
                as u64,
        )?;
        result
            .issues
            .data
            .sort_unstable_by_key(|issue| (issue.function, issue.issue.pc));
        ctx.charge(
            result
                .incomplete
                .data
                .len()
                .saturating_mul(result.incomplete.data.len().max(1).ilog2() as usize + 1)
                as u64,
        )?;
        result.incomplete.data.sort_unstable();
        ctx.checkpoint()?;
        Ok(result)
    }

    fn enqueue(&mut self, ctx: &mut CallContext, index: usize) -> Result<()> {
        if !self.jobs.data[index].queued {
            self.queue.push(ctx, index)?;
            self.jobs.data[index].queued = true;
        }
        Ok(())
    }

    fn request(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        function: usize,
        inputs: &[Input],
        current_error: u16,
        context: &Context,
    ) -> Result<usize> {
        ctx.charge(inputs.len() as u64 + 1)?;
        let mut hash = DefaultHasher::new();
        function.hash(&mut hash);
        inputs.hash(&mut hash);
        current_error.hash(&mut hash);
        context.hash(ctx, &mut hash)?;
        let hash = hash.finish();
        if !self.buckets.data.is_empty() {
            let mut index = self.buckets.data[hash as usize & (self.buckets.data.len() - 1)];
            while index != EMPTY {
                ctx.charge(inputs.len() as u64 + 1)?;
                let job = &self.jobs.data[index];
                if job.hash == hash
                    && job.function == function
                    && job.inputs.data == inputs
                    && job.error_key == current_error
                    && job.context.equal(ctx, context)?
                {
                    return Ok(index);
                }
                index = job.next;
            }
        }
        if self.functions.data[function] {
            if let Some(path) = self.ancestor(ctx, Ancestor::Function(function, context))? {
                let index = path.data[0];
                self.cycle(ctx, &path.data)?;
                let depth = *self.jobs.data[index]
                    .input_depth
                    .get_or_insert(facts.max_depth());
                let mut joined = Buffer::empty();
                let mut changed = self.jobs.data[index].current_error | current_error
                    != self.jobs.data[index].current_error;
                self.jobs.data[index].current_error |= current_error;
                let mut widened_context = self.jobs.data[index]
                    .widened_context
                    .as_ref()
                    .unwrap_or(&self.jobs.data[index].context)
                    .snapshot(ctx)?;
                changed |= widened_context.widen(ctx, facts, context, depth)?;
                for (i, &incoming) in inputs.iter().enumerate() {
                    ctx.charge(1)?;
                    let job = &self.jobs.data[index];
                    let before = job.widened.as_ref().unwrap_or(&job.inputs).data[i];
                    let value = before.widen(ctx, facts, incoming, depth)?;
                    changed |= before != value;
                    joined.push(ctx, value)?;
                }
                if changed {
                    self.jobs.data[index].widened = Some(joined);
                    self.jobs.data[index].widened_context = Some(widened_context);
                    self.enqueue(ctx, index)?;
                }
                return Ok(index);
            }
        }
        if self.jobs.data.len() >= self.buckets.data.len() / 2 {
            let Some(length) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(crate::ErrorKind::Memory, "checker call table size overflow");
            };
            let mut buckets = Buffer::with_capacity(ctx, length)?;
            for _ in 0..length {
                ctx.charge(1)?;
                buckets.data.push(EMPTY);
            }
            for (index, job) in self.jobs.data.iter_mut().enumerate() {
                ctx.charge(1)?;
                let bucket = job.hash as usize & (length - 1);
                job.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let mut copied = Buffer::empty();
        copied.extend(ctx, inputs)?;
        let context = context.snapshot(ctx)?;
        let mut parents = Buffer::empty();
        // A newly created context cannot close a cycle. Record its first edge directly.
        if self.current != EMPTY {
            parents.push(ctx, self.current)?;
        }
        let index = self.jobs.data.len();
        let bucket = hash as usize & (self.buckets.data.len() - 1);
        self.jobs.push(
            ctx,
            Job {
                function,
                error_key: current_error,
                current_error,
                inputs: copied,
                widened: None,
                context,
                widened_context: None,
                input_depth: None,
                return_depth: None,
                cyclic: false,
                visited: 0,
                hash,
                next: self.buckets.data[bucket],
                parents,
                dependencies: Buffer::empty(),
                queued: false,
                returns: Atom::Never.fact(),
                throws: 0,
                report: None,
            },
        )?;
        self.buckets.data[bucket] = index;
        self.functions.data[function] = true;
        self.enqueue(ctx, index)?;
        Ok(index)
    }

    fn ancestor(
        &mut self,
        ctx: &mut CallContext,
        target: Ancestor<'_>,
    ) -> Result<Option<Buffer<usize>>> {
        ctx.checkpoint()?;
        if self.current == EMPTY {
            return Ok(None);
        }
        self.search = self.search.wrapping_add(1);
        if self.search == 0 {
            for job in &mut self.jobs.data {
                ctx.charge(1)?;
                job.visited = 0;
            }
            self.search = 1;
        }
        let mut pending = Buffer::empty();
        pending.push(ctx, (self.current, EMPTY))?;
        self.jobs.data[self.current].visited = self.search;
        let mut cursor = 0;
        while cursor < pending.data.len() {
            ctx.charge(1)?;
            let index = pending.data[cursor].0;
            let matched = match target {
                Ancestor::Function(function, context) => {
                    self.jobs.data[index].function == function
                        && self.jobs.data[index].context.globals.same_initialization(
                            ctx,
                            self.world.program,
                            &context.globals,
                        )?
                        && self.jobs.data[index].context.compatible(ctx, context)?
                }
                Ancestor::Expanding(function, context) => {
                    self.jobs.data[index].function == function
                        && self.jobs.data[index].context.expands(ctx, context)?
                }
                Ancestor::Job(job) => index == job,
            };
            if matched {
                let mut path = Buffer::empty();
                while cursor != EMPTY {
                    ctx.charge(1)?;
                    let (index, parent) = pending.data[cursor];
                    path.push(ctx, index)?;
                    cursor = parent;
                }
                return Ok(Some(path));
            }
            for i in 0..self.jobs.data[index].parents.data.len() {
                ctx.charge(1)?;
                let parent = self.jobs.data[index].parents.data[i];
                if self.jobs.data[parent].visited != self.search {
                    self.jobs.data[parent].visited = self.search;
                    pending.push(ctx, (parent, cursor))?;
                }
            }
            cursor += 1;
        }
        Ok(None)
    }

    fn cycle(&mut self, ctx: &mut CallContext, path: &[usize]) -> Result<()> {
        for &index in path {
            ctx.charge(1)?;
            self.jobs.data[index].cyclic = true;
        }
        Ok(())
    }
}

impl Calls for Solver<'_> {
    fn receiver_fields(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        module: usize,
        globals: &Globals,
    ) -> Result<Option<Fact>> {
        self.constructor_fields(ctx, facts, module, globals)
    }

    fn initialize(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        body: &blocks::Closure,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        self.initialize_body(ctx, facts, body, current_error, globals)
    }

    fn writer(&mut self, ctx: &mut CallContext, kind: crate::output::Kind) -> Result<Option<bool>> {
        ctx.charge(1)?;
        Ok(self
            .values
            .writers
            .map(|writers| writers[usize::from(kind == crate::output::Kind::Warn)]))
    }
    fn load_root(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
    ) -> Result<super::inputs::Loaded> {
        let Some((_, Target::Deferred(input))) = self.world.globals.get(index) else {
            return Ok(super::inputs::Loaded::unavailable());
        };
        self.values.read(ctx, facts, &self.world, *input)
    }

    fn host_uses_block(&mut self, ctx: &mut CallContext, index: usize) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self
            .values
            .host(&self.world, index)
            .is_some_and(|host| host.blocks == HostBlocks::Possible))
    }

    fn host_boundary(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        boundary: HostBoundary<'_>,
        globals: &Globals,
    ) -> Result<Outcome> {
        let mut outcome = Outcome::empty();
        match boundary {
            HostBoundary::Arguments(args) => {
                self.host_arguments(ctx, facts, index, args, globals, &mut outcome)?
            }
            HostBoundary::Result(value) => {
                self.host_result(ctx, facts, index, value, globals, &mut outcome)?
            }
        }
        Ok(outcome)
    }

    fn roots(&mut self, ctx: &mut CallContext, facts: &mut Facts) -> Result<Buffer<Root>> {
        let mut roots = Buffer::empty();
        for (name, target) in self.world.globals {
            ctx.charge(1)?;
            let value = match *target {
                Target::Value(value) => Some(value),
                Target::Deferred(_) => Some(Atom::Never.fact()),
                Target::Builtin(builtin) => Some(facts.builtin(ctx, builtin)?),
                Target::Host(index) => {
                    Some(facts.callable(ctx, self.world.source_owner, Callable::Host(index))?)
                }
                Target::Function(index) => {
                    Some(facts.callable(ctx, self.world.source_owner, Callable::Function(index))?)
                }
                _ => None,
            };
            if let Some(value) = value {
                roots.push(
                    ctx,
                    Root {
                        name: name.clone(),
                        value,
                        missing: matches!(target, Target::Deferred(_)),
                    },
                )?;
            }
        }
        Ok(roots)
    }
    fn attached(
        &mut self,
        ctx: &mut CallContext,
        owner: usize,
        target: Callable,
    ) -> Result<Target> {
        ctx.charge(1)?;
        Ok(if owner != self.world.source_owner {
            Target::Unsupported
        } else {
            match target {
                Callable::Host(index) if self.values.host(&self.world, index).is_some() => {
                    Target::Host(index)
                }
                Callable::Function(index) if index < self.world.program.functions.len() => {
                    Target::Function(index)
                }
                _ => Target::Unsupported,
            }
        })
    }
    fn type_bindings(
        &mut self,
        ctx: &mut CallContext,
        bindings: &mut super::type_bindings::Bindings,
        scope: super::type_bindings::Scope,
    ) -> Result<bool> {
        ctx.checkpoint()?;
        for (name, _) in self.world.globals {
            bindings.insert(
                ctx,
                scope,
                name.as_bytes().unwrap(),
                super::type_bindings::Binding::Unknown,
            )?;
        }
        Ok(!self.world.globals.is_empty())
    }
    fn global(&mut self, ctx: &mut CallContext, name: &str) -> Result<bool> {
        for (key, _) in self.world.globals {
            let key = key.as_bytes().unwrap();
            ctx.work_bytes(key.len().max(name.len()))?;
            if key == name.as_bytes() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn resolve(&mut self, ctx: &mut CallContext, name: &str) -> Result<Target> {
        for (key, target) in self.world.globals {
            let key = key.as_bytes().unwrap();
            ctx.work_bytes(key.len().max(name.len()))?;
            if key == name.as_bytes() {
                return Ok(*target);
            }
        }
        ctx.work_bytes(name.len())?;
        let program = self.world.program;
        if program.declaration_names.contains_key(name) {
            return Ok(Target::NonCallable);
        }
        if let Some(&index) = program.names.get(name) {
            return Ok(Target::Function(index));
        }
        for (index, host) in program.hosts.iter().enumerate() {
            ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                return Ok(Target::Host(index));
            }
        }
        for (global, value) in &program.globals {
            ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                return Ok(if let crate::value::Kind::Builtin(builtin) = value.0 {
                    Target::Builtin(builtin)
                } else {
                    Target::NonCallable
                });
            }
        }
        Ok(Target::Undefined)
    }

    fn invoke(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: Target,
        mut args: Arguments,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        let mut outcome = Outcome {
            value: Atom::Never.fact(),
            throws: 0,
            failures: Buffer::empty(),
            incomplete: false,
            exits: Buffer::empty(),
        };
        if !args.admit(ctx, facts, &mut outcome.failures)? {
            return Ok(outcome);
        }
        if let Target::Helper { receiver, name, .. } = target {
            let site = crate::bytecode::CallSite {
                name: usize::MAX,
                method: None,
                auto: false,
                scope: false,
                parenthesized: true,
            };
            return Ok(
                super::builtins::member(ctx, facts, receiver, site, name, &args)?
                    .expect("known instance helper"),
            );
        }
        if args.block.is_some()
            && !matches!(
                target,
                Target::Function(_)
                    | Target::Method { .. }
                    | Target::Block(_)
                    | Target::Host(_)
                    | Target::Undefined
                    | Target::NonCallable
            )
        {
            outcome.incomplete = true;
            return Ok(outcome);
        }
        match target {
            Target::Builtin(builtin) => return super::builtins::invoke(ctx, facts, builtin, &args),
            Target::Offset(value) => {
                return super::builtins::protected::invoke(ctx, facts, value, &args);
            }
            Target::Function(function)
            | Target::Block(function)
            | Target::Method { function, .. } => {
                let mut context = args
                    .block
                    .as_ref()
                    .map(|b| Context::receiving(ctx, b))
                    .transpose()?
                    .unwrap_or_else(Context::plain);
                context.globals = globals.snapshot(ctx)?;
                if let Target::Method {
                    receiver,
                    constructor,
                    ..
                } = target
                {
                    context.receiver = Some(receiver);
                    context.constructor = constructor;
                }
                let inputs = if matches!(target, Target::Block(_)) {
                    context.scope = context.block_scope;
                    context.receiver = context.block_receiver;
                    context.ambient = context.block_ambient;
                    context.kind = Kind::Invoked {
                        given: args.block.as_ref().unwrap().given,
                    };
                    context.arguments = args.positional;
                    Buffer::empty()
                } else {
                    let bound =
                        args.bind(ctx, facts, &self.world.program.functions[function].params)?;
                    if !bound.failures.data.is_empty() {
                        outcome.failures.extend(ctx, &bound.failures.data)?;
                        return Ok(outcome);
                    }
                    bound.inputs
                };
                if (!context.inherited.data.is_empty()
                    || !globals.pending.addresses.data.is_empty())
                    && self.functions.data[function]
                    && self
                        .ancestor(ctx, Ancestor::Expanding(function, &context))?
                        .is_some()
                {
                    outcome.incomplete = true;
                    return Ok(outcome);
                }
                let index =
                    self.request(ctx, facts, function, &inputs.data, current_error, &context)?;
                self.depend(ctx, index)?;
                outcome.value = self.jobs.data[index].returns;
                if context.kind == Kind::Plain && globals.values.data.is_empty() {
                    outcome.throws = self.jobs.data[index].throws;
                } else if let Some(report) = &self.jobs.data[index].report {
                    for exit in &report.block_exits.data {
                        let exit = exit.snapshot(ctx)?;
                        outcome.exits.push(ctx, exit)?;
                    }
                }
            }
            Target::Host(index) => {
                self.host_call(ctx, facts, index, &args, globals, &mut outcome)?;
            }
            Target::Dynamic => {
                outcome.value = Atom::Unknown.fact();
                outcome.throws = u8::MAX;
            }
            Target::Unsupported
            | Target::Value(_)
            | Target::Deferred(_)
            | Target::Helper { .. } => outcome.incomplete = true,
            Target::NonCallable => outcome.failures.push(ctx, Failure::NonCallable)?,
            Target::Undefined => outcome.failures.push(ctx, Failure::Undefined)?,
        }
        Ok(outcome)
    }
}
