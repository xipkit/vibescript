use super::*;
use crate::checking::facts::{Callable, Node};

impl Walker<'_> {
    pub(super) fn import_root(
        &mut self,
        state: &mut State,
        pc: usize,
        slot: usize,
    ) -> Result<bool> {
        let Some(root) = state
            .source_slots
            .root(self.ctx, slot - state.global_base)?
        else {
            return Ok(true);
        };
        let mut binding = state.locals.get(self.ctx, slot)?;
        if binding.missing {
            let globals = state.global_call(self.ctx)?;
            let current_error = state.current_error(self.ctx, self.current_error)?;
            let loaded =
                self.calls
                    .admit_root(self.ctx, self.facts, root, current_error, &globals)?;
            self.emit_error(state, pc, loaded.throws)?;
            if loaded.incomplete {
                self.incomplete(pc)?;
                return Ok(false);
            }
            if !loaded.exits.data.is_empty() {
                let mut normal: Option<State> = None;
                for exit in loaded.exits.data {
                    let mut next = state.snapshot(self.ctx)?;
                    next.apply_globals(self.ctx, self.facts, &exit.globals)?;
                    match exit.completion {
                        blocks::Completion::Value => {
                            if let Some(normal) = &mut normal {
                                if !normal.compatible(self.ctx, &next)? {
                                    self.incomplete(pc)?;
                                    return Ok(false);
                                }
                                normal.join(self.ctx, self.facts, &next, false, self.program)?;
                            } else {
                                normal = Some(next);
                            }
                        }
                        blocks::Completion::Error(class) => {
                            self.emit_error(&next, pc, handlers::bit(class))?
                        }
                        blocks::Completion::Escape => self.callback_escape(next, pc, exit.value)?,
                        _ => unreachable!(),
                    }
                }
                let Some(normal) = normal else {
                    return Ok(false);
                };
                *state = normal;
            }
            let value = self.facts.union(self.ctx, &[binding.value, loaded.value])?;
            if value == Atom::Never.fact() {
                return Ok(false);
            }
            state.store(self.ctx, self.facts, slot, value)?;
            binding = state.locals.get(self.ctx, slot)?;
        }
        if !self.facts.escapes(binding.value) {
            return Ok(true);
        }
        let mut values = Buffer::empty();
        let mut rejected = false;
        for i in 0..self.facts.arm_count(binding.value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(binding.value, i);
            let value = if matches!(
                self.facts.node(arm),
                Node::Callable {
                    target: Callable::Host(_),
                    ..
                }
            ) {
                arm
            } else {
                rejected |= self.facts.escapes(arm);
                self.facts.exported(self.ctx, arm)?
            };
            values.push(self.ctx, value)?;
        }
        if rejected {
            self.issue(pc, IssueKind::DetachedValue(Target::Value(binding.value)))?;
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        let value = self.facts.union(self.ctx, &values.data)?;
        state
            .locals
            .set(self.ctx, slot, Binding { value, ..binding })?;
        Ok(value != Atom::Never.fact())
    }

    pub(super) fn root_read_slot(&mut self, state: &State, op: Op) -> Result<Option<usize>> {
        if self.program.file && matches!(op, Op::RootAddress(..)) {
            return Ok(None);
        }
        let name = match op {
            Op::Load(slot)
            | Op::LoadOptional(slot, _)
            | Op::ReceiverBound(slot, _)
            | Op::AddressLocal(slot)
            | Op::AddressBound(slot, _) => {
                return match slot.checked_sub(state.global_base) {
                    Some(index) => Ok(state.source_slots.root(self.ctx, index)?.map(|_| slot)),
                    None => Ok(None),
                };
            }
            Op::Global(index)
            | Op::GlobalReceiver(index, _)
            | Op::AddressGlobal(index)
            | Op::ResolveGlobalCall(index) => self.program.globals[index].0.name(),
            Op::RootAddress(name, _) | Op::Unbound(name) | Op::RootCall(name, _) => {
                &self.program.members[name]
            }
            Op::ResolveCall(slot, name, _) | Op::CallName(slot, name) => {
                if slot != usize::MAX
                    && state.locals.get(self.ctx, slot)?.value != Atom::Never.fact()
                {
                    return Ok(None);
                }
                if self
                    .namespace_constant(
                        state,
                        &self.program.members[name],
                        matches!(op, Op::ResolveCall(..)),
                    )?
                    .is_some()
                {
                    return Ok(None);
                }
                &self.program.members[name]
            }
            Op::AutoCall(function) => &self.program.functions[function].name,
            Op::HostValue(host) => &self.program.hosts[host],
            Op::Declaration(index) => match &self.program.declarations[index].0 {
                Kind::Enum(value) => &value.definition.name,
                Kind::Namespace(value) => &value.definition.name,
                _ => return Ok(None),
            },
            _ => return Ok(None),
        };
        if self.ambient_binding(state, name)?.is_some() {
            return Ok(None);
        }
        if self.program.file {
            if self.file_binding(state, name)?.is_some() {
                return Ok(None);
            }
            if matches!(
                op,
                Op::ResolveCall(..) | Op::CallName(..) | Op::Declaration(_)
            ) && self.file_target(state, name)?.is_some()
            {
                return Ok(None);
            }
        }
        Ok(self
            .root_index(state, name)?
            .map(|index| state.global_base + index))
    }

    pub(super) fn export_value(
        &mut self,
        state: &State,
        pc: usize,
        value: Fact,
        target: bool,
    ) -> Result<Fact> {
        if !self.facts.escapes(value) {
            return Ok(value);
        }
        let mut values = Buffer::empty();
        let mut rejected = false;
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let next = if target && matches!(self.facts.node(arm), Node::Callable { .. }) {
                arm
            } else {
                rejected |= self.facts.escapes(arm);
                self.facts.exported(self.ctx, arm)?
            };
            values.push(self.ctx, next)?;
        }
        if rejected {
            self.issue(pc, IssueKind::DetachedValue(Target::Value(value)))?;
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        self.facts.union(self.ctx, &values.data)
    }

    pub(super) fn export_stack(&mut self, state: &mut State, pc: usize, op: Op) -> Result<bool> {
        let Some(value) = state.stack.data.last().map(|value| value.value) else {
            return Ok(true);
        };
        let value = self.export_value(state, pc, value, matches!(op, Op::CallValue))?;
        state.stack.data.last_mut().unwrap().value = value;
        Ok(value != Atom::Never.fact())
    }

    pub(super) fn read_attached(
        &mut self,
        state: &mut State,
        pc: usize,
        value: Fact,
        origin: Option<usize>,
    ) -> Result<bool> {
        let mut normal: Option<State> = None;
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let mut next = state.snapshot(self.ctx)?;
            let readable = match *self.facts.node(arm) {
                Node::Callable {
                    target: Callable::Host(_),
                    ..
                } => {
                    let target = self.value_target(arm)?;
                    self.issue(pc, IssueKind::DetachedValue(target))?;
                    self.emit_error(&next, pc, handlers::bit(ErrorClass::Runtime))?;
                    false
                }
                Node::Callable { .. } => {
                    let Target::Function(function) = self.value_target(arm)? else {
                        self.incomplete(pc)?;
                        return Ok(false);
                    };
                    self.read_function(&mut next, pc, function)?
                }
                _ => self.read_value(&mut next, pc, arm, origin)?,
            };
            if readable {
                if let Some(normal) = &mut normal {
                    if !normal.compatible(self.ctx, &next)? {
                        self.native_continue(pc, next)?;
                    } else {
                        normal.join(self.ctx, self.facts, &next, false, self.program)?;
                    }
                } else {
                    normal = Some(next);
                }
            }
        }
        if let Some(normal) = normal {
            *state = normal;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
