use super::*;
use crate::checking::{flow::IssueKind, scalar::Test};

impl Solver<'_, '_> {
    pub(super) fn initialize_body(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        body: &blocks::Closure,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        if body.function.source != self.source {
            let Some((index, handle)) = self.callee(ctx, facts, body.function.source)? else {
                return Ok(Outcome {
                    incomplete: true,
                    ..Outcome::empty()
                });
            };
            return self.state.adapter(index, &handle).initialize_body(
                ctx,
                facts,
                body,
                current_error,
                globals,
            );
        }
        let mut context = Context::receiving(ctx, body)?;
        context.kind = Kind::Initializing;
        context.ambient = context.block_ambient.take();
        context.globals = globals.snapshot(ctx)?;
        let fresh = self.state.jobs.data.len();
        let index = self.request(
            ctx,
            facts,
            body.function.index,
            &[],
            current_error,
            &context,
        )?;
        self.depend(ctx, index)?;
        if index == fresh {
            self.created(ctx, index)?;
        }
        let mut outcome = Outcome::empty();
        outcome.value = self.state.jobs.data[index].returns;
        if let Some(report) = &self.state.jobs.data[index].report {
            for exit in &report.block_exits.data {
                let exit = exit.snapshot(ctx)?;
                outcome.exits.push(ctx, exit)?;
            }
        }
        Ok(outcome)
    }

    pub(super) fn depend(&mut self, ctx: &mut CallContext, index: usize) -> Result<()> {
        ctx.charge(self.state.dependencies.data.len() as u64)?;
        if !self.state.dependencies.data.contains(&index) {
            self.state.dependencies.push(ctx, index)?;
        }
        ctx.charge(self.state.jobs.data[index].parents.data.len() as u64)?;
        if !self.state.jobs.data[index]
            .parents
            .data
            .contains(&self.state.current)
        {
            if let Some(path) = self.ancestor(ctx, Ancestor::Job(index))? {
                self.cycle(ctx, &path.data)?;
            }
            self.state.jobs.data[index]
                .parents
                .push(ctx, self.state.current)?;
        }
        Ok(())
    }

    pub(super) fn initialize_entry(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        function: usize,
        inputs: &[Input],
        initial: &Context,
        current_error: u16,
    ) -> Result<Report> {
        let Kind::Entry { general, admit } = initial.kind else {
            unreachable!()
        };
        let globals = &initial.globals;
        let mut report = Report {
            returns: Atom::Never.fact(),
            normal_returns: Atom::Never.fact(),
            throws: 0,
            issues: Buffer::empty(),
            incomplete: Buffer::empty(),
            block_exits: Buffer::empty(),
        };
        let mut alternatives = Buffer::empty();
        let globals = globals.snapshot(ctx)?;
        alternatives.push(ctx, globals)?;
        if admit {
            let mut sources = Buffer::empty();
            sources.extend(ctx, &self.state.values.captured.entry.data)?;
            alternatives = self.activate_captures(
                ctx,
                facts,
                &sources.data,
                alternatives,
                current_error,
                &mut report,
            )?;
        }
        if function != 0 || self.world.program.file {
            alternatives =
                self.initialize_namespaces(ctx, facts, alternatives, current_error, &mut report)?;
        }
        if alternatives.data.is_empty() {
            return Ok(report);
        }
        // Runtime initializes namespaces before binding the entry's argument shape.
        let failures = &self.state.worlds.entries.data[self.world_index]
            .entry_failures
            .data;
        if !failures.is_empty() {
            for &failure in failures {
                ctx.charge(1)?;
                report.issues.push(
                    ctx,
                    Issue {
                        pc: usize::MAX,
                        kind: IssueKind::Call {
                            target: Target::Function(self.source.callable(function)),
                            failure,
                        },
                    },
                )?;
            }
            report.throws |= 1 << crate::ErrorClass::Argument as u8;
            return Ok(report);
        }
        for globals in alternatives.data {
            ctx.charge(1)?;
            let mut context = Context::plain();
            if general {
                context.kind = Kind::General;
            }
            context.globals = globals;
            context.constructor = initial.constructor;
            context.scope = initial.scope;
            let index = self.request(ctx, facts, function, inputs, current_error, &context)?;
            self.depend(ctx, index)?;
            report.normal_returns = facts.union(
                ctx,
                &[report.normal_returns, self.state.jobs.data[index].returns],
            )?;
            report.throws |= self.state.jobs.data[index].throws;
            if let Some(result) = &self.state.jobs.data[index].report {
                report.returns = facts.union(ctx, &[report.returns, result.returns])?;
                for exit in &result.block_exits.data {
                    let mut exit = exit.snapshot(ctx)?;
                    exit.globals.inherit_writes(ctx, &context.globals)?;
                    report.block_exits.push(ctx, exit)?;
                }
            }
        }
        Ok(report)
    }

    pub(super) fn initialize_namespaces(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        mut alternatives: Buffer<Globals>,
        current_error: u16,
        report: &mut Report,
    ) -> Result<Buffer<Globals>> {
        for module in 0..self.world.program.namespaces.len() {
            ctx.charge(1)?;
            let Some(body) = self.world.program.namespaces[module].body else {
                continue;
            };
            let mut normal = Buffer::empty();
            for globals in alternatives.data {
                ctx.charge(1)?;
                let flag = globals.layout.source(ctx, self.source)?.namespace(module) + 1;
                let initialized = globals.values.data[flag];
                let yes = facts.filter(ctx, initialized, Test::Truth, true)?;
                let no = facts.filter(ctx, initialized, Test::Truth, false)?;
                if no == Atom::Never.fact() {
                    globals.join_into(ctx, facts, &mut normal)?;
                    continue;
                }
                if yes != Atom::Never.fact() {
                    let mut skipped = globals.snapshot(ctx)?;
                    skipped.values.data[flag] = yes;
                    skipped.join_into(ctx, facts, &mut normal)?;
                }
                let mut context = Context::plain();
                context.globals = globals;
                context.globals.values.data[flag] = no;
                let index = self.request(ctx, facts, body, &[], current_error, &context)?;
                self.depend(ctx, index)?;
                report.throws |= self.state.jobs.data[index].throws;
                if let Some(result) = &self.state.jobs.data[index].report {
                    for exit in &result.block_exits.data {
                        ctx.charge(1)?;
                        if exit.completion == blocks::Completion::Value {
                            let mut globals = exit.globals.snapshot(ctx)?;
                            globals.inherit_writes(ctx, &context.globals)?;
                            globals.join_into(ctx, facts, &mut normal)?;
                        } else {
                            let mut exit = exit.snapshot(ctx)?;
                            exit.globals.inherit_writes(ctx, &context.globals)?;
                            report.block_exits.push(ctx, exit)?;
                        }
                    }
                }
            }
            if normal.data.is_empty() {
                return Ok(normal);
            }
            alternatives = normal;
        }
        Ok(alternatives)
    }
}
