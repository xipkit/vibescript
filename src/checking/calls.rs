use super::{
    arguments::{Arguments, Failure, Input},
    facts::{Atom, Fact, Facts},
    flow::{self, Issue, Report},
    relation::Relation,
};
use crate::{CallContext, Result, Value, budget::Buffer, bytecode::Program};
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    Function(usize),
    Host(usize),
    NonCallable,
    Undefined,
    Dynamic,
    Unsupported,
}

pub(super) struct Outcome {
    pub value: Fact,
    pub failures: Buffer<Failure>,
    pub incomplete: bool,
}

pub(super) trait Calls {
    fn global(&mut self, ctx: &mut CallContext, name: &str) -> Result<bool>;
    fn resolve(&mut self, ctx: &mut CallContext, name: &str) -> Result<Target>;
    fn invoke(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: Target,
        args: Arguments,
    ) -> Result<Outcome>;
}

pub(super) struct Unavailable;

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
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        Ok(Outcome {
            value: Atom::Never.fact(),
            failures: Buffer::empty(),
            incomplete: true,
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
}

impl Host {
    pub fn new(
        ctx: &mut CallContext,
        facts: &mut Facts,
        signature: Option<&crate::signature::Compiled>,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let mut host = Self {
            params: Buffer::empty(),
            required: 0,
            result: Atom::Unknown.fact(),
            constrained: signature.is_some(),
            unresolved: false,
        };
        if let Some(signature) = signature {
            host.required = signature.required;
            for param in &signature.params {
                ctx.charge(1)?;
                let fact = param
                    .as_ref()
                    .map(|ty| facts.annotation(ctx, ty, |_, _| Ok(None)))
                    .transpose()?;
                host.unresolved |= fact.is_some_and(|fact| facts.unresolved(fact));
                host.params.push(ctx, fact)?;
            }
            if let Some(ty) = &signature.result {
                host.result = facts.annotation(ctx, ty, |_, _| Ok(None))?;
            }
            host.unresolved |= facts.unresolved(host.result);
        }
        Ok(host)
    }
}

pub(super) struct World<'a> {
    pub program: &'a Program,
    pub contracts: &'a [Fact],
    pub hosts: &'a [Host],
    // Callers supply already-bound descriptors; analysis never runs factories.
    pub globals: &'a [(Value, Target)],
}

#[derive(Debug)]
pub(super) struct LocatedIssue {
    pub function: usize,
    pub issue: Issue,
}

#[derive(Debug)]
pub(super) struct Analysis {
    pub returns: Fact,
    pub issues: Buffer<LocatedIssue>,
    pub incomplete: Buffer<(usize, usize)>,
    pub contexts: usize,
}

struct Job {
    function: usize,
    inputs: Buffer<Input>,
    hash: u64,
    next: usize,
    parents: Buffer<usize>,
    dependencies: Buffer<usize>,
    queued: bool,
    returns: Fact,
    report: Option<Report>,
}

struct Solver<'a> {
    world: World<'a>,
    jobs: Buffer<Job>,
    buckets: Buffer<usize>,
    queue: Buffer<usize>,
    current: usize,
    dependencies: Buffer<usize>,
}

const EMPTY: usize = usize::MAX;

pub(super) fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'_>,
    function: usize,
    inputs: &[Input],
) -> Result<Analysis> {
    ctx.checkpoint()?;
    let mut solver = Solver {
        world,
        jobs: Buffer::empty(),
        buckets: Buffer::empty(),
        queue: Buffer::empty(),
        current: EMPTY,
        dependencies: Buffer::empty(),
    };
    let entry = solver.request(ctx, function, inputs)?;
    while let Some(index) = solver.queue.data.pop() {
        ctx.charge(1)?;
        solver.current = index;
        solver.jobs.data[index].queued = false;
        solver.dependencies = Buffer::empty();
        let mut inputs = Buffer::empty();
        inputs.extend(ctx, &solver.jobs.data[index].inputs.data)?;
        let function = solver.jobs.data[index].function;
        let body = flow::Body {
            program: solver.world.program,
            contracts: solver.world.contracts,
            function,
            inputs: &inputs.data,
        };
        let report = flow::analyze_body(ctx, facts, body, &mut solver)?;
        let mut returns = report.returns;
        if returns != Atom::Never.fact() {
            if let Some(ty) = solver.world.program.functions[function].return_type {
                returns = facts.normalized(ctx, returns, solver.world.contracts[ty])?;
            }
        }
        let job = &mut solver.jobs.data[index];
        let changed = job.returns != returns;
        job.returns = returns;
        job.report = Some(report);
        job.dependencies = std::mem::replace(&mut solver.dependencies, Buffer::empty());
        if changed {
            for parent in 0..solver.jobs.data[index].parents.data.len() {
                ctx.charge(1)?;
                let parent = solver.jobs.data[index].parents.data[parent];
                solver.enqueue(ctx, parent)?;
            }
        }
    }
    let mut result = Analysis {
        returns: solver.jobs.data[entry].returns,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: solver.jobs.data.len(),
    };
    let mut reached = Buffer::with_capacity(ctx, solver.jobs.data.len())?;
    for _ in &solver.jobs.data {
        ctx.charge(1)?;
        reached.data.push(false);
    }
    solver.queue.push(ctx, entry)?;
    while let Some(index) = solver.queue.data.pop() {
        ctx.charge(1)?;
        if reached.data[index] {
            continue;
        }
        reached.data[index] = true;
        let job = &solver.jobs.data[index];
        solver.queue.extend(ctx, &job.dependencies.data)?;
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
            .saturating_mul(result.issues.data.len().max(1).ilog2() as usize + 1) as u64,
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

impl Solver<'_> {
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
        function: usize,
        inputs: &[Input],
    ) -> Result<usize> {
        ctx.charge(inputs.len() as u64 + 1)?;
        let mut hash = DefaultHasher::new();
        function.hash(&mut hash);
        inputs.hash(&mut hash);
        let hash = hash.finish();
        if !self.buckets.data.is_empty() {
            let mut index = self.buckets.data[hash as usize & (self.buckets.data.len() - 1)];
            while index != EMPTY {
                ctx.charge(inputs.len() as u64 + 1)?;
                let job = &self.jobs.data[index];
                if job.hash == hash && job.function == function && job.inputs.data == inputs {
                    return Ok(index);
                }
                index = job.next;
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
        let index = self.jobs.data.len();
        let bucket = hash as usize & (self.buckets.data.len() - 1);
        self.jobs.push(
            ctx,
            Job {
                function,
                inputs: copied,
                hash,
                next: self.buckets.data[bucket],
                parents: Buffer::empty(),
                dependencies: Buffer::empty(),
                queued: false,
                returns: Atom::Never.fact(),
                report: None,
            },
        )?;
        self.buckets.data[bucket] = index;
        self.enqueue(ctx, index)?;
        Ok(index)
    }
}

impl Calls for Solver<'_> {
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
            return Ok(Target::Unsupported);
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
        // Builtin resolution and receiving-file exports are not wired in yet.
        for (global, _) in &program.globals {
            ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                return Ok(Target::Unsupported);
            }
        }
        Ok(Target::Undefined)
    }

    fn invoke(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: Target,
        args: Arguments,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        let mut outcome = Outcome {
            value: Atom::Never.fact(),
            failures: Buffer::empty(),
            incomplete: false,
        };
        match target {
            Target::Function(function) => {
                let bound = args.bind(
                    ctx,
                    facts,
                    &self.world.program.functions[function].params,
                    self.world.contracts,
                )?;
                if !bound.failures.data.is_empty() {
                    outcome.failures = bound.failures;
                    return Ok(outcome);
                }
                let index = self.request(ctx, function, &bound.inputs.data)?;
                ctx.charge(self.dependencies.data.len() as u64)?;
                if !self.dependencies.data.contains(&index) {
                    self.dependencies.push(ctx, index)?;
                }
                let job = &mut self.jobs.data[index];
                ctx.charge(job.parents.data.len() as u64)?;
                if !job.parents.data.contains(&self.current) {
                    job.parents.push(ctx, self.current)?;
                }
                outcome.value = job.returns;
            }
            Target::Host(index) => {
                let Some(host) = self.world.hosts.get(index) else {
                    outcome.incomplete = true;
                    return Ok(outcome);
                };
                if host.unresolved {
                    outcome.incomplete = true;
                    return Ok(outcome);
                }
                if host.constrained {
                    if args.positional.data.len() < host.required
                        || args.positional.data.len() > host.params.data.len()
                    {
                        outcome.failures.push(ctx, Failure::HostArity)?;
                    }
                    if !args.keywords.data.is_empty() {
                        outcome.failures.push(ctx, Failure::HostKeywords)?;
                    }
                    for (index, (&actual, expected)) in args
                        .positional
                        .data
                        .iter()
                        .zip(&host.params.data)
                        .enumerate()
                    {
                        ctx.charge(1)?;
                        if let Some(expected) = expected {
                            if facts.relation(ctx, actual, *expected)? == Relation::Rejected {
                                outcome.failures.push(
                                    ctx,
                                    Failure::Type {
                                        parameter: index,
                                        actual,
                                        expected: *expected,
                                    },
                                )?;
                            }
                        }
                    }
                }
                if outcome.failures.data.is_empty() {
                    outcome.value = host.result;
                }
            }
            Target::Dynamic => outcome.value = Atom::Unknown.fact(),
            Target::Unsupported => outcome.incomplete = true,
            Target::NonCallable => outcome.failures.push(ctx, Failure::NonCallable)?,
            Target::Undefined => outcome.failures.push(ctx, Failure::Undefined)?,
        }
        Ok(outcome)
    }
}
