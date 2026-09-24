use super::*;
use crate::bytecode::Op;

pub(in crate::checking) fn analyze(
    ctx: &mut CallContext,
    facts: &mut Facts,
    environment: crate::checking::environment::Environment,
    values: super::super::inputs::Values,
) -> Result<Analysis> {
    let handle = Handle::owned(ctx, facts, environment)?;
    let view = handle.view();
    let program = view.world.program;
    let source = view.source;
    let mut state = Scheduler::new(values, true);
    let world_index = state.worlds.insert(ctx, handle.clone())?;
    let mut solver = state.adapter(world_index, &handle);
    let mut context = Context::plain();
    context.kind = Kind::General;
    let layout = solver.prepare(ctx, facts)?;
    context.globals = Globals::initial(ctx, &layout)?;
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
        let mut next = Buffer::empty();
        for globals in globals.data {
            ctx.charge(1)?;
            let mut context = solver.whole_initializer(ctx, body)?;
            context.globals = globals;
            let index = solver.request(ctx, facts, body, &[], flow::NO_ERROR, &context)?;
            entries.push(ctx, index)?;
            solver.solve(ctx, facts)?;
            for globals in solver
                .whole_globals(ctx, facts, index, &context.globals)?
                .data
            {
                globals.join_into(ctx, facts, &mut next)?;
            }
        }
        globals = next;
    }

    for globals in globals.data {
        solver.queue_declarations(ctx, facts, &globals, &mut entries)?;
    }
    solver.solve(ctx, facts)?;
    let analysis = Analysis {
        #[cfg(test)]
        returns: solver.state.jobs.data[entry].returns,
        #[cfg(test)]
        throws: solver.state.jobs.data[entry].throws,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        #[cfg(test)]
        contexts: solver.state.jobs.data.len(),
    };
    solver.collect(ctx, &entries.data, analysis)
}

impl Solver<'_, '_> {
    pub(super) fn queue_declarations(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
        entries: &mut Buffer<usize>,
    ) -> Result<()> {
        let program = self.world.program;
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
                self.world.contracts,
            )?;
            // A declaration that cannot observe its incoming block analyzes identically with or
            // without one, including in the blocks it passes on.
            let observes = self.layouts.observes_block(ctx, function)?;
            for (mode, constructor) in [(1, false), (2, true)] {
                if modes & mode == 0 {
                    continue;
                }
                for given in [false, true] {
                    if given && !observes {
                        continue;
                    }
                    let mut context = Context::plain();
                    context.kind = Kind::General;
                    context.constructor = constructor;
                    context.scope = blocks::Scope::Declaration { given };
                    context.globals = globals.snapshot(ctx)?;
                    let index =
                        self.request(ctx, facts, function, &inputs.data, flow::NO_ERROR, &context)?;
                    entries.push(ctx, index)?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn required_declarations(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
    ) -> Result<()> {
        if !self.state.whole {
            return Ok(());
        }
        let mut alternatives = Buffer::empty();
        let globals = globals.snapshot(ctx)?;
        alternatives.push(ctx, globals)?;
        let mut bodies = Buffer::empty();
        for namespace in &self.world.program.namespaces {
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
            .sort_unstable_by_key(|&body| self.world.program.functions[body].offset);
        for body in bodies.data {
            let module = self.world.program.functions[body].namespace.unwrap();
            let mut next = Buffer::empty();
            for globals in alternatives.data {
                ctx.charge(1)?;
                let flag = globals.layout.source(ctx, self.source)?.namespace(module) + 1;
                if facts.filter(
                    ctx,
                    globals.values.data[flag],
                    super::super::scalar::Test::Truth,
                    true,
                )? != Atom::Never.fact()
                {
                    globals.join_into(ctx, facts, &mut next)?;
                    continue;
                }
                let mut context = self.whole_initializer(ctx, body)?;
                context.globals = globals;
                let index = self.request(ctx, facts, body, &[], flow::NO_ERROR, &context)?;
                self.depend(ctx, index)?;
                if self.state.jobs.data[index].report.is_none() {
                    return Ok(());
                }
                for globals in self
                    .whole_globals(ctx, facts, index, &context.globals)?
                    .data
                {
                    globals.join_into(ctx, facts, &mut next)?;
                }
            }
            alternatives = next;
        }
        let mut entries = Buffer::empty();
        for globals in alternatives.data {
            self.queue_declarations(ctx, facts, &globals, &mut entries)?;
        }
        for entry in entries.data {
            self.depend(ctx, entry)?;
        }
        Ok(())
    }

    pub(super) fn whole_reached(
        &self,
        ctx: &mut CallContext,
        entries: &[usize],
        source: SourceId,
        function: usize,
    ) -> Result<bool> {
        let mut seen = Buffer::with_capacity(ctx, self.state.jobs.data.len())?;
        ctx.charge(self.state.jobs.data.len() as u64)?;
        seen.data.resize(self.state.jobs.data.len(), false);
        let mut pending = Buffer::empty();
        pending.extend(ctx, entries)?;
        while let Some(index) = pending.data.pop() {
            ctx.charge(1)?;
            if seen.data[index] {
                continue;
            }
            seen.data[index] = true;
            let job = &self.state.jobs.data[index];
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
        if !self.state.whole {
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
        let root = globals.layout.source(ctx, self.source)?.namespace(module) + 2;
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
            let Some(report) = &self.state.jobs.data[index].report else {
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
    ) -> Result<Buffer<Globals>> {
        let report = self.state.jobs.data[index].report.as_ref().unwrap();
        ctx.charge(report.block_exits.data.len() as u64)?;
        let normal = report
            .block_exits
            .data
            .iter()
            .any(|exit| exit.completion == blocks::Completion::Value);
        let mut globals = Buffer::empty();
        for exit in &report.block_exits.data {
            ctx.charge(1)?;
            if normal && exit.completion != blocks::Completion::Value {
                continue;
            }
            exit.globals
                .snapshot(ctx)?
                .join_allocations_into(ctx, facts, &mut globals)?;
        }
        if globals.data.is_empty() {
            let initial = initial.snapshot(ctx)?;
            globals.push(ctx, initial)?;
        }
        Ok(globals)
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
