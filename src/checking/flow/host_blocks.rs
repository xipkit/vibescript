use super::*;
use crate::checking::calls::{HostBoundary, Outcome};
use blocks::{Closure, Completion};

#[derive(Clone, Copy)]
enum CallbackTarget {
    Host(CallableId),
    Dynamic,
    Mutation { address: bool },
}

impl CallbackTarget {
    fn dynamic(self) -> bool {
        !matches!(self, Self::Host(_))
    }
}

impl Walker<'_> {
    pub(super) fn dynamic_call(
        &mut self,
        state: &mut State,
        pc: usize,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        self.dynamic_invoke(state, pc, args, CallbackTarget::Dynamic)
    }

    pub(super) fn dynamic_mutation(
        &mut self,
        state: &mut State,
        pc: usize,
        site: MemberSite,
        args: Arguments,
        address: bool,
    ) -> Result<Option<Edges>> {
        let selected = state.addresses.data.last().unwrap();
        let protection = selected.protection(self.ctx, self.facts)?;
        if protection.readonly != Attached::No {
            if protection.report {
                self.collection_error(state, pc, selected.value, site, &args, ErrorClass::Runtime)?;
            } else {
                self.protected_error(state, pc, selected)?;
            }
            if protection.readonly == Attached::Yes {
                return Ok(Some([None, None]));
            }
        }
        self.dynamic_invoke(state, pc, args, CallbackTarget::Mutation { address })
    }

    /// Calls a member whose name is known only at runtime on the addressed
    /// receiver. Any member, or none, may run: it may mutate the receiver,
    /// call the block, have unknown effects and raise any error class.
    pub(super) fn dynamic_member(
        &mut self,
        state: &mut State,
        pc: usize,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        self.dynamic_invoke(state, pc, args, CallbackTarget::Mutation { address: false })
    }

    fn dynamic_invoke(
        &mut self,
        state: &mut State,
        pc: usize,
        mut args: Arguments,
        target: CallbackTarget,
    ) -> Result<Option<Edges>> {
        let mut admission = Outcome::empty();
        let admitted = args.admit(self.ctx, self.facts, &mut admission.failures)?;
        self.call_effects(state, pc, Target::Dynamic, &admission)?;
        if !admitted {
            return Ok(Some([None, None]));
        }
        if let Some(block) = args.block {
            self.callback_schedule(state, pc, target, &block)?;
            return Ok(Some([None, None]));
        }
        if matches!(target, CallbackTarget::Mutation { .. }) {
            self.callback_finish(state, pc, target, None)?;
            return Ok(Some([None, None]));
        }
        self.unknown_call_effects(state, pc)?;
        self.emit_error(state, pc, u8::MAX)?;
        state
            .stack
            .push(self.ctx, Operand::new(Atom::Unknown.fact()))?;
        Ok(None)
    }

    pub(super) fn host_block(
        &mut self,
        state: &mut State,
        pc: usize,
        index: CallableId,
        mut args: Arguments,
    ) -> Result<()> {
        let target = Target::Host(index);
        let mut admission = Outcome::empty();
        let admitted = args.admit(self.ctx, self.facts, &mut admission.failures)?;
        self.call_effects(state, pc, target, &admission)?;
        if !admitted {
            return Ok(());
        }
        let guard = loop {
            let globals = state.global_call(self.ctx)?;
            let guard = self.calls.host_boundary(
                self.ctx,
                self.facts,
                index,
                HostBoundary::Arguments(&args),
                &globals,
            )?;
            let Some(root) = guard.pending else {
                break guard;
            };
            self.call_effects(state, pc, target, &guard)?;
            let slot = state.global_base + state.source_slots.roots.data[root];
            let Some(alternatives) = self.import_root_branches(state, pc, slot)? else {
                return Ok(());
            };
            for mut next in alternatives.data {
                let args = args.snapshot(self.ctx)?;
                self.host_block(&mut next, pc, index, args)?;
            }
        };
        self.call_effects(state, pc, target, &guard)?;
        if guard.incomplete {
            self.incomplete(pc)?;
            return Ok(());
        }
        if guard.value == Atom::Never.fact() {
            return Ok(());
        }
        if !self.calls.host_uses_block(self.ctx, index)? {
            return self.callback_finish(state, pc, CallbackTarget::Host(index), None);
        }
        let block = args.block.as_ref().unwrap();
        self.callback_schedule(state, pc, CallbackTarget::Host(index), block)
    }

    fn callback_schedule(
        &mut self,
        state: &State,
        pc: usize,
        target: CallbackTarget,
        block: &Closure,
    ) -> Result<()> {
        let first = self.callback_step(state, pc, target, block)?;
        // Keep the zero-invocation state separate from repeated invocations, just
        // as native collection loops retain their first completed iteration.
        let mut depth = self.host_depth(state, block)?;
        for next in &first.data {
            depth = depth.max(self.host_depth(next, block)?);
        }
        let mut states: Buffer<State> = Buffer::empty();
        let mut pending = Buffer::empty();
        for next in first.data {
            self.host_enqueue(&mut states, &mut pending, next, depth)?;
        }
        while let Some(position) = pending.data.pop() {
            self.ctx.charge(1)?;
            let state = states.data[position].snapshot(self.ctx)?;
            for next in self.callback_step(&state, pc, target, block)?.data {
                self.host_enqueue(&mut states, &mut pending, next, depth)?;
            }
        }
        Ok(())
    }

    fn host_depth(&mut self, state: &State, block: &Closure) -> Result<usize> {
        let mut depth = 0;
        for &contract in self.contracts {
            self.ctx.charge(1)?;
            depth = depth.max(self.facts.depth(contract));
        }
        for slot in 0..state.global_base + state.global_count {
            let value = state.locals.get(self.ctx, slot)?.value;
            depth = depth.max(self.facts.depth(value));
        }
        for link in &block.captures.data {
            self.ctx.charge(1)?;
            if let blocks::Parent::Capture(slot) = link.parent {
                let value = state.captures.as_ref().unwrap().value(self.ctx, slot)?;
                depth = depth.max(self.facts.depth(value));
            }
        }
        Ok(depth)
    }

    fn host_enqueue(
        &mut self,
        states: &mut Buffer<State>,
        pending: &mut Buffer<usize>,
        mut next: State,
        depth: usize,
    ) -> Result<()> {
        for (index, state) in states.data.iter_mut().enumerate() {
            self.ctx.charge(1)?;
            if state.compatible(self.ctx, &next)? {
                if state.join(self.ctx, self.facts, &next, true, self.program)? {
                    self.ctx.charge(pending.data.len() as u64)?;
                    if !pending.data.contains(&index) {
                        pending.push(self.ctx, index)?;
                    }
                }
                return Ok(());
            }
        }
        next.widening = Some(depth);
        pending.push(self.ctx, states.data.len())?;
        states.push(self.ctx, next)
    }

    fn callback_step(
        &mut self,
        state: &State,
        pc: usize,
        target: CallbackTarget,
        block: &Closure,
    ) -> Result<Buffer<State>> {
        let entry = if target.dynamic() {
            let mut entry = state.snapshot(self.ctx)?;
            self.dynamic_effects(&mut entry, pc, target)?;
            Some(entry)
        } else {
            None
        };
        let state = entry.as_ref().unwrap_or(state);
        self.callback_finish(state, pc, target, None)?;
        let mut callback = block.snapshot(self.ctx)?;
        self.prepare_callback(state, &mut callback)?;
        let mut args = Arguments::new();
        for _ in 0..self.block_arity(block.function)? {
            args.positional.push(self.ctx, Atom::Unknown.fact())?;
        }
        args.block = Some(callback.snapshot(self.ctx)?);
        let globals = state.global_call(self.ctx)?;
        let current_error = state.current_error(self.ctx, self.current_error)?;
        let result = self.calls.invoke(
            self.ctx,
            self.facts,
            Target::Block(block.function),
            args,
            current_error,
            &globals,
        )?;
        self.call_effects(state, pc, Target::Block(block.function), &result)?;
        // Block summaries that fold the caller's objects are not modeled here.
        if result.incomplete || !result.folds.data.is_empty() {
            self.incomplete(pc)?;
        }
        let mut repeat = Buffer::empty();
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let mut next = self.capture_exit(state, &callback, &exit)?;
            next.apply_globals(self.ctx, self.facts, &exit.globals)?;
            // Unknown script cleanup can replace a transfer or yield again.
            // Known host callbacks keep transfers latched instead.
            match exit.completion {
                Completion::Value | Completion::Error(_) => {
                    // A host can ignore an ordinary block error and invoke the
                    // block again with the writes completed before that error.
                    repeat.push(self.ctx, next)?;
                }
                Completion::Break(_) => {
                    self.callback_finish(&next, pc, target, Some(exit.value))?;
                    if target.dynamic() {
                        repeat.push(self.ctx, next)?;
                    }
                }
                Completion::Return(depth) => {
                    if target.dynamic() {
                        self.dynamic_effects(&mut next, pc, target)?;
                        self.emit_error(&next, pc, u8::MAX)?;
                        let cleanup = next.snapshot(self.ctx)?;
                        repeat.push(self.ctx, cleanup)?;
                    }
                    self.callback_return(next, pc, depth, exit.value)?;
                }
                Completion::Escape => {
                    if target.dynamic() {
                        self.dynamic_effects(&mut next, pc, target)?;
                        self.emit_error(&next, pc, u8::MAX)?;
                        let cleanup = next.snapshot(self.ctx)?;
                        repeat.push(self.ctx, cleanup)?;
                    }
                    self.callback_escape(next, pc, exit.value)?;
                }
            }
        }
        Ok(repeat)
    }

    fn callback_finish(
        &mut self,
        state: &State,
        pc: usize,
        target: CallbackTarget,
        value: Option<Fact>,
    ) -> Result<()> {
        let mut state = state.snapshot(self.ctx)?;
        let CallbackTarget::Host(index) = target else {
            self.dynamic_effects(&mut state, pc, target)?;
            self.emit_error(&state, pc, u8::MAX)?;
            let value = value.unwrap_or(Atom::Unknown.fact());
            if let CallbackTarget::Mutation { address } = target {
                state.addresses.data.pop().unwrap();
                if address {
                    state.addresses.push(self.ctx, Address::new(None, value))?;
                    return self.native_continue(pc, state);
                }
            }
            state.stack.push(self.ctx, Operand::new(value))?;
            return self.native_continue(pc, state);
        };
        let result = loop {
            let globals = state.global_call(self.ctx)?;
            let result = self.calls.host_boundary(
                self.ctx,
                self.facts,
                index,
                HostBoundary::Result(value),
                &globals,
            )?;
            let Some(root) = result.pending else {
                break result;
            };
            self.call_effects(&state, pc, Target::Host(index), &result)?;
            let slot = state.global_base + state.source_slots.roots.data[root];
            let Some(alternatives) = self.import_root_branches(&mut state, pc, slot)? else {
                return Ok(());
            };
            for next in alternatives.data {
                self.callback_finish(&next, pc, target, value)?;
            }
        };
        self.call_effects(&state, pc, Target::Host(index), &result)?;
        if result.incomplete {
            self.incomplete(pc)?;
        }
        if result.value != Atom::Never.fact() {
            state.stack.push(self.ctx, Operand::new(result.value))?;
            self.native_continue(pc, state)?;
        }
        Ok(())
    }

    fn dynamic_effects(
        &mut self,
        state: &mut State,
        pc: usize,
        target: CallbackTarget,
    ) -> Result<()> {
        self.unknown_call_effects(state, pc)?;
        if matches!(target, CallbackTarget::Mutation { .. }) {
            if let Some(root) = state.addresses.data.last().unwrap().root {
                if root < state.global_base {
                    // The unknown method can write before, between or after callbacks.
                    // Broaden the addressed local too, including a parent rebound by a block.
                    let before = state.locals.get(self.ctx, root)?.value;
                    let value = self
                        .facts
                        .union(self.ctx, &[before, Atom::Unknown.fact()])?;
                    self.store(state, pc, root, Operand::new(value))?;
                }
            }
        }
        Ok(())
    }
}
