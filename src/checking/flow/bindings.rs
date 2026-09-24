use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    pub(super) fn read_value(
        &mut self,
        state: &mut State,
        pc: usize,
        value: Fact,
        mut origin: Option<usize>,
    ) -> Result<bool> {
        for index in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            if matches!(
                self.facts.node(self.facts.arm(value, index)),
                Node::Callable { .. }
            ) {
                return self.read_attached(state, pc, value, origin);
            }
        }
        let mut readable = Buffer::empty();
        for index in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, index);
            let unreadable = match *self.facts.node(arm) {
                Node::Offset(_) => Some(Target::Offset(arm)),
                Node::Builtin(builtin) if !builtin.auto() => Some(Target::Builtin(builtin)),
                Node::Builtin(builtin) => {
                    let mut next = state.snapshot(self.ctx)?;
                    if let Some(edges) =
                        self.invoke(&mut next, pc, Target::Builtin(builtin), Arguments::new())?
                    {
                        for edge in edges.into_iter().flatten() {
                            self.extra.push(self.ctx, edge)?;
                        }
                    } else {
                        readable.push(self.ctx, next.stack.data.pop().unwrap().value)?;
                        origin = None;
                    }
                    continue;
                }
                _ => None,
            };
            if let Some(target) = unreadable {
                self.issue(
                    pc,
                    IssueKind::Call {
                        target,
                        failure: Failure::BuiltinValue,
                    },
                )?;
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            } else {
                readable.push(self.ctx, arm)?;
            }
        }
        let value = self.facts.union(self.ctx, &readable.data)?;
        if value == Atom::Never.fact() {
            return Ok(false);
        }
        state.stack.push(
            self.ctx,
            Operand {
                origin,
                ..Operand::new(value)
            },
        )?;
        Ok(true)
    }

    pub(super) fn read_function(
        &mut self,
        state: &mut State,
        pc: usize,
        function: CallableId,
    ) -> Result<bool> {
        let target = Target::Function(function);
        let parameters = if function.source == self.source {
            self.program.functions[function.index].params.len()
        } else if let Some(parameters) = self.calls.function_arity(self.ctx, function)? {
            parameters
        } else {
            self.incomplete(pc)?;
            return Ok(false);
        };
        if parameters != 0 {
            self.issue(pc, IssueKind::DetachedValue(target))?;
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            return Ok(false);
        }
        if let Some(edges) = self.invoke(state, pc, target, Arguments::new())? {
            for edge in edges.into_iter().flatten() {
                self.extra.push(self.ctx, edge)?;
            }
            return Ok(false);
        }
        Ok(true)
    }

    pub(super) fn read_fallback(
        &mut self,
        state: &mut State,
        pc: usize,
        name: usize,
    ) -> Result<bool> {
        let index = name;
        let name = &self.program.members[name];
        if let Some((slot, binding)) = self.ambient_binding(state, name)? {
            if binding.missing {
                self.incomplete(pc)?;
                return Ok(false);
            }
            state
                .stack
                .push(self.ctx, Operand::local(binding.value, slot))?;
            return Ok(true);
        }
        let file_declared = self.file_declared_target(name)?.is_some();
        if let Some(index) = if file_declared {
            None
        } else {
            self.root_index(state, name)?
        } {
            return self.read_global(state, pc, index, None);
        }
        if !file_declared && self.calls.global(self.ctx, name)? {
            self.incomplete(pc)?;
            return Ok(false);
        }
        self.ctx.work_bytes(name.len())?;
        if let Some(&index) = self.program.declaration_names.get(name) {
            let value = self.load_declaration(state, pc, index)?;
            state.stack.push(self.ctx, Operand::new(value))?;
            return Ok(true);
        }
        if let Some(&function) = self.program.names.get(name) {
            return self.read_function(state, pc, self.source.callable(function));
        }
        for (index, host) in self.program.hosts.iter().enumerate() {
            self.ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                self.issue(
                    pc,
                    IssueKind::DetachedValue(Target::Host(self.source.callable(index))),
                )?;
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                return Ok(false);
            }
        }
        if let Some(readable) = self.read_receiving(state, pc, name)? {
            return Ok(readable);
        }
        for (index, (global, _)) in self.program.globals.iter().enumerate() {
            self.ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                return self.read_global(state, pc, state.source_slots.globals.data[index], None);
            }
        }
        // A file required by a runtime name may have published this name.
        if self.unknown_exports(state)? {
            if let Some(edges) = self.dynamic_call(state, pc, Arguments::new())? {
                for edge in edges.into_iter().flatten() {
                    self.extra.push(self.ctx, edge)?;
                }
                return Ok(false);
            }
            return Ok(true);
        }
        if let Some(module) = self.function.namespace {
            let receiver = self.self_value(module)?;
            let site = CallSite {
                name: index,
                method: None,
                auto: true,
                scope: false,
                parenthesized: false,
            };
            if let Some(edges) =
                self.namespace_member(state, pc, receiver, site.into(), Arguments::new(), true)?
            {
                for edge in edges.into_iter().flatten() {
                    self.extra.push(self.ctx, edge)?;
                }
                return Ok(false);
            }
            return Ok(true);
        }
        self.issue(
            pc,
            IssueKind::Call {
                target: Target::Undefined,
                failure: Failure::Undefined,
            },
        )?;
        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        Ok(false)
    }

    /// Reports whether `read_fallback` resolves `name` before the members of the
    /// running instance or class.
    pub(super) fn fallback_bound(&mut self, state: &State, name: &str) -> Result<bool> {
        if self.ambient_binding(state, name)?.is_some()
            || self.file_declared_target(name)?.is_some()
            || self.root_index(state, name)?.is_some()
            || self.calls.global(self.ctx, name)?
        {
            return Ok(true);
        }
        self.ctx.work_bytes(name.len())?;
        if self.program.declaration_names.contains_key(name)
            || self.program.names.contains_key(name)
        {
            return Ok(true);
        }
        for host in &self.program.hosts {
            self.ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                return Ok(true);
            }
        }
        if self.program.file && self.calls.receiving_binding(self.ctx, name)? != Target::Undefined {
            return Ok(true);
        }
        for (global, _) in &self.program.globals {
            self.ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                return Ok(true);
            }
        }
        self.unknown_exports(state)
    }

    pub(super) fn read_receiving(
        &mut self,
        state: &mut State,
        pc: usize,
        name: &str,
    ) -> Result<Option<bool>> {
        if !self.program.file {
            return Ok(None);
        }
        match self.calls.receiving_binding(self.ctx, name)? {
            Target::NonCallable => {
                let globals = state.global_call(self.ctx)?;
                if let Some(value) = self
                    .calls
                    .receiving_declaration(self.ctx, self.facts, name, &globals)?
                {
                    state.stack.push(self.ctx, Operand::new(value))?;
                    Ok(Some(true))
                } else {
                    self.incomplete(pc)?;
                    Ok(Some(false))
                }
            }
            Target::Function(function) => self.read_function(state, pc, function).map(Some),
            target @ Target::Host(_) => {
                self.issue(pc, IssueKind::DetachedValue(target))?;
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                Ok(Some(false))
            }
            Target::Undefined => Ok(None),
            _ => {
                self.incomplete(pc)?;
                Ok(Some(false))
            }
        }
    }

    pub(super) fn load_optional(
        &mut self,
        state: &mut State,
        pc: usize,
        slot: usize,
        name: usize,
    ) -> Result<bool> {
        let binding = state.locals.get(self.ctx, slot)?;
        if !binding.missing {
            return self.read_value(state, pc, binding.value, Some(slot));
        }
        let present = if binding.value != Atom::Never.fact() {
            let mut present = state.snapshot(self.ctx)?;
            present.locals.set(
                self.ctx,
                slot,
                Binding {
                    missing: false,
                    ..binding
                },
            )?;
            self.read_value(&mut present, pc, binding.value, Some(slot))?
                .then_some(present)
        } else {
            None
        };
        state.locals.set(
            self.ctx,
            slot,
            Binding {
                value: Atom::Never.fact(),
                missing: true,
                ..binding
            },
        )?;
        if self.read_fallback(state, pc, name)? {
            if let Some(present) = present {
                if state.compatible(self.ctx, &present)? {
                    state.join(self.ctx, self.facts, &present, false, self.program)?;
                } else {
                    self.native_continue(pc, present)?;
                }
            }
            Ok(true)
        } else if let Some(present) = present {
            *state = present;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
