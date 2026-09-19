use super::*;
use crate::checking::builtins;

#[cfg(test)]
mod tests;

impl World<'_> {
    /// Resolves source declarations without replacing them with invocation bindings.
    pub(super) fn declared_target(
        &self,
        ctx: &mut CallContext,
        source: SourceId,
        name: &str,
    ) -> Result<Target> {
        ctx.work_bytes(name.len())?;
        if self.program.declaration_names.contains_key(name) {
            return Ok(Target::NonCallable);
        }
        if let Some(&index) = self.program.names.get(name) {
            return Ok(Target::Function(source.callable(index)));
        }
        for (index, host) in self.program.hosts.iter().enumerate() {
            ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                return Ok(Target::Host(source.callable(index)));
            }
        }
        Ok(Target::Undefined)
    }
}

impl<'a> Solver<'_, 'a> {
    /// Selects supplied bindings from the invocation receiving this source.
    pub(super) fn root_handle(&self, ctx: &mut CallContext) -> Result<Handle<'a>> {
        let source = self
            .state
            .storage
            .layout
            .find(ctx, self.source)?
            .map_or(self.source, |source| source.receiving);
        self.state.worlds.get(ctx, source).map(|(_, handle)| handle)
    }

    /// Associates a prepared callee with the caller's receiving environment before queuing work.
    pub(super) fn callee(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        source: SourceId,
    ) -> Result<Option<(usize, Handle<'a>)>> {
        let Some((index, handle)) = self.state.worlds.find(ctx, source)? else {
            return Ok(None);
        };
        let layout = self.prepare(ctx, facts)?;
        let receiving = layout.source(ctx, self.source)?.receiving;
        if let Some(existing) = layout.find(ctx, source)? {
            if existing.receiving != receiving {
                return Ok(None);
            }
        } else {
            self.state
                .adapter(index, &handle)
                .prepare_receiving(ctx, facts, Some(receiving))?;
        }
        Ok(Some((index, handle)))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn invoke_local(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: Target,
        args: Arguments,
        current_error: u16,
        globals: &Globals,
        mut outcome: Outcome,
    ) -> Result<Outcome> {
        if let Target::Helper { receiver, name, .. } = target {
            let site = crate::bytecode::CallSite {
                name: usize::MAX,
                method: None,
                auto: false,
                scope: false,
                parenthesized: true,
            };
            return Ok(builtins::member(ctx, facts, receiver, site, name, &args)?
                .expect("known instance helper"));
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
            Target::Builtin(builtin) => return builtins::invoke(ctx, facts, builtin, &args),
            Target::Offset(value) => {
                return builtins::protected::invoke(ctx, facts, value, &args);
            }
            Target::Function(function)
            | Target::Block(function)
            | Target::Method { function, .. } => {
                if function.source != self.source
                    || function.index >= self.world.program.functions.len()
                    || matches!(target, Target::Block(_))
                        && args.block.as_ref().map(|block| block.function) != Some(function)
                {
                    outcome.incomplete = true;
                    return Ok(outcome);
                }
                let function = function.index;
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
                let source = facts.source_id(ctx, self.world.source_owner)?;
                if (!context.inherited.data.is_empty()
                    || !globals.pending.addresses.data.is_empty())
                    && self.requested(function)
                    && self
                        .ancestor(ctx, Ancestor::Expanding(source, function, &context))?
                        .is_some()
                {
                    outcome.incomplete = true;
                    return Ok(outcome);
                }
                let index =
                    self.request(ctx, facts, function, &inputs.data, current_error, &context)?;
                self.depend(ctx, index)?;
                outcome.value = self.state.jobs.data[index].returns;
                if context.kind == Kind::Plain && globals.values.data.is_empty() {
                    outcome.throws = self.state.jobs.data[index].throws;
                } else if let Some(report) = &self.state.jobs.data[index].report {
                    for exit in &report.block_exits.data {
                        let exit = exit.snapshot(ctx)?;
                        outcome.exits.push(ctx, exit)?;
                    }
                }
            }
            Target::Host(index) => {
                let Some((_, handle)) = self.state.worlds.find(ctx, index.source)? else {
                    outcome.incomplete = true;
                    return Ok(outcome);
                };
                let host = HostTarget {
                    world: handle.view().world,
                    index: index.index,
                };
                self.host_call(ctx, facts, host, &args, globals, &mut outcome)?;
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
