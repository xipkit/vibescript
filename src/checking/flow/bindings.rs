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

    /// Reads `value` for `receiving`. A receiver keeps a dynamically bound
    /// builtin as a value, and Go refuses any member on a function kept that way.
    pub(super) fn receive_value(
        &mut self,
        state: &mut State,
        pc: usize,
        value: Fact,
        origin: Option<usize>,
        receiving: Receiving,
    ) -> Result<bool> {
        let Some(member) = receiving.member() else {
            return self.read_value(state, pc, value, origin);
        };
        let mut kept = Buffer::empty();
        for index in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, index);
            if matches!(self.facts.node(arm), Node::Callable { .. }) {
                let target = self.value_target(arm)?;
                self.callable_member(state, pc, target, member)?;
            } else {
                kept.push(self.ctx, arm)?;
            }
        }
        let value = self.facts.union(self.ctx, &kept.data)?;
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

    /// Reports Go's refusal of `member` on a callable a receiver kept as a value.
    pub(super) fn callable_member(
        &mut self,
        state: &State,
        pc: usize,
        target: Target,
        member: usize,
    ) -> Result<()> {
        self.issue(pc, IssueKind::CallableMember { target, member })?;
        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))
    }

    /// Reads a script function for `receiving`. Only a required file's own
    /// functions are bound dynamically.
    pub(super) fn receive_function(
        &mut self,
        state: &mut State,
        pc: usize,
        function: CallableId,
        receiving: Receiving,
        dynamic: bool,
    ) -> Result<bool> {
        if let Some(member) = receiving.member() {
            let runs = if dynamic {
                receiving.runs_dynamic()
            } else {
                let Some(parameters) = self.function_parameters(pc, function)? else {
                    return Ok(false);
                };
                receiving.runs_static(Some(parameters))
            };
            if !runs {
                self.callable_member(state, pc, Target::Function(function), member)?;
                return Ok(false);
            }
        }
        self.read_function(state, pc, function)
    }

    /// Reads a host method for `receiving`, which always fails.
    pub(super) fn receive_host(
        &mut self,
        state: &State,
        pc: usize,
        target: Target,
        receiving: Receiving,
    ) -> Result<()> {
        match receiving.member() {
            Some(member) if !receiving.runs_static(None) => {
                self.callable_member(state, pc, target, member)
            }
            _ => {
                self.issue(pc, IssueKind::DetachedValue(target))?;
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))
            }
        }
    }

    /// The parameter count of `function`, or `None` after reporting incomplete
    /// analysis when another source's function is unknown.
    fn function_parameters(&mut self, pc: usize, function: CallableId) -> Result<Option<usize>> {
        if function.source == self.source {
            return Ok(Some(self.program.functions[function.index].params.len()));
        }
        let parameters = self.calls.function_arity(self.ctx, function)?;
        if parameters.is_none() {
            self.incomplete(pc)?;
        }
        Ok(parameters)
    }

    pub(super) fn read_function(
        &mut self,
        state: &mut State,
        pc: usize,
        function: CallableId,
    ) -> Result<bool> {
        let target = Target::Function(function);
        let Some(parameters) = self.function_parameters(pc, function)? else {
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
        receiving: Receiving,
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
            return self.read_global(state, pc, index, receiving);
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
            let function = self.source.callable(function);
            return self.receive_function(state, pc, function, receiving, self.program.file);
        }
        for (index, host) in self.program.hosts.iter().enumerate() {
            self.ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                let target = Target::Host(self.source.callable(index));
                self.receive_host(state, pc, target, receiving)?;
                return Ok(false);
            }
        }
        if let Some(readable) = self.read_receiving(state, pc, name, receiving)? {
            return Ok(readable);
        }
        for (index, (global, _)) in self.program.globals.iter().enumerate() {
            self.ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                let index = state.source_slots.globals.data[index];
                return self.read_global(state, pc, index, receiving);
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
        receiving: Receiving,
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
            Target::Function(function) => self
                .receive_function(state, pc, function, receiving, false)
                .map(Some),
            target @ Target::Host(_) => {
                self.receive_host(state, pc, target, receiving)?;
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
        receiving: Receiving,
    ) -> Result<bool> {
        let binding = state.locals.get(self.ctx, slot)?;
        if !binding.missing {
            return self.receive_value(state, pc, binding.value, Some(slot), receiving);
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
            self.receive_value(&mut present, pc, binding.value, Some(slot), receiving)?
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
        if self.read_fallback(state, pc, name, receiving)? {
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
