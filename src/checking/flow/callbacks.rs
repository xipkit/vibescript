use super::*;
use blocks::{Closure, Completion, Link};

impl Walker<'_> {
    pub(super) fn given(&self) -> bool {
        self.incoming.is_some() || self.block_inputs.is_some_and(|b| b.given)
    }

    pub(super) fn attach(&mut self, state: &mut State, function: usize) -> Result<bool> {
        let mut captures = Buffer::empty();
        for (slot, capture) in self.program.functions[function].captures.iter().enumerate() {
            self.ctx.charge(1)?;
            let Some(capture) = capture else {
                continue;
            };
            if capture.depth != 0 {
                return Ok(false);
            }
            let binding = state.locals.get(self.ctx, capture.slot)?;
            if binding.missing {
                if binding.value != Atom::Never.fact() {
                    return Ok(false);
                }
                continue;
            }
            captures.push(
                self.ctx,
                Link {
                    slot,
                    parent: capture.slot,
                    value: binding.value,
                },
            )?;
        }
        state.arguments.data.last_mut().unwrap().arguments.block = Some(Closure {
            function,
            given: self.given(),
            captures,
        });
        Ok(true)
    }

    pub(super) fn call_exits(
        &mut self,
        state: &State,
        pc: usize,
        block: &Closure,
        exits: Buffer<blocks::Exit>,
    ) -> Result<()> {
        for exit in exits.data {
            self.ctx.charge(1)?;
            let mut next = state.snapshot(self.ctx)?;
            let mut supported = true;
            for link in &block.captures.data {
                if exit.written.get(self.ctx, link.slot)? {
                    // A final value does not distinguish mutation from root replacement.
                    // Pending addresses require the ordered mutation history of the callback.
                    for address in &next.addresses.data {
                        self.ctx.charge(1)?;
                        if address.root == Some(link.parent) {
                            supported = false;
                        }
                    }
                    let value = exit.captures.get(self.ctx, link.slot)?;
                    next.store(self.ctx, self.facts, link.parent, value)?;
                }
            }
            if !supported {
                self.incomplete(pc)?;
                continue;
            }
            match exit.completion {
                Completion::Value => {
                    next.stack.push(self.ctx, Operand::new(exit.value))?;
                    self.extra.push(self.ctx, (pc + 1, next))?;
                }
                Completion::Return => {
                    let transfer = if self.block_inputs.is_some() {
                        Transfer::Block {
                            pc,
                            completion: Completion::Return,
                            value: exit.value,
                        }
                    } else {
                        Transfer::Return {
                            pc,
                            value: exit.value,
                        }
                    };
                    let edges = self.transfer(next, pc, transfer)?;
                    for edge in edges.into_iter().flatten() {
                        self.extra.push(self.ctx, edge)?;
                    }
                }
                Completion::Error(class) => self.emit_error(&next, pc, handlers::bit(class))?,
                Completion::Break(_) => unreachable!("receiving functions consume block breaks"),
            }
        }
        Ok(())
    }

    pub(super) fn yield_block(
        &mut self,
        mut state: State,
        pc: usize,
        count: usize,
    ) -> Result<Edges> {
        let Some(incoming) = self.incoming else {
            return self.incomplete(pc);
        };
        let mut block = incoming.snapshot(self.ctx)?;
        for link in &mut block.captures.data {
            link.value = state
                .incoming
                .as_ref()
                .unwrap()
                .value(self.ctx, link.slot)?;
        }
        let mut args = Arguments::new();
        let base = state.stack.data.len() - count;
        for operand in &state.stack.data[base..] {
            self.ctx.charge(1)?;
            args.positional.push(self.ctx, operand.value)?;
        }
        state.stack.data.truncate(base);
        args.block = Some(block);
        let current_error = state.current_error(self.ctx, self.current_error)?;
        let result = self.calls.invoke(
            self.ctx,
            self.facts,
            Target::Block(incoming.function),
            args,
            current_error,
        )?;
        if result.incomplete {
            return self.incomplete(pc);
        }
        assert!(result.failures.data.is_empty());
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let mut next = state.snapshot(self.ctx)?;
            for link in &incoming.captures.data {
                if exit.written.get(self.ctx, link.slot)? {
                    let value = exit.captures.get(self.ctx, link.slot)?;
                    next.incoming
                        .as_mut()
                        .unwrap()
                        .store(self.ctx, self.facts, link.slot, value)?;
                }
            }
            let transfer = match exit.completion {
                Completion::Value => {
                    next.stack.push(self.ctx, Operand::new(exit.value))?;
                    self.extra.push(self.ctx, (pc + 1, next))?;
                    continue;
                }
                Completion::Error(class) => {
                    self.emit_error(&next, pc, handlers::bit(class))?;
                    continue;
                }
                Completion::Return => Transfer::Block {
                    pc,
                    completion: Completion::Return,
                    value: exit.value,
                },
                Completion::Break(supplied) => {
                    if let Some(current) = next.loops.data.last() {
                        let value = if supplied {
                            exit.value
                        } else if current.expression {
                            Atom::Nil.fact()
                        } else {
                            current.last
                        };
                        Transfer::Jump {
                            target: current.end,
                            index: next.loops.data.len() - 1,
                            breaking: true,
                            value,
                        }
                    } else {
                        Transfer::Return {
                            pc,
                            value: exit.value,
                        }
                    }
                }
            };
            let edges = self.transfer(next, pc, transfer)?;
            for edge in edges.into_iter().flatten() {
                self.extra.push(self.ctx, edge)?;
            }
        }
        Ok([None, None])
    }
}
