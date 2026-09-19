use super::{
    arguments::{Arguments, Failure, Input},
    blocks,
    facts::{Atom, Callable, Fact, Facts},
    flow::{self, Issue, Report},
    globals::Globals,
    lexical::Layouts,
    relation::Relation,
    sources::{CallableId, SourceId},
};
use crate::{CallContext, Result, Value, budget::Buffer, bytecode::Program};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

mod captured;
mod context;
mod dispatch;
mod hosts;
mod initializers;
mod requires;
#[cfg(test)]
mod source_tests;
mod whole;
mod worlds;
use context::{Context, Kind};
use hosts::HostTarget;
pub(super) use requires::{Request as Require, failure as require_failure};
pub(super) use whole::analyze as analyze_whole;
use worlds::{Handle, Registry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// An admitted root value or a value selected before call arguments run.
    Value(Fact),
    /// A supplied global whose value has not been read.
    Deferred(usize),
    Builtin(crate::builtin::Builtin),
    Offset(Fact),
    Function(CallableId),
    Helper {
        receiver: Fact,
        name: &'static str,
        implicit: bool,
    },
    Method {
        function: CallableId,
        receiver: Fact,
        constructor: bool,
    },
    Block(CallableId),
    Host(CallableId),
    NonCallable,
    Undefined,
    Dynamic,
    Unsupported,
}

impl Target {
    /// Identifies the source whose callable metadata this target references.
    pub fn source(self) -> Option<SourceId> {
        match self {
            Self::Function(id)
            | Self::Block(id)
            | Self::Host(id)
            | Self::Method { function: id, .. } => Some(id.source),
            _ => None,
        }
    }
}

pub(super) struct Outcome {
    pub value: Fact,
    pub throws: u8,
    pub failures: Buffer<Failure>,
    pub incomplete: bool,
    pub pending: Option<usize>,
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
            pending: None,
            exits: Buffer::empty(),
        }
    }
}

pub(super) struct Annotation {
    pub expected: Fact,
    pub resolution: super::type_bindings::Resolution,
    pub throws: u8,
}

pub(super) trait Calls {
    /// Reads a declaration from the script receiving a required file.
    fn receiving_declaration(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: &str,
        _: &Globals,
    ) -> Result<Option<Fact>> {
        ctx.checkpoint()?;
        Ok(None)
    }

    /// Adds receiving declarations after the defining file's own type scopes.
    fn receiving_types(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: &Globals,
        _: &mut super::type_bindings::Bindings,
    ) -> Result<Option<super::type_bindings::Scope>> {
        ctx.checkpoint()?;
        Ok(None)
    }

    /// Resolves a property annotation in its defining source and current invocation state.
    fn annotation(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: SourceId,
        _: usize,
        _: &Globals,
    ) -> Result<Annotation> {
        ctx.checkpoint()?;
        Ok(Annotation {
            expected: Atom::Unknown.fact(),
            resolution: super::type_bindings::Resolution::Dynamic,
            throws: 0,
        })
    }
    /// Loads a reachable source and summarizes its invocation-local initialization.
    fn require(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: &Require,
        _: u16,
        _: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        Ok(Outcome {
            incomplete: true,
            ..Outcome::empty()
        })
    }
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
    /// Admits a deferred root and follows any captured source initialization.
    fn admit_root(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        root: usize,
        _: u16,
        _: &Globals,
    ) -> Result<Outcome> {
        let loaded = self.load_root(ctx, facts, root)?;
        Ok(Outcome {
            value: loaded.value,
            throws: loaded.throws,
            incomplete: loaded.incomplete,
            ..Outcome::empty()
        })
    }
    /// Reports whether the selected host implementation may invoke its block.
    fn host_uses_block(&mut self, ctx: &mut CallContext, _: CallableId) -> Result<bool> {
        ctx.checkpoint()?;
        Ok(true)
    }
    /// Checks one host boundary without invoking callbacks or attached blocks.
    fn host_boundary(
        &mut self,
        ctx: &mut CallContext,
        _: &mut Facts,
        _: CallableId,
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
    /// Resolves a bound method through the prepared world that owns its declaration.
    fn attached(&mut self, ctx: &mut CallContext, _: usize, _: Callable) -> Result<Target> {
        ctx.checkpoint()?;
        Ok(Target::Unsupported)
    }
    /// Reads the defining declaration before deciding whether a bare read can auto-call it.
    fn function_arity(&mut self, ctx: &mut CallContext, _: CallableId) -> Result<Option<usize>> {
        ctx.checkpoint()?;
        Ok(None)
    }
    /// Finds a receiving script declaration after an imported file's private bindings.
    fn receiving_binding(&mut self, ctx: &mut CallContext, _: &str) -> Result<Target> {
        ctx.checkpoint()?;
        Ok(Target::Undefined)
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
            pending: None,
        })
    }
}

#[derive(Debug)]
pub(super) struct Host {
    pub params: Buffer<Option<Fact>>,
    pub required: usize,
    pub result: Fact,
    pub constrained: bool,
    pub unresolved: bool,
    accepts_block: bool,
    source: Option<Arc<crate::signature::Compiled>>,
    blocks: HostBlocks,
    granted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostBlocks {
    Possible,
    Ignored,
    Rejected,
}

impl Host {
    /// Reads a compiled registration without constructing a grant or invoking host code.
    pub fn registered(
        ctx: &mut CallContext,
        facts: &mut Facts,
        value: &crate::capability::Registered,
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
        value: &crate::capability::BoundMethod,
    ) -> Result<Self> {
        let mut host = Self::method(
            ctx,
            facts,
            value.compiled_signature(),
            value.supports_block(),
        )?;
        host.granted = value.fresh_grant();
        Ok(host)
    }

    fn method(
        ctx: &mut CallContext,
        facts: &mut Facts,
        signature: Option<Arc<crate::signature::Compiled>>,
        supports_block: bool,
    ) -> Result<Self> {
        let constrained = signature.is_some();
        let mut host = Self::new(ctx, facts, signature)?;
        host.blocks = if supports_block {
            HostBlocks::Possible
        } else if constrained {
            HostBlocks::Ignored
        } else {
            HostBlocks::Rejected
        };
        Ok(host)
    }

    /// Retains metadata so named contracts use each call's current root and source bindings.
    pub fn new(
        ctx: &mut CallContext,
        facts: &mut Facts,
        signature: Option<Arc<crate::signature::Compiled>>,
    ) -> Result<Self> {
        let mut host = Self::resolved(ctx, facts, signature.as_deref(), |_, _| Ok(None))?;
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

#[derive(Clone, Copy)]
pub(super) struct World<'a> {
    pub loader: Option<&'a Arc<crate::loading::Loader>>,
    pub program: &'a Program,
    /// Identity owner used when constructing the source declaration contracts.
    pub source_owner: usize,
    pub contracts: &'a [Fact],
    pub hosts: &'a [Host],
    // Callers supply unique names and admitted facts or bound descriptors, never factories.
    pub globals: &'a [(Value, Target)],
    pub inputs: &'a [Value],
}

#[derive(Debug)]
pub(super) struct LocatedIssue {
    pub source: SourceId,
    pub function: usize,
    pub issue: Issue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Location {
    pub source: SourceId,
    pub function: usize,
    pub pc: usize,
}

#[derive(Debug)]
pub(super) struct Analysis {
    #[cfg(test)]
    pub returns: Fact,
    #[cfg(test)]
    pub throws: u8,
    pub issues: Buffer<LocatedIssue>,
    pub incomplete: Buffer<Location>,
    #[cfg(test)]
    pub contexts: usize,
}

struct Job {
    source: SourceId,
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

struct Scheduler<'a> {
    imports: requires::Imports,
    whole: bool,
    worlds: Registry<'a>,
    storage: super::globals::layout::Storage,
    values: super::inputs::Values,
    jobs: Buffer<Job>,
    buckets: Buffer<usize>,
    queue: Buffer<usize>,
    current: usize,
    dependencies: Buffer<usize>,
    search: usize,
}

struct Solver<'s, 'a> {
    source: SourceId,
    world: World<'s>,
    layouts: &'s Layouts,
    world_index: usize,
    state: &'s mut Scheduler<'a>,
}

impl<'a> Scheduler<'a> {
    fn new(values: super::inputs::Values, whole: bool) -> Self {
        Self {
            imports: requires::Imports::new(),
            whole,
            worlds: Registry::new(),
            storage: super::globals::layout::Storage::new(),
            values,
            jobs: Buffer::empty(),
            buckets: Buffer::empty(),
            queue: Buffer::empty(),
            current: EMPTY,
            dependencies: Buffer::empty(),
            search: 0,
        }
    }

    fn adapter<'s>(&'s mut self, world_index: usize, handle: &'s Handle<'a>) -> Solver<'s, 'a> {
        let view = handle.view();
        Solver {
            source: view.source,
            world: view.world,
            layouts: view.layouts,
            world_index,
            state: self,
        }
    }

    fn solve(&mut self, ctx: &mut CallContext, facts: &mut Facts) -> Result<()> {
        ctx.checkpoint()?;
        while let Some(index) = self.queue.data.pop() {
            ctx.charge(1)?;
            let (world_index, handle) = self.worlds.get(ctx, self.jobs.data[index].source)?;
            self.adapter(world_index, &handle)
                .solve_job(ctx, facts, index)?;
        }
        self.current = EMPTY;
        self.dependencies = Buffer::empty();
        Ok(())
    }
}

enum Ancestor<'a> {
    Function(SourceId, usize, &'a Context),
    Expanding(SourceId, usize, &'a Context),
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
pub(super) fn analyze_with_values(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'_>,
    function: usize,
    inputs: &[Input],
    values: super::inputs::Values,
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

pub(super) fn analyze_general(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'_>,
    entry: General,
    values: super::inputs::Values,
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

fn analyze_entry(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'_>,
    entry: Entry<'_>,
    values: super::inputs::Values,
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
    let source = facts.source_id(ctx, world.source_owner)?;
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
                        source,
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
    let layouts = Layouts::new(ctx, world.program, world.source_owner)?;
    let handle = Handle::borrowed(ctx, facts, world, &layouts)?;
    let mut state = Scheduler::new(values, false);
    let world_index = state.worlds.insert(ctx, handle.clone())?;
    let mut solver = state.adapter(world_index, &handle);
    solver.state.worlds.entries.data[world_index]
        .entry_failures
        .extend(ctx, failures)?;
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
        context.kind = Kind::Entry {
            general,
            admit: false,
        };
    } else if general {
        context.kind = Kind::General;
    }
    solver.prepare(ctx, facts)?;
    solver.prepare_captures(ctx, facts)?;
    if !solver.state.values.captured.entry.data.is_empty() {
        context.kind = Kind::Entry {
            general,
            admit: true,
        };
    }
    context.globals = Globals::initial(ctx, &solver.state.storage.layout)?;
    let entry = solver.request(ctx, facts, function, inputs, flow::NO_ERROR, &context)?;
    solver.solve(ctx, facts)?;
    let result = Analysis {
        #[cfg(test)]
        returns: solver.state.jobs.data[entry].returns,
        #[cfg(test)]
        throws: solver.state.jobs.data[entry].throws | admission_throws,
        issues: admission_issues,
        incomplete: Buffer::empty(),
        #[cfg(test)]
        contexts: solver.state.jobs.data.len(),
    };
    solver.collect(ctx, &[entry], result)
}

impl Solver<'_, '_> {
    fn prepare(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
    ) -> Result<super::globals::layout::Layout> {
        if self.state.storage.layout.find(ctx, self.source)?.is_some() {
            return Ok(self.state.storage.layout.clone());
        }
        self.prepare_receiving(ctx, facts, None)
    }

    fn prepare_receiving(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        receiving: Option<SourceId>,
    ) -> Result<super::globals::layout::Layout> {
        let roots = if let Some(receiving) = receiving {
            let (index, handle) = self.state.worlds.get(ctx, receiving)?;
            self.state.adapter(index, &handle).roots(ctx, facts)?
        } else {
            self.roots(ctx, facts)?
        };
        self.state.storage.prepare(
            ctx,
            facts,
            super::globals::layout::Definition {
                source: self.source,
                owner: self.world.source_owner,
                program: self.world.program,
                files: &self.layouts.files,
                roots: &roots.data,
                receiving,
            },
        )
    }

    fn solve(&mut self, ctx: &mut CallContext, facts: &mut Facts) -> Result<()> {
        self.state.solve(ctx, facts)
    }

    fn requested(&self, function: usize) -> bool {
        self.state.worlds.entries.data[self.world_index]
            .functions
            .data[function]
    }

    fn solve_job(&mut self, ctx: &mut CallContext, facts: &mut Facts, index: usize) -> Result<()> {
        let source = self.source;
        self.state.current = index;
        self.state.jobs.data[index].queued = false;
        self.state.dependencies = Buffer::empty();
        let mut inputs = Buffer::empty();
        let job = &self.state.jobs.data[index];
        inputs.extend(ctx, &job.widened.as_ref().unwrap_or(&job.inputs).data)?;
        let mut context = job
            .widened_context
            .as_ref()
            .unwrap_or(&job.context)
            .snapshot(ctx)?;
        context.globals.expand(ctx, &self.state.storage.layout)?;
        let incoming = context.incoming(ctx)?;
        if context
            .ambient
            .is_some_and(|ambient| ambient.source != source)
        {
            return Err(crate::Error::new(
                crate::ErrorKind::Runtime,
                "checker ambient binding belongs to a different source",
            ));
        }
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
        let function = self.state.jobs.data[index].function;
        let body = flow::Body {
            scope: context.scope,
            ambient: context.ambient.map(|ambient| ambient.index),
            general: context.kind == Kind::General,
            receiver: context.receiver,
            constructor: context.constructor,
            program: self.world.program,
            contracts: self.world.contracts,
            function,
            inputs: &inputs.data,
            current_error: self.state.jobs.data[index].current_error,
            block: block.as_ref(),
            incoming: incoming.as_ref(),
            layouts: Some(self.layouts),
            globals: Some(&context.globals),
        };
        let mut report = if matches!(context.kind, Kind::Entry { .. }) {
            self.initialize_entry(
                ctx,
                facts,
                function,
                &inputs.data,
                &context,
                self.state.jobs.data[index].current_error,
            )?
        } else {
            flow::analyze_body(ctx, facts, body, self)?
        };
        let mut returns = report.normal_returns;
        let previous = self.state.jobs.data[index].returns;
        if self.state.jobs.data[index].cyclic
            && previous != Atom::Never.fact()
            && previous != returns
        {
            let depth = *self.state.jobs.data[index]
                .return_depth
                .get_or_insert(facts.max_depth());
            returns = facts.widen(ctx, previous, returns, depth)?;
        }
        let job = &mut self.state.jobs.data[index];
        if job.cyclic {
            if let Some(previous) = &job.report {
                let depth = *job.return_depth.get_or_insert(facts.max_depth());
                for exit in &mut report.block_exits.data {
                    for before in &previous.block_exits.data {
                        ctx.charge(1)?;
                        if exit.pc == before.pc
                            && exit.completion == before.completion
                            && exit.pending.compatible(ctx, &before.pending)?
                            && exit.globals.compatible(ctx, &before.globals)?
                        {
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
        job.dependencies = std::mem::replace(&mut self.state.dependencies, Buffer::empty());
        if changed {
            for parent in 0..self.state.jobs.data[index].parents.data.len() {
                ctx.charge(1)?;
                let parent = self.state.jobs.data[index].parents.data[parent];
                self.enqueue(ctx, parent)?;
            }
        }
        Ok(())
    }

    fn collect(
        &mut self,
        ctx: &mut CallContext,
        entries: &[usize],
        mut result: Analysis,
    ) -> Result<Analysis> {
        let mut reached = Buffer::with_capacity(ctx, self.state.jobs.data.len())?;
        for _ in &self.state.jobs.data {
            ctx.charge(1)?;
            reached.data.push(false);
        }
        self.state.queue.extend(ctx, entries)?;
        while let Some(index) = self.state.queue.data.pop() {
            ctx.charge(1)?;
            if reached.data[index] {
                continue;
            }
            reached.data[index] = true;
            let job = &self.state.jobs.data[index];
            self.state.queue.extend(ctx, &job.dependencies.data)?;
            let report = job.report.as_ref().unwrap();
            for &issue in &report.issues.data {
                ctx.charge(result.issues.data.len() as u64)?;
                if !result.issues.data.iter().any(|old| {
                    old.source == job.source && old.function == job.function && old.issue == issue
                }) {
                    result.issues.push(
                        ctx,
                        LocatedIssue {
                            source: job.source,
                            function: job.function,
                            issue,
                        },
                    )?;
                }
            }
            for &pc in &report.incomplete.data {
                ctx.charge(result.incomplete.data.len() as u64)?;
                let location = Location {
                    source: job.source,
                    function: job.function,
                    pc,
                };
                if !result.incomplete.data.contains(&location) {
                    result.incomplete.push(ctx, location)?;
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
            .sort_unstable_by_key(|issue| (issue.source, issue.function, issue.issue.pc));
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
        if !self.state.jobs.data[index].queued {
            self.state.queue.push(ctx, index)?;
            self.state.jobs.data[index].queued = true;
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
        let layout = self.prepare(ctx, facts)?;
        let mut normalized;
        let context = if context.globals.layout.same(&layout) {
            context
        } else {
            normalized = context.snapshot(ctx)?;
            normalized.globals.expand(ctx, &layout)?;
            &normalized
        };
        let source = facts.source_id(ctx, self.world.source_owner)?;
        let mut hash = DefaultHasher::new();
        source.hash(&mut hash);
        function.hash(&mut hash);
        inputs.hash(&mut hash);
        current_error.hash(&mut hash);
        context.hash(ctx, &mut hash)?;
        let hash = hash.finish();
        if !self.state.buckets.data.is_empty() {
            let mut index =
                self.state.buckets.data[hash as usize & (self.state.buckets.data.len() - 1)];
            while index != EMPTY {
                ctx.charge(inputs.len() as u64 + 1)?;
                let job = &self.state.jobs.data[index];
                if job.hash == hash
                    && job.source == source
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
        if self.requested(function) {
            if let Some(path) = self.ancestor(ctx, Ancestor::Function(source, function, context))? {
                let index = path.data[0];
                self.cycle(ctx, &path.data)?;
                let depth = *self.state.jobs.data[index]
                    .input_depth
                    .get_or_insert(facts.max_depth());
                let mut joined = Buffer::empty();
                let mut changed = self.state.jobs.data[index].current_error | current_error
                    != self.state.jobs.data[index].current_error;
                self.state.jobs.data[index].current_error |= current_error;
                let mut widened_context = self.state.jobs.data[index]
                    .widened_context
                    .as_ref()
                    .unwrap_or(&self.state.jobs.data[index].context)
                    .snapshot(ctx)?;
                changed |= widened_context.widen(ctx, facts, context, depth)?;
                for (i, &incoming) in inputs.iter().enumerate() {
                    ctx.charge(1)?;
                    let job = &self.state.jobs.data[index];
                    let before = job.widened.as_ref().unwrap_or(&job.inputs).data[i];
                    let value = before.widen(ctx, facts, incoming, depth)?;
                    changed |= before != value;
                    joined.push(ctx, value)?;
                }
                if changed {
                    self.state.jobs.data[index].widened = Some(joined);
                    self.state.jobs.data[index].widened_context = Some(widened_context);
                    self.enqueue(ctx, index)?;
                }
                return Ok(index);
            }
        }
        if self.state.jobs.data.len() >= self.state.buckets.data.len() / 2 {
            let Some(length) = self.state.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(crate::ErrorKind::Memory, "checker call table size overflow");
            };
            let mut buckets = Buffer::with_capacity(ctx, length)?;
            for _ in 0..length {
                ctx.charge(1)?;
                buckets.data.push(EMPTY);
            }
            for (index, job) in self.state.jobs.data.iter_mut().enumerate() {
                ctx.charge(1)?;
                let bucket = job.hash as usize & (length - 1);
                job.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.state.buckets = buckets;
        }
        let mut copied = Buffer::empty();
        copied.extend(ctx, inputs)?;
        let context = context.snapshot(ctx)?;
        let mut parents = Buffer::empty();
        // A newly created context cannot close a cycle. Record its first edge directly.
        if self.state.current != EMPTY {
            parents.push(ctx, self.state.current)?;
        }
        let index = self.state.jobs.data.len();
        let bucket = hash as usize & (self.state.buckets.data.len() - 1);
        self.state.jobs.push(
            ctx,
            Job {
                source,
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
                next: self.state.buckets.data[bucket],
                parents,
                dependencies: Buffer::empty(),
                queued: false,
                returns: Atom::Never.fact(),
                throws: 0,
                report: None,
            },
        )?;
        self.state.buckets.data[bucket] = index;
        self.state.worlds.entries.data[self.world_index]
            .functions
            .data[function] = true;
        self.enqueue(ctx, index)?;
        Ok(index)
    }

    fn ancestor(
        &mut self,
        ctx: &mut CallContext,
        target: Ancestor<'_>,
    ) -> Result<Option<Buffer<usize>>> {
        ctx.checkpoint()?;
        if self.state.current == EMPTY {
            return Ok(None);
        }
        self.state.search = self.state.search.wrapping_add(1);
        if self.state.search == 0 {
            for job in &mut self.state.jobs.data {
                ctx.charge(1)?;
                job.visited = 0;
            }
            self.state.search = 1;
        }
        let mut pending = Buffer::empty();
        pending.push(ctx, (self.state.current, EMPTY))?;
        self.state.jobs.data[self.state.current].visited = self.state.search;
        let mut cursor = 0;
        while cursor < pending.data.len() {
            ctx.charge(1)?;
            let index = pending.data[cursor].0;
            let matched = match target {
                Ancestor::Function(source, function, context) => {
                    self.state.jobs.data[index].source == source
                        && self.state.jobs.data[index].function == function
                        && self.state.jobs.data[index]
                            .context
                            .globals
                            .same_initialization(ctx, &context.globals)?
                        && self.state.jobs.data[index]
                            .context
                            .compatible(ctx, context)?
                }
                Ancestor::Expanding(source, function, context) => {
                    self.state.jobs.data[index].source == source
                        && self.state.jobs.data[index].function == function
                        && self.state.jobs.data[index].context.expands(ctx, context)?
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
            for i in 0..self.state.jobs.data[index].parents.data.len() {
                ctx.charge(1)?;
                let parent = self.state.jobs.data[index].parents.data[i];
                if self.state.jobs.data[parent].visited != self.state.search {
                    self.state.jobs.data[parent].visited = self.state.search;
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
            self.state.jobs.data[index].cyclic = true;
        }
        Ok(())
    }
}

impl Calls for Solver<'_, '_> {
    fn receiving_declaration(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        name: &str,
        globals: &Globals,
    ) -> Result<Option<Fact>> {
        self.receiving_value(ctx, facts, name, globals)
    }

    fn receiving_types(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
        bindings: &mut super::type_bindings::Bindings,
    ) -> Result<Option<super::type_bindings::Scope>> {
        self.receiving_type_scope(ctx, facts, globals, bindings)
    }

    fn annotation(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        source: SourceId,
        ty: usize,
        globals: &Globals,
    ) -> Result<Annotation> {
        let Some((index, handle)) = self.callee(ctx, facts, source)? else {
            return Ok(Annotation {
                expected: Atom::Unknown.fact(),
                resolution: super::type_bindings::Resolution::Dynamic,
                throws: 0,
            });
        };
        self.state
            .adapter(index, &handle)
            .source_annotation(ctx, facts, ty, globals)
    }
    fn require(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        request: &Require,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        self.require_file(ctx, facts, request, current_error, globals)
    }
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
            .state
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
        let handle = self.root_handle(ctx)?;
        let world = handle.view().world;
        let Some((_, Target::Deferred(input))) = world.globals.get(index) else {
            return Ok(super::inputs::Loaded::unavailable());
        };
        let loaded = self.state.values.read(ctx, facts, &world, *input)?;
        self.prepare_captures(ctx, facts)?;
        Ok(loaded)
    }

    fn host_uses_block(&mut self, ctx: &mut CallContext, index: CallableId) -> Result<bool> {
        let Some((_, handle)) = self.state.worlds.find(ctx, index.source)? else {
            return Ok(false);
        };
        Ok(self
            .state
            .values
            .host(ctx, &handle.view().world, index.index)?
            .is_some_and(|host| host.blocks == HostBlocks::Possible))
    }

    fn admit_root(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        root: usize,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        self.materialize_root(ctx, facts, root, current_error, globals)
    }

    fn host_boundary(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: CallableId,
        boundary: HostBoundary<'_>,
        globals: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        let mut outcome = Outcome::empty();
        let Some((_, handle)) = self.state.worlds.find(ctx, index.source)? else {
            outcome.incomplete = true;
            return Ok(outcome);
        };
        let host = HostTarget {
            world: handle.view().world,
            index: index.index,
        };
        match boundary {
            HostBoundary::Arguments(args) => {
                self.host_arguments(ctx, facts, host, args, globals, &mut outcome)?
            }
            HostBoundary::Result(value) => {
                self.host_result(ctx, facts, host, value, globals, &mut outcome)?
            }
        }
        Ok(outcome)
    }

    fn roots(&mut self, ctx: &mut CallContext, facts: &mut Facts) -> Result<Buffer<Root>> {
        let mut roots = Buffer::empty();
        let handle = self.root_handle(ctx)?;
        for (name, target) in handle.view().world.globals {
            ctx.charge(1)?;
            let value = match *target {
                Target::Value(value) => Some(value),
                Target::Deferred(_) => Some(Atom::Never.fact()),
                Target::Builtin(builtin) => Some(facts.builtin(ctx, builtin)?),
                Target::Host(index) | Target::Function(index) => {
                    if let Some((_, handle)) = self.state.worlds.find(ctx, index.source)? {
                        let callable = if matches!(target, Target::Host(_)) {
                            Callable::Host(index.index)
                        } else {
                            Callable::Function(index.index)
                        };
                        Some(facts.callable(ctx, handle.view().world.source_owner, callable)?)
                    } else {
                        None
                    }
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
        let Some(handle) = self.state.worlds.owner(ctx, owner)? else {
            return Ok(Target::Unsupported);
        };
        let view = handle.view();
        Ok(match target {
            Callable::Host(index) if self.state.values.host(ctx, &view.world, index)?.is_some() => {
                Target::Host(view.source.callable(index))
            }
            Callable::Function(index) if index < view.world.program.functions.len() => {
                Target::Function(view.source.callable(index))
            }
            _ => Target::Unsupported,
        })
    }
    fn type_bindings(
        &mut self,
        ctx: &mut CallContext,
        bindings: &mut super::type_bindings::Bindings,
        scope: super::type_bindings::Scope,
    ) -> Result<bool> {
        ctx.checkpoint()?;
        let handle = self.root_handle(ctx)?;
        let globals = handle.view().world.globals;
        for (name, _) in globals {
            bindings.insert(
                ctx,
                scope,
                name.as_bytes().unwrap(),
                super::type_bindings::Binding::Unknown,
            )?;
        }
        Ok(!globals.is_empty())
    }
    fn function_arity(
        &mut self,
        ctx: &mut CallContext,
        function: CallableId,
    ) -> Result<Option<usize>> {
        let Some((_, handle)) = self.state.worlds.find(ctx, function.source)? else {
            return Ok(None);
        };
        ctx.charge(1)?;
        Ok(handle
            .view()
            .world
            .program
            .functions
            .get(function.index)
            .map(|body| body.params.len()))
    }
    fn global(&mut self, ctx: &mut CallContext, name: &str) -> Result<bool> {
        let handle = self.root_handle(ctx)?;
        for (key, _) in handle.view().world.globals {
            let key = key.as_bytes().unwrap();
            ctx.work_bytes(key.len().max(name.len()))?;
            if key == name.as_bytes() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn receiving_binding(&mut self, ctx: &mut CallContext, name: &str) -> Result<Target> {
        let handle = self.root_handle(ctx)?;
        let root = handle.view();
        if self.world.program.file && root.source != self.source {
            root.world.declared_target(ctx, root.source, name)
        } else {
            Ok(Target::Undefined)
        }
    }
    fn resolve(&mut self, ctx: &mut CallContext, name: &str) -> Result<Target> {
        let receiving = self.root_handle(ctx)?;
        let root = receiving.view();
        for (key, target) in root.world.globals {
            let key = key.as_bytes().unwrap();
            ctx.work_bytes(key.len().max(name.len()))?;
            if key == name.as_bytes() {
                return Ok(*target);
            }
        }
        let program = self.world.program;
        let target = self.world.declared_target(ctx, self.source, name)?;
        if target != Target::Undefined {
            return Ok(target);
        }
        if program.file && root.source != self.source {
            let target = root.world.declared_target(ctx, root.source, name)?;
            if target != Target::Undefined {
                return Ok(target);
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
            pending: None,
            exits: Buffer::empty(),
        };
        if !args.admit(ctx, facts, &mut outcome.failures)? {
            return Ok(outcome);
        }
        if let Some(source) = target.source() {
            if let Some(flag) = globals
                .layout
                .find(ctx, source)?
                .and_then(|source| source.activation)
            {
                if matches!(
                    facts.node(globals.values.data[flag]),
                    super::facts::Node::Boolean(false)
                ) {
                    outcome.throws |= 1 << crate::ErrorClass::Runtime as u8;
                    return Ok(outcome);
                }
            }
        }
        if let Some(source) = target.source().filter(|&source| source != self.source) {
            if !matches!(target, Target::Host(_)) {
                let Some((index, handle)) = self.callee(ctx, facts, source)? else {
                    outcome.incomplete = true;
                    return Ok(outcome);
                };
                return self.state.adapter(index, &handle).invoke_local(
                    ctx,
                    facts,
                    target,
                    args,
                    current_error,
                    globals,
                    outcome,
                );
            }
        }
        self.invoke_local(ctx, facts, target, args, current_error, globals, outcome)
    }
}
