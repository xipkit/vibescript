use super::*;
use blocks::{Closure, Completion, Layer, Link, Parent};

impl Walker<'_> {
    pub(super) fn given(&self) -> bool {
        self.incoming.is_some() || self.block_inputs.is_some_and(|b| b.given)
    }

    fn incoming_base(&mut self) -> Result<usize> {
        if self.block_inputs.is_some() {
            self.layouts
                .locals(self.ctx, self.program, self.function_index)
        } else {
            self.ctx.charge(1)?;
            Ok(0)
        }
    }

    pub(super) fn attach(&mut self, state: &mut State, function: usize) -> Result<bool> {
        let mut captures = Buffer::empty();
        let locals = self.layouts.locals(self.ctx, self.program, function)?;
        for slot in 0..locals {
            self.ctx.charge(1)?;
            let capture = self
                .layouts
                .capture(self.ctx, self.program, function, slot)?;
            let Some(capture) = capture else {
                continue;
            };
            let parent = if capture.depth == 0 {
                capture.slot
            } else {
                let source = crate::bytecode::Capture {
                    depth: capture.depth - 1,
                    slot: capture.slot,
                };
                let Some(parent) =
                    self.layouts
                        .find(self.ctx, self.program, self.function_index, source)?
                else {
                    return Ok(false);
                };
                parent
            };
            let binding = state.locals.get(self.ctx, parent)?;
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
                    parent: Parent::Local(parent),
                    value: binding.value,
                },
            )?;
        }
        let mut inherited = Buffer::empty();
        let forwarding = self.layouts.forwarding(self.ctx, function)?;
        if let Some(incoming) = self.incoming.filter(|_| forwarding) {
            let base = self.incoming_base()?;
            inherited.push(
                self.ctx,
                Layer {
                    function: incoming.function,
                    given: incoming.given,
                    locals: incoming.locals,
                },
            )?;
            inherited.extend(self.ctx, &incoming.inherited.data)?;
            // Check the full extent before adding offsets to the individual capture slots.
            blocks::extent(self.ctx, locals, &inherited.data)?;
            for link in &incoming.captures.data {
                self.ctx.charge(1)?;
                let parent = base + link.slot;
                let value = state.captures.as_ref().unwrap().value(self.ctx, parent)?;
                captures.push(
                    self.ctx,
                    Link {
                        slot: locals + link.slot,
                        parent: Parent::Capture(parent),
                        value,
                    },
                )?;
            }
        }
        state.arguments.data.last_mut().unwrap().arguments.block = Some(Closure {
            function,
            given: self.given(),
            locals,
            inherited,
            captures,
        });
        Ok(true)
    }

    pub(super) fn capture_exit(
        &mut self,
        state: &State,
        pc: usize,
        block: &Closure,
        exit: &blocks::Exit,
    ) -> Result<Option<State>> {
        let mut next = state.snapshot(self.ctx)?;
        let mut supported = true;
        for link in &block.captures.data {
            if exit.written.get(self.ctx, link.slot)? {
                let value = exit.captures.get(self.ctx, link.slot)?;
                let parent = match link.parent {
                    Parent::Local(parent) => parent,
                    Parent::Capture(parent) => {
                        next.captures
                            .as_mut()
                            .unwrap()
                            .store(self.ctx, self.facts, parent, value)?;
                        continue;
                    }
                };
                // A final value does not distinguish mutation from root replacement.
                // Pending addresses require the ordered mutation history of the callback.
                for address in &next.addresses.data {
                    self.ctx.charge(1)?;
                    if address.root == Some(parent) {
                        supported = false;
                    }
                }
                next.store(self.ctx, self.facts, parent, value)?;
            }
        }
        if !supported {
            self.incomplete(pc)?;
            return Ok(None);
        }
        Ok(Some(next))
    }

    pub(super) fn callback_return(
        &mut self,
        next: State,
        pc: usize,
        depth: usize,
        value: Fact,
    ) -> Result<()> {
        // A block shares its caller's lexical home. An ordinary caller
        // consumes that home and moves every older destination one layer nearer.
        let transfer = if self.block_inputs.is_some() || depth > 0 {
            Transfer::Block {
                pc,
                completion: Completion::Return(depth - usize::from(self.block_inputs.is_none())),
                value,
            }
        } else {
            Transfer::Return { pc, value }
        };
        let edges = self.transfer(next, pc, transfer)?;
        for edge in edges.into_iter().flatten() {
            self.extra.push(self.ctx, edge)?;
        }
        Ok(())
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
            let Some(mut next) = self.capture_exit(state, pc, block, &exit)? else {
                continue;
            };
            match exit.completion {
                Completion::Value => {
                    next.stack.push(self.ctx, Operand::new(exit.value))?;
                    self.extra.push(self.ctx, (pc + 1, next))?;
                }
                Completion::Return(depth) => {
                    self.callback_return(next, pc, depth, exit.value)?;
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
        let base = self.incoming_base()?;
        let mut block = incoming.snapshot(self.ctx)?;
        for link in &mut block.captures.data {
            link.value = state
                .captures
                .as_ref()
                .unwrap()
                .value(self.ctx, base + link.slot)?;
        }
        let mut args = Arguments::new();
        let argument_base = state.stack.data.len() - count;
        for operand in &state.stack.data[argument_base..] {
            self.ctx.charge(1)?;
            args.positional.push(self.ctx, operand.value)?;
        }
        state.stack.data.truncate(argument_base);
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
                    next.captures.as_mut().unwrap().store(
                        self.ctx,
                        self.facts,
                        base + link.slot,
                        value,
                    )?;
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
                Completion::Return(depth) => Transfer::Block {
                    pc,
                    completion: Completion::Return(
                        depth + usize::from(self.block_inputs.is_some()),
                    ),
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
                    } else if self.block_inputs.is_some() {
                        Transfer::Block {
                            pc,
                            completion: Completion::Break(supplied),
                            value: exit.value,
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
