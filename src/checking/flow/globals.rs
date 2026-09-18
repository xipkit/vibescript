use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    pub(super) fn global_address(
        &mut self,
        state: &State,
        pc: usize,
        index: usize,
    ) -> Result<Option<Address>> {
        let slot = state.global_base + index;
        let value = state.locals.get(self.ctx, slot)?.value;
        let mut selected: Option<Address> = None;
        for index_arm in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, index_arm);
            let original = self.program.globals.get(index).map(|(_, value)| &value.0);
            let address = if matches!((self.facts.node(arm), original), (Node::Builtin(current), Some(Kind::Builtin(original))) if current==original)
            {
                // Original builtin reads precede arguments and produce detached temporary values.
                let mut next = state.snapshot(self.ctx)?;
                if !self.read_value(&mut next, pc, arm, None)? {
                    continue;
                }
                Address::new(None, next.stack.data.pop().unwrap().value)
            } else {
                Address::new(Some(slot), arm)
            };
            if let Some(selected) = &mut selected {
                selected.join(self.ctx, self.facts, &address, None)?;
            } else {
                selected = Some(address);
            }
        }
        Ok(selected)
    }

    pub(super) fn root_target(&mut self, state: &State, name: &str) -> Result<Target> {
        if let Some(index) = self.root_index(name)? {
            return Ok(Target::Value(
                state.locals.get(self.ctx, state.global_base + index)?.value,
            ));
        }
        let target = self.calls.resolve(self.ctx, name)?;
        self.ctx.work_bytes(name.len())?;
        if matches!(target, Target::Builtin(_) | Target::NonCallable)
            && !self.program.declaration_names.contains_key(name)
            && !self.calls.global(self.ctx, name)?
        {
            for (index, (global, _)) in self.program.globals.iter().enumerate() {
                self.ctx.work_bytes(name.len().max(global.name().len()))?;
                if global.name() == name {
                    let value = state.locals.get(self.ctx, state.global_base + index)?.value;
                    return self.value_target(value);
                }
            }
        }
        Ok(target)
    }

    pub(super) fn global_exits(
        &mut self,
        state: &mut State,
        pc: usize,
        exits: Buffer<blocks::Exit>,
    ) -> Result<bool> {
        let mut normal: Option<State> = None;
        for exit in exits.data {
            self.ctx.charge(1)?;
            let mut next = state.snapshot(self.ctx)?;
            next.apply_globals(self.ctx, self.facts, &exit.globals)?;
            match exit.completion {
                blocks::Completion::Value => {
                    next.stack.push(self.ctx, Operand::new(exit.value))?;
                    if let Some(normal) = &mut normal {
                        normal.join(self.ctx, self.facts, &next, false)?;
                    } else {
                        normal = Some(next);
                    }
                }
                blocks::Completion::Error(class) => {
                    self.emit_error(&next, pc, handlers::bit(class))?
                }
                _ => unreachable!("ordinary functions consume their own control transfers"),
            }
        }
        if let Some(normal) = normal {
            *state = normal;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(super) fn read_global(
        &mut self,
        state: &mut State,
        pc: usize,
        index: usize,
        receiver: Option<bool>,
    ) -> Result<bool> {
        let slot = state.global_base + index;
        let value = state.locals.get(self.ctx, slot)?.value;
        let Some(auto) = receiver else {
            return self.read_value(state, pc, value, Some(slot));
        };
        let mut values = Buffer::empty();
        let mut origin = Some(slot);
        for index_arm in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, index_arm);
            if let Node::Builtin(builtin) = *self.facts.node(arm) {
                if matches!(self.program.globals.get(index).map(|(_, value)| &value.0), Some(Kind::Builtin(original)) if *original == builtin)
                    && (auto || !builtin.auto())
                {
                    let mut next = state.snapshot(self.ctx)?;
                    if self.read_value(&mut next, pc, arm, None)? {
                        values.push(self.ctx, next.stack.data.pop().unwrap().value)?;
                        origin = None;
                    }
                    continue;
                }
            }
            values.push(self.ctx, arm)?;
        }
        let value = self.facts.union(self.ctx, &values.data)?;
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
}

impl State {
    pub(super) fn globals(&self, ctx: &mut CallContext) -> Result<Globals> {
        let mut globals = Globals::empty();
        globals.pending = self.global_pending.snapshot(ctx)?;
        for index in 0..self.global_count {
            ctx.charge(1)?;
            let value = self.locals.get(ctx, self.global_base + index)?.value;
            globals.values.push(ctx, value)?;
            let written = self.global_written.get(ctx, index)?;
            globals.written.push(ctx, written)?;
        }
        Ok(globals)
    }

    pub(super) fn global_call(&self, ctx: &mut CallContext) -> Result<Globals> {
        let mut globals = self.globals(ctx)?;
        ctx.charge(globals.written.data.len() as u64)?;
        globals.written.data.fill(false);
        for address in &self.addresses.data {
            ctx.charge(1)?;
            if let Some(root) = address.root.filter(|&slot| slot >= self.global_base) {
                let mut address = address.snapshot(ctx)?;
                address.root = Some(root - self.global_base);
                globals.pending.addresses.push(ctx, address)?;
            }
        }
        Ok(globals)
    }

    pub(super) fn apply_globals(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
    ) -> Result<()> {
        assert_eq!(self.global_count, globals.values.data.len());
        for (index, &value) in globals.values.data.iter().enumerate() {
            ctx.charge(1)?;
            if globals.written.data[index] {
                self.store(ctx, facts, self.global_base + index, value)?;
            }
        }
        let mut returned = globals.pending.addresses.data.iter();
        for address in &mut self.global_pending.addresses.data {
            ctx.charge(1)?;
            *address = returned.next().unwrap().snapshot(ctx)?;
        }
        for address in &mut self.addresses.data {
            ctx.charge(1)?;
            if address.root.is_some_and(|slot| slot >= self.global_base) {
                let mut updated = returned.next().unwrap().snapshot(ctx)?;
                updated.root = address.root;
                *address = updated;
            }
        }
        assert!(returned.next().is_none());
        Ok(())
    }

    pub(super) fn refresh_globals(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        slot: usize,
        value: Fact,
        change: &Change<'_>,
    ) -> Result<()> {
        if slot < self.global_base {
            return Ok(());
        }
        for address in &mut self.global_pending.addresses.data {
            ctx.charge(1)?;
            if address.root == Some(slot - self.global_base) {
                address.refresh(ctx, facts, value, change)?;
            }
        }
        Ok(())
    }
}
