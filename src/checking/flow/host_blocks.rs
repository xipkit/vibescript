use super::*;
use crate::checking::calls::{HostBoundary, Outcome};
use blocks::{Closure, Completion};

impl Walker<'_> {
    pub(super) fn host_block(
        &mut self,
        state: &State,
        pc: usize,
        index: usize,
        mut args: Arguments,
    ) -> Result<()> {
        let target = Target::Host(index);
        let mut admission = Outcome::empty();
        let admitted = args.admit(self.ctx, self.facts, &mut admission.failures)?;
        self.call_effects(state, pc, target, &admission)?;
        if !admitted {
            return Ok(());
        }
        let globals = state.global_call(self.ctx)?;
        let guard = self.calls.host_boundary(
            self.ctx,
            self.facts,
            index,
            HostBoundary::Arguments(&args),
            &globals,
        )?;
        self.call_effects(state, pc, target, &guard)?;
        if guard.incomplete {
            self.incomplete(pc)?;
            return Ok(());
        }
        if guard.value == Atom::Never.fact() {
            return Ok(());
        }
        if !self.calls.host_uses_block(self.ctx, index)? {
            return self.host_finish(state, pc, index, None);
        }
        let block = args.block.as_ref().unwrap();
        let first = self.host_step(state, pc, index, block)?;
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
            for next in self.host_step(&state, pc, index, block)?.data {
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

    fn host_step(
        &mut self,
        state: &State,
        pc: usize,
        index: usize,
        block: &Closure,
    ) -> Result<Buffer<State>> {
        self.host_finish(state, pc, index, None)?;
        let mut callback = block.snapshot(self.ctx)?;
        self.prepare_callback(state, &mut callback)?;
        let mut args = Arguments::new();
        for _ in 0..self.program.functions[block.function].block_arity {
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
        if result.incomplete {
            self.incomplete(pc)?;
        }
        let mut repeat = Buffer::empty();
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let mut next = self.capture_exit(state, &callback, &exit)?;
            next.apply_globals(self.ctx, self.facts, &exit.globals)?;
            match exit.completion {
                Completion::Value | Completion::Error(_) => {
                    // A host can ignore an ordinary block error and invoke the
                    // block again with the writes completed before that error.
                    repeat.push(self.ctx, next)?;
                }
                Completion::Break(_) => self.host_finish(&next, pc, index, Some(exit.value))?,
                Completion::Return(depth) => self.callback_return(next, pc, depth, exit.value)?,
            }
        }
        Ok(repeat)
    }

    fn host_finish(
        &mut self,
        state: &State,
        pc: usize,
        index: usize,
        value: Option<Fact>,
    ) -> Result<()> {
        let globals = state.global_call(self.ctx)?;
        let result = self.calls.host_boundary(
            self.ctx,
            self.facts,
            index,
            HostBoundary::Result(value),
            &globals,
        )?;
        self.call_effects(state, pc, Target::Host(index), &result)?;
        if result.incomplete {
            self.incomplete(pc)?;
        }
        if result.value != Atom::Never.fact() {
            let mut next = state.snapshot(self.ctx)?;
            next.stack.push(self.ctx, Operand::new(result.value))?;
            self.native_continue(pc, next)?;
        }
        Ok(())
    }
}
