use super::*;
use crate::bytecode::Op;

pub(in crate::checking) fn analyze<'a>(
    ctx: &mut CallContext,
    facts: &mut Facts,
    world: World<'a>,
    values: super::super::inputs::Values<'a>,
) -> Result<Analysis> {
    let program = world.program;
    let source = facts.source_id(ctx, world.source_owner)?;
    let layouts = Layouts::new(ctx, program, world.source_owner)?;
    let mut functions = Buffer::with_capacity(ctx, program.functions.len())?;
    ctx.charge(program.functions.len() as u64)?;
    functions.data.resize(program.functions.len(), false);
    let mut solver = Solver {
        whole: true,
        source,
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
    let mut context = Context::plain();
    context.kind = Kind::General;
    context.globals = Globals::initial(ctx, facts, program)?;
    let roots = solver.roots(ctx, facts)?;
    context.globals.roots(ctx, &roots.data)?;
    context.globals.files(ctx, &layouts.files)?;
    context
        .globals
        .namespaces(ctx, facts, program, solver.world.source_owner)?;
    let mut entries = Buffer::empty();
    let entry = solver.request(ctx, facts, 0, &[], flow::NO_ERROR, &context)?;
    entries.push(ctx, entry)?;
    solver.solve(ctx, facts)?;
    let mut globals = solver.whole_globals(ctx, facts, entry, &context.globals)?;

    // Bodies reached from the entrypoint retain their actual declaring bindings.
    // Unreachable bodies still receive an independent declaration check.
    let mut bodies = Buffer::empty();
    for namespace in &program.namespaces {
        ctx.charge(1)?;
        if let Some(body) = namespace.body {
            bodies.push(ctx, body)?;
        }
    }
    ctx.charge(
        bodies
            .data
            .len()
            .saturating_mul(bodies.data.len().max(1).ilog2() as usize + 1) as u64,
    )?;
    bodies
        .data
        .sort_unstable_by_key(|&body| program.functions[body].offset);
    for body in bodies.data {
        if solver.whole_reached(ctx, &entries.data, source, body)? {
            continue;
        }
        let mut context = solver.whole_initializer(ctx, body)?;
        context.globals = globals.snapshot(ctx)?;
        let index = solver.request(ctx, facts, body, &[], flow::NO_ERROR, &context)?;
        entries.push(ctx, index)?;
        solver.solve(ctx, facts)?;
        globals = solver.whole_globals(ctx, facts, index, &globals)?;
    }

    let mut selected = Buffer::with_capacity(ctx, program.functions.len())?;
    ctx.charge(program.functions.len() as u64)?;
    selected.data.resize(program.functions.len(), 0u8);
    for &function in program.names.values() {
        ctx.charge(1)?;
        selected.data[function] |= 1;
    }
    for namespace in &program.namespaces {
        ctx.charge(1)?;
        for method in namespace.methods.iter().chain(&namespace.instance_methods) {
            ctx.charge(1)?;
            if program.functions[method.function].accessor.is_none() {
                selected.data[method.function] |= 1;
            }
        }
        if let Some((function, _)) = namespace.constructor {
            selected.data[function] |= 2;
        }
    }
    selected.data[0] = 0;
    for (function, &modes) in selected.data.iter().enumerate() {
        ctx.charge(1)?;
        if modes == 0 {
            continue;
        }
        let inputs = super::super::arguments::general_inputs(
            ctx,
            facts,
            &program.functions[function].params,
            solver.world.contracts,
        )?;
        for (mode, constructor) in [(1, false), (2, true)] {
            if modes & mode == 0 {
                continue;
            }
            for given in [false, true] {
                let mut context = Context::plain();
                context.kind = Kind::General;
                context.constructor = constructor;
                context.scope = blocks::Scope::Declaration { given };
                context.globals = globals.snapshot(ctx)?;
                let index =
                    solver.request(ctx, facts, function, &inputs.data, flow::NO_ERROR, &context)?;
                entries.push(ctx, index)?;
            }
        }
    }
    solver.solve(ctx, facts)?;
    let analysis = Analysis {
        #[cfg(test)]
        returns: solver.jobs.data[entry].returns,
        #[cfg(test)]
        throws: solver.jobs.data[entry].throws,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        #[cfg(test)]
        contexts: solver.jobs.data.len(),
    };
    solver.collect(ctx, &entries.data, analysis)
}

impl Solver<'_> {
    pub(super) fn whole_reached(
        &self,
        ctx: &mut CallContext,
        entries: &[usize],
        source: SourceId,
        function: usize,
    ) -> Result<bool> {
        let mut seen = Buffer::with_capacity(ctx, self.jobs.data.len())?;
        ctx.charge(self.jobs.data.len() as u64)?;
        seen.data.resize(self.jobs.data.len(), false);
        let mut pending = Buffer::empty();
        pending.extend(ctx, entries)?;
        while let Some(index) = pending.data.pop() {
            ctx.charge(1)?;
            if seen.data[index] {
                continue;
            }
            seen.data[index] = true;
            let job = &self.jobs.data[index];
            if job.source == source && job.function == function {
                return Ok(true);
            }
            pending.extend(ctx, &job.dependencies.data)?;
        }
        Ok(false)
    }

    pub(super) fn constructor_fields(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        module: usize,
        globals: &Globals,
    ) -> Result<Option<Fact>> {
        if !self.whole {
            return Ok(None);
        }
        let program = self.world.program;
        let namespace = &program.namespaces[module];
        let Some((function, _)) = namespace.constructor else {
            return Ok(None);
        };
        let mut typed = false;
        for method in &namespace.instance_methods {
            ctx.charge(1)?;
            if let Some((name, _)) = &program.functions[method.function].accessor {
                typed |=
                    super::super::namespaces::property_type(ctx, program, module, name)?.is_some();
            }
        }
        if !typed {
            return Ok(None);
        }
        let root = super::super::namespaces::slot(globals.values.data.len(), program, module) + 2;
        let Some(heap) = super::super::heaps::entries(ctx, facts, globals.values.data[root])?
        else {
            return Ok(Some(Atom::Unknown.fact()));
        };
        let slot = facts.integer(ctx, heap.data.len() as i64)?;
        let inputs = super::super::arguments::general_inputs(
            ctx,
            facts,
            &program.functions[function].params,
            self.world.contracts,
        )?;
        let mut value = Atom::Never.fact();
        let mut pending = false;
        for given in [false, true] {
            let mut context = Context::plain();
            context.kind = Kind::General;
            context.constructor = true;
            context.scope = blocks::Scope::Declaration { given };
            context.globals = globals.snapshot(ctx)?;
            let index =
                self.request(ctx, facts, function, &inputs.data, flow::NO_ERROR, &context)?;
            self.depend(ctx, index)?;
            let Some(report) = &self.jobs.data[index].report else {
                pending = true;
                continue;
            };
            for exit in &report.block_exits.data {
                ctx.charge(1)?;
                if exit.completion != blocks::Completion::Value {
                    continue;
                }
                let selected =
                    super::super::heaps::read(ctx, facts, exit.globals.values.data[root], slot)?;
                let fields = if selected.unsupported {
                    Atom::Unknown.fact()
                } else {
                    selected.value
                };
                value = facts.union(ctx, &[value, fields])?;
            }
        }
        if pending {
            Ok(Some(Atom::Never.fact()))
        } else if value == Atom::Never.fact() {
            Ok(Some(Atom::Unknown.fact()))
        } else {
            Ok(Some(value))
        }
    }

    fn whole_globals(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        initial: &Globals,
    ) -> Result<Globals> {
        let report = self.jobs.data[index].report.as_ref().unwrap();
        ctx.charge(report.block_exits.data.len() as u64)?;
        let normal = report
            .block_exits
            .data
            .iter()
            .any(|exit| exit.completion == blocks::Completion::Value);
        let mut globals: Option<Globals> = None;
        for exit in &report.block_exits.data {
            ctx.charge(1)?;
            if normal && exit.completion != blocks::Completion::Value {
                continue;
            }
            if let Some(globals) = &mut globals {
                globals.join(ctx, facts, &exit.globals, None)?;
            } else {
                globals = Some(exit.globals.snapshot(ctx)?);
            }
        }
        match globals {
            Some(globals) => Ok(globals),
            None => initial.snapshot(ctx),
        }
    }

    fn whole_initializer(&self, ctx: &mut CallContext, function: usize) -> Result<Context> {
        let program = self.world.program;
        let module = program.functions[function].namespace.unwrap();
        let mut context = Context::plain();
        context.kind = Kind::Initializing;
        for (parent, body) in program.functions.iter().enumerate() {
            for op in &body.code {
                ctx.charge(1)?;
                if !matches!(*op, Op::InitNamespace(index) if index == module) {
                    continue;
                }
                context.ambient = Some(self.source.callable(parent));
                let base = self.layouts.locals(ctx, program, function)?;
                let Some(locals) = base.checked_add(body.local_names.len()) else {
                    return ctx.fail(
                        crate::ErrorKind::Memory,
                        "checker ambient layout size overflow",
                    );
                };
                context.locals = locals;
                for slot in 0..body.local_names.len() {
                    context.captures.push(
                        ctx,
                        blocks::Capture {
                            slot: base + slot,
                            value: Atom::Unknown.fact(),
                            missing: false,
                            owner: blocks::Owner::Function(self.source.callable(parent)),
                        },
                    )?;
                }
                return Ok(context);
            }
        }
        context.locals = self.layouts.locals(ctx, program, function)?;
        Ok(context)
    }
}
