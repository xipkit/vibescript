use super::*;
use crate::checking::{flow::IssueKind, namespaces, scalar::Test};

impl Solver<'_> {
    pub(super) fn depend(&mut self, ctx: &mut CallContext, index: usize) -> Result<()> {
        ctx.charge(self.dependencies.data.len() as u64)?;
        if !self.dependencies.data.contains(&index) {
            self.dependencies.push(ctx, index)?;
        }
        ctx.charge(self.jobs.data[index].parents.data.len() as u64)?;
        if !self.jobs.data[index].parents.data.contains(&self.current) {
            if let Some(path) = self.ancestor(ctx, Ancestor::Job(index))? {
                self.cycle(ctx, &path.data)?;
            }
            self.jobs.data[index].parents.push(ctx, self.current)?;
        }
        Ok(())
    }

    pub(super) fn initialize_entry(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        function: usize,
        inputs: &[Input],
        globals: &Globals,
    ) -> Result<Report> {
        let mut report = Report {
            returns: Atom::Never.fact(),
            normal_returns: Atom::Never.fact(),
            throws: 0,
            issues: Buffer::empty(),
            incomplete: Buffer::empty(),
            block_exits: Buffer::empty(),
        };
        let mut globals = globals.snapshot(ctx)?;
        for module in 0..self.world.program.namespaces.len() {
            ctx.charge(1)?;
            let Some(body) = self.world.program.namespaces[module].body else {
                continue;
            };
            let flag = namespaces::slot(globals.values.data.len(), self.world.program, module) + 1;
            let initialized = globals.values.data[flag];
            let yes = facts.filter(ctx, initialized, Test::Truth, true)?;
            let no = facts.filter(ctx, initialized, Test::Truth, false)?;
            if no == Atom::Never.fact() {
                continue;
            }
            let mut normal = if yes == Atom::Never.fact() {
                None
            } else {
                let mut skipped = globals.snapshot(ctx)?;
                skipped.values.data[flag] = yes;
                Some(skipped)
            };
            let mut context = Context::plain();
            context.globals = globals;
            context.globals.values.data[flag] = no;
            let index = self.request(ctx, facts, body, &[], flow::NO_ERROR, &context)?;
            self.depend(ctx, index)?;
            report.throws |= self.jobs.data[index].throws;
            if let Some(result) = &self.jobs.data[index].report {
                for exit in &result.block_exits.data {
                    ctx.charge(1)?;
                    if exit.completion == blocks::Completion::Value {
                        if let Some(normal) = &mut normal {
                            normal.join(ctx, facts, &exit.globals, None)?;
                        } else {
                            normal = Some(exit.globals.snapshot(ctx)?);
                        }
                    } else {
                        let exit = exit.snapshot(ctx)?;
                        report.block_exits.push(ctx, exit)?;
                    }
                }
            }
            let Some(next) = normal else {
                return Ok(report);
            };
            globals = next;
        }
        // Runtime initializes namespaces before binding the entry's argument shape.
        if !self.entry_failures.data.is_empty() {
            for &failure in &self.entry_failures.data {
                ctx.charge(1)?;
                report.issues.push(
                    ctx,
                    Issue {
                        pc: usize::MAX,
                        kind: IssueKind::Call {
                            target: Target::Function(function),
                            failure,
                        },
                    },
                )?;
            }
            report.throws |= 1 << crate::ErrorClass::Argument as u8;
            return Ok(report);
        }
        let mut context = Context::plain();
        context.globals = globals;
        let index = self.request(ctx, facts, function, inputs, flow::NO_ERROR, &context)?;
        self.depend(ctx, index)?;
        report.normal_returns = self.jobs.data[index].returns;
        report.throws |= self.jobs.data[index].throws;
        if let Some(result) = &self.jobs.data[index].report {
            report.returns = result.returns;
            for exit in &result.block_exits.data {
                let exit = exit.snapshot(ctx)?;
                report.block_exits.push(ctx, exit)?;
            }
        }
        Ok(report)
    }
}
