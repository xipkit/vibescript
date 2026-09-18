use super::*;
use blocks::{Closure, Completion, Link, Parent};

pub(super) fn local_count(
    ctx: &mut CallContext,
    layouts: &Layouts,
    program: &Program,
    function: usize,
    ambient: Option<usize>,
) -> Result<usize> {
    let count = ambient.map_or(0, |parent| program.functions[parent].local_names.len());
    match layouts.locals(ctx, program, function)?.checked_add(count) {
        Some(count) => Ok(count),
        None => ctx.fail(
            crate::ErrorKind::Memory,
            "checker ambient layout size overflow",
        ),
    }
}

impl Walker<'_> {
    pub(super) fn local_count(&mut self, function: usize) -> Result<usize> {
        local_count(self.ctx, self.layouts, self.program, function, self.ambient)
    }

    pub(super) fn ambient_binding(
        &mut self,
        state: &State,
        name: &str,
    ) -> Result<Option<(usize, Binding)>> {
        let Some(parent) = self.ambient else {
            return Ok(None);
        };
        let base = self
            .layouts
            .locals(self.ctx, self.program, self.function_index)?;
        for (index, candidate) in self.program.functions[parent]
            .local_names
            .iter()
            .enumerate()
        {
            self.ctx.work_bytes(candidate.len().max(name.len()))?;
            if candidate == name {
                let slot = base + index;
                let binding = state.locals.get(self.ctx, slot)?;
                return Ok((binding.value != Atom::Never.fact()).then_some((slot, binding)));
            }
        }
        Ok(None)
    }

    pub(super) fn ambient_op(&mut self, state: &State, op: Op) -> Result<Option<Op>> {
        if !self.function.initializer || self.ambient.is_none() {
            return Ok(Some(op));
        }
        let slot = match op {
            Op::Load(slot)
            | Op::LoadOptional(slot, _)
            | Op::ReceiverBound(slot, _)
            | Op::Declare(slot)
            | Op::Store(slot)
            | Op::AddStore(slot)
            | Op::AddressLocal(slot)
            | Op::AddressBound(slot, _)
            | Op::ResolveCall(slot, _, _)
            | Op::CallName(slot, _)
            | Op::Bypass(slot) => slot,
            _ => return Ok(Some(op)),
        };
        if slot == usize::MAX || !state.locals.get(self.ctx, slot)?.missing {
            return Ok(Some(op));
        }
        let name = &self.function.local_names[slot];
        let Some((ambient, binding)) = self.ambient_binding(state, name)? else {
            return Ok(Some(op));
        };
        if binding.missing || state.locals.get(self.ctx, slot)?.value != Atom::Never.fact() {
            return Ok(None);
        }
        Ok(Some(match op {
            Op::Load(_) => Op::Load(ambient),
            Op::LoadOptional(_, name) => Op::LoadOptional(ambient, name),
            Op::ReceiverBound(_, next) => Op::ReceiverBound(ambient, next),
            Op::Declare(_) => Op::Declare(ambient),
            Op::Store(_) => Op::Store(ambient),
            Op::AddStore(_) => Op::AddStore(ambient),
            Op::AddressLocal(_) => Op::AddressLocal(ambient),
            Op::AddressBound(_, next) => Op::AddressBound(ambient, next),
            Op::ResolveCall(_, name, parens) => Op::ResolveCall(ambient, name, parens),
            Op::CallName(_, name) => Op::CallName(ambient, name),
            Op::Bypass(_) => Op::Bypass(ambient),
            _ => unreachable!(),
        }))
    }

    pub(super) fn ambient_edges(
        &mut self,
        mut state: State,
        pc: usize,
        name: usize,
        next: usize,
        address: bool,
    ) -> Result<Edges> {
        let name = &self.program.members[name];
        if address
            && name
                .chars()
                .next()
                .is_some_and(crate::syntax::unicode::upper)
        {
            return Ok([Some((pc + 1, state)), None]);
        }
        let Some((slot, binding)) = self.ambient_binding(&state, name)? else {
            return Ok([Some((pc + 1, state)), None]);
        };
        let missing = if binding.missing {
            let mut missing = state.snapshot(self.ctx)?;
            missing.locals.set(
                self.ctx,
                slot,
                Binding {
                    value: Atom::Never.fact(),
                    ..binding
                },
            )?;
            Some((pc + 1, missing))
        } else {
            None
        };
        state.locals.set(
            self.ctx,
            slot,
            Binding {
                missing: false,
                ..binding
            },
        )?;
        if address {
            state
                .addresses
                .push(self.ctx, Address::new(Some(slot), binding.value))?;
        } else {
            state
                .stack
                .push(self.ctx, Operand::local(binding.value, slot))?;
        }
        Ok([Some((next, state)), missing])
    }

    pub(super) fn initialize_with_ambient(
        &mut self,
        state: &State,
        pc: usize,
        function: usize,
    ) -> Result<()> {
        let mut pending = Buffer::empty();
        let initial = state.snapshot(self.ctx)?;
        pending.push(self.ctx, (initial, 0))?;
        while let Some((mut state, start)) = pending.data.pop() {
            self.ctx.charge(1)?;
            let mut split = None;
            for slot in start..self.function.local_names.len() {
                let binding = state.locals.get(self.ctx, slot)?;
                if binding.missing && binding.value != Atom::Never.fact() {
                    split = Some((slot, binding));
                    break;
                }
            }
            if let Some((slot, binding)) = split {
                let mut present = state.snapshot(self.ctx)?;
                present.locals.set(
                    self.ctx,
                    slot,
                    Binding {
                        missing: false,
                        ..binding
                    },
                )?;
                state.locals.set(
                    self.ctx,
                    slot,
                    Binding {
                        value: Atom::Never.fact(),
                        ..binding
                    },
                )?;
                pending.push(self.ctx, (present, slot + 1))?;
                pending.push(self.ctx, (state, slot + 1))?;
            } else {
                self.initialize_variant(&state, pc, function)?;
            }
        }
        Ok(())
    }

    fn initialize_variant(&mut self, state: &State, pc: usize, function: usize) -> Result<()> {
        let base = self.layouts.locals(self.ctx, self.program, function)?;
        let locals = local_count(
            self.ctx,
            self.layouts,
            self.program,
            function,
            Some(self.function_index),
        )?;
        let mut body = Closure {
            scope: blocks::Scope::Invocation,
            function: self.source.callable(function),
            receiver: None,
            ambient: Some(self.source.callable(self.function_index)),
            given: false,
            locals,
            inherited: Buffer::empty(),
            captures: Buffer::empty(),
            pending: super::super::pending::Pending::new(),
            destinations: Buffer::empty(),
        };
        for slot in 0..self.function.local_names.len() {
            self.ctx.charge(1)?;
            let binding = state.locals.get(self.ctx, slot)?;
            body.captures.push(
                self.ctx,
                Link {
                    slot: base + slot,
                    parent: Parent::Local(slot),
                    value: binding.value,
                    missing: binding.missing,
                    owner: binding.owner,
                },
            )?;
        }
        self.prepare_callback(state, &mut body)?;
        let globals = state.global_call(self.ctx)?;
        let error = state.current_error(self.ctx, self.current_error)?;
        let result = self
            .calls
            .initialize(self.ctx, self.facts, &body, error, &globals)?;
        if result.incomplete {
            self.incomplete(pc)?;
            return Ok(());
        }
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let mut next = self.capture_exit(state, &body, &exit)?;
            next.apply_globals(self.ctx, self.facts, &exit.globals)?;
            match exit.completion {
                Completion::Value => self.native_continue(pc, next)?,
                Completion::Error(class) => self.emit_error(&next, pc, handlers::bit(class))?,
                Completion::Escape => self.callback_escape(next, pc, exit.value)?,
                Completion::Return(_) | Completion::Break(_) => {
                    self.incomplete(pc)?;
                }
            }
        }
        Ok(())
    }
}
