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
        function: usize,
    ) -> Result<bool> {
        let target = Target::Function(function);
        if !self.program.functions[function].params.is_empty() {
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
        if let Some(index) = self.root_index(name)? {
            return self.read_global(state, pc, index, None);
        }
        if self.calls.global(self.ctx, name)? {
            self.incomplete(pc)?;
            return Ok(false);
        }
        self.ctx.work_bytes(name.len())?;
        if let Some(&index) = self.program.declaration_names.get(name) {
            if self.declaration_pending(index) {
                self.incomplete(pc)?;
                return Ok(false);
            }
            let value = self.declaration_value(index)?;
            state.stack.push(self.ctx, Operand::new(value))?;
            return Ok(true);
        }
        if let Some(&function) = self.program.names.get(name) {
            return self.read_function(state, pc, function);
        }
        for (index, host) in self.program.hosts.iter().enumerate() {
            self.ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                self.issue(pc, IssueKind::DetachedValue(Target::Host(index)))?;
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                return Ok(false);
            }
        }
        for (index, (global, _)) in self.program.globals.iter().enumerate() {
            self.ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                return self.read_global(state, pc, index, None);
            }
        }
        if let Some(module) = self.function.namespace {
            let receiver = self.namespace_value(module)?;
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
                state.join(self.ctx, self.facts, &present, false)?;
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
