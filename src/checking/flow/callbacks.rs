use super::*;
use crate::checking::pending;
use blocks::{Closure, Completion, Layer, Link, Parent};

impl Walker<'_> {
    pub(super) fn block_arity(&mut self, function: CallableId) -> Result<usize> {
        self.ctx.charge(1)?;
        let code = if function.source == self.source {
            None
        } else {
            Some(
                self.facts
                    .source_code(self.ctx, function.source)?
                    .ok_or_else(|| {
                        crate::Error::new(
                            crate::ErrorKind::Runtime,
                            "unknown checker callback source",
                        )
                    })?,
            )
        };
        let program = code.as_ref().map_or(self.program, |code| &code.program);
        program
            .functions
            .get(function.index)
            .map(|body| body.block_arity)
            .ok_or_else(|| {
                crate::Error::new(
                    crate::ErrorKind::Runtime,
                    "unknown checker callback function",
                )
            })
    }

    pub(super) fn given(&self) -> bool {
        self.incoming.is_some()
            || self.block_inputs.is_some_and(|b| b.given)
            || self.scope == blocks::Scope::Declaration { given: true }
    }

    fn incoming_base(&mut self) -> Result<usize> {
        if self.block_inputs.is_some() {
            self.local_count(self.function_index)
        } else {
            self.ctx.charge(1)?;
            Ok(0)
        }
    }

    pub(super) fn attach(&mut self, state: &mut State, function: usize) -> Result<bool> {
        let mut captures = Buffer::empty();
        let lexical = self.layouts.locals(self.ctx, self.program, function)?;
        let locals = self.local_count(function)?;
        for slot in 0..lexical {
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
            captures.push(
                self.ctx,
                Link {
                    slot,
                    parent: Parent::Local(parent),
                    value: binding.value,
                    missing: binding.missing,
                    owner: binding.owner,
                },
            )?;
        }
        if let Some(ambient) = self.ambient {
            let base = self
                .layouts
                .locals(self.ctx, self.program, self.function_index)?;
            for index in 0..self.program.functions[ambient].local_names.len() {
                self.ctx.charge(1)?;
                let binding = state.locals.get(self.ctx, base + index)?;
                captures.push(
                    self.ctx,
                    Link {
                        slot: lexical + index,
                        parent: Parent::Local(base + index),
                        value: binding.value,
                        missing: binding.missing,
                        owner: binding.owner,
                    },
                )?;
            }
        }
        let mut inherited = Buffer::empty();
        let forwarding = self.layouts.forwarding(self.ctx, function)?;
        if let Some(incoming) = self.incoming.filter(|_| forwarding) {
            let base = self.incoming_base()?;
            inherited.push(
                self.ctx,
                Layer {
                    scope: incoming.scope,
                    function: incoming.function,
                    receiver: incoming.receiver,
                    ambient: incoming.ambient,
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
                let missing = state.captures.as_ref().unwrap().missing(self.ctx, parent)?;
                let owner = state.captures.as_ref().unwrap().owner(self.ctx, parent)?;
                captures.push(
                    self.ctx,
                    Link {
                        slot: locals + link.slot,
                        parent: Parent::Capture(parent),
                        value,
                        missing,
                        owner,
                    },
                )?;
            }
        }
        state.arguments.data.last_mut().unwrap().arguments.block = Some(Closure {
            scope: self.scope,
            receiver: self.receiver,
            ambient: self.ambient.map(|index| self.source.callable(index)),
            pending: pending::Pending::new(),
            destinations: Buffer::empty(),
            function: self.source.callable(function),
            given: self.given(),
            locals,
            inherited,
            captures,
        });
        Ok(true)
    }

    pub(super) fn prepare_callback(&mut self, state: &State, block: &mut Closure) -> Result<()> {
        block.pending = pending::Pending::new();
        block.destinations = Buffer::empty();
        for link in &mut block.captures.data {
            self.ctx.charge(1)?;
            (link.value, link.missing, link.owner) = match link.parent {
                Parent::Local(slot) => {
                    let binding = state.locals.get(self.ctx, slot)?;
                    (binding.value, binding.missing, binding.owner)
                }
                Parent::Capture(slot) => {
                    let captures = state.captures.as_ref().unwrap();
                    (
                        captures.value(self.ctx, slot)?,
                        captures.missing(self.ctx, slot)?,
                        captures.owner(self.ctx, slot)?,
                    )
                }
            };
            if let Parent::Local(slot) = link.parent {
                for (index, address) in state.addresses.data.iter().enumerate() {
                    self.ctx.charge(1)?;
                    if address.root == Some(slot) {
                        let mut address = address.snapshot(self.ctx)?;
                        address.root = Some(link.slot);
                        block.pending.addresses.push(self.ctx, address)?;
                        block.destinations.push(self.ctx, Parent::Local(index))?;
                    }
                }
            }
            let slot = match link.parent {
                Parent::Capture(slot) => slot,
                Parent::Local(slot) if state.capture_locals => slot,
                Parent::Local(_) => continue,
            };
            let captures = state.captures.as_ref().unwrap();
            if captures.attachment(self.ctx, slot)? == Attached::No {
                continue;
            }
            for (index, address) in captures.pending.addresses.data.iter().enumerate() {
                self.ctx.charge(1)?;
                if address.root == Some(slot) {
                    let mut address = address.snapshot(self.ctx)?;
                    address.root = Some(link.slot);
                    block.pending.addresses.push(self.ctx, address)?;
                    block.destinations.push(self.ctx, Parent::Capture(index))?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn capture_exit(
        &mut self,
        state: &State,
        block: &Closure,
        exit: &blocks::Exit,
    ) -> Result<State> {
        let mut next = state.snapshot(self.ctx)?;
        for link in &block.captures.data {
            if exit.written.get(self.ctx, link.slot)? {
                let value = exit.captures.get(self.ctx, link.slot)?;
                match link.parent {
                    Parent::Local(parent) => {
                        let before = next.locals.get(self.ctx, parent)?;
                        next.store(self.ctx, self.facts, parent, value)?;
                        next.locals
                            .set(self.ctx, parent, Binding { value, ..before })?;
                    }
                    Parent::Capture(parent) => next
                        .captures
                        .as_mut()
                        .unwrap()
                        .store(self.ctx, self.facts, parent, value)?,
                }
            } else if exit.refined.get(self.ctx, link.slot)? {
                let value = exit.captures.get(self.ctx, link.slot)?;
                match link.parent {
                    Parent::Local(parent) => next.refine(self.ctx, self.facts, parent, value)?,
                    Parent::Capture(parent) => next
                        .captures
                        .as_mut()
                        .unwrap()
                        .refine(self.ctx, self.facts, parent, value)?,
                }
            }
        }
        assert_eq!(
            block.destinations.data.len(),
            exit.pending.addresses.data.len()
        );
        for (destination, address) in block
            .destinations
            .data
            .iter()
            .zip(&exit.pending.addresses.data)
        {
            self.ctx.charge(1)?;
            let (original, attached) = match *destination {
                Parent::Local(index) => (&mut next.addresses.data[index], Attached::Yes),
                Parent::Capture(index) => {
                    let captures = next.captures.as_mut().unwrap();
                    let slot = captures.pending.addresses.data[index].root.unwrap();
                    let attached = captures.attachment(self.ctx, slot)?;
                    (&mut captures.pending.addresses.data[index], attached)
                }
            };
            let mut address = address.snapshot(self.ctx)?;
            address.root = original.root;
            match attached {
                Attached::Yes => *original = address,
                Attached::Maybe => {
                    original.join(self.ctx, self.facts, &address, None)?;
                }
                Attached::No => (),
            }
        }
        Ok(next)
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

    pub(super) fn callback_escape(&mut self, state: State, pc: usize, value: Fact) -> Result<()> {
        let edges = self.transfer(
            state,
            pc,
            Transfer::Block {
                pc,
                completion: Completion::Escape,
                value,
            },
        )?;
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
            let mut next = self.capture_exit(state, block, &exit)?;
            next.apply_globals(self.ctx, self.facts, &exit.globals)?;
            match exit.completion {
                Completion::Value => {
                    next.stack.push(self.ctx, Operand::new(exit.value))?;
                    self.extra.push(self.ctx, (pc + 1, next))?;
                }
                Completion::Return(depth) => {
                    self.callback_return(next, pc, depth, exit.value)?;
                }
                Completion::Escape => self.callback_escape(next, pc, exit.value)?,
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
            if self.scope == (blocks::Scope::Declaration { given: true }) {
                return self.abstract_yield(state, pc, count);
            }
            return self.incomplete(pc);
        };
        let base = self.incoming_base()?;
        let mut block = incoming.snapshot(self.ctx)?;
        for link in &mut block.captures.data {
            self.ctx.charge(1)?;
            link.parent = Parent::Capture(base + link.slot);
        }
        self.prepare_callback(&state, &mut block)?;
        let mut args = Arguments::new();
        let argument_base = state.stack.data.len() - count;
        for operand in &state.stack.data[argument_base..] {
            self.ctx.charge(1)?;
            args.positional.push(self.ctx, operand.value)?;
        }
        state.stack.data.truncate(argument_base);
        args.block = Some(block.snapshot(self.ctx)?);
        let current_error = state.current_error(self.ctx, self.current_error)?;
        let globals = state.global_call(self.ctx)?;
        let result = self.calls.invoke(
            self.ctx,
            self.facts,
            Target::Block(incoming.function),
            args,
            current_error,
            &globals,
        )?;
        if result.incomplete {
            return self.incomplete(pc);
        }
        for failure in result.failures.data {
            self.issue(
                pc,
                IssueKind::Call {
                    target: Target::Block(incoming.function),
                    failure,
                },
            )?;
            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let mut next = self.capture_exit(&state, &block, &exit)?;
            next.apply_globals(self.ctx, self.facts, &exit.globals)?;
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
                Completion::Escape => Transfer::Block {
                    pc,
                    completion: Completion::Escape,
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
