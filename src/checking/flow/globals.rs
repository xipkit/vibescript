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
        let original = state
            .source_slots
            .original(self.ctx, index)?
            .map(|index| &self.program.globals[index].1.0);
        let mut selected: Option<Address> = None;
        for index_arm in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, index_arm);
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
        if let Some(index) = self.root_index(state, name)? {
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
                    let value = state
                        .locals
                        .get(
                            self.ctx,
                            state.global_base + state.source_slots.globals.data[index],
                        )?
                        .value;
                    return self.value_target(value);
                }
            }
        }
        Ok(target)
    }

    /// Resolves `name` in the receiving root for a same-name call that skips the
    /// required file's scope, and with it the file's own functions and declarations.
    pub(super) fn root_target_past_file(&mut self, state: &State, name: &str) -> Result<Target> {
        let target = self.root_target(state, name)?;
        if matches!(target, Target::Host(_)) || self.file_declared_target(name)? != Some(target) {
            return Ok(target);
        }
        let target = self.calls.receiving_binding(self.ctx, name)?;
        if target != Target::Undefined {
            return Ok(target);
        }
        for (global, value) in &self.program.globals {
            self.ctx.work_bytes(global.name().len().max(name.len()))?;
            if global.name() == name {
                return Ok(if let Kind::Builtin(builtin) = value.0 {
                    Target::Builtin(builtin)
                } else {
                    Target::NonCallable
                });
            }
        }
        Ok(Target::Undefined)
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
                        if normal.compatible(self.ctx, &next)? {
                            normal.join(self.ctx, self.facts, &next, false, self.program)?;
                        } else {
                            self.native_continue(pc, next)?;
                        }
                    } else {
                        normal = Some(next);
                    }
                }
                blocks::Completion::Error(class) => {
                    self.emit_error(&next, pc, handlers::bit(class))?
                }
                blocks::Completion::Escape => self.callback_escape(next, pc, exit.value)?,
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
        if state.source_slots.root(self.ctx, index)?.is_some() {
            let Some(alternatives) = self.import_root_branches(state, pc, slot)? else {
                return Ok(false);
            };
            for mut next in alternatives.data {
                if self.read_global(&mut next, pc, index, receiver)? {
                    self.native_continue(pc, next)?;
                }
            }
        }
        let value = state.locals.get(self.ctx, slot)?.value;
        let Some(auto) = receiver else {
            return self.read_value(state, pc, value, Some(slot));
        };
        let mut values = Buffer::empty();
        let mut origin = Some(slot);
        let original = state
            .source_slots
            .original(self.ctx, index)?
            .map(|index| &self.program.globals[index].1.0);
        for index_arm in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, index_arm);
            if let Node::Builtin(builtin) = *self.facts.node(arm) {
                if matches!(original, Some(Kind::Builtin(original)) if *original == builtin)
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
        ctx.charge(1)?;
        Ok(Globals {
            layout: self.global_layout.clone(),
            bindings: self.locals.globals().snapshot(ctx)?,
            written: self.global_written.snapshot(ctx)?,
            pending: self.global_pending.snapshot(ctx)?,
        })
    }

    pub(super) fn global_call(&self, ctx: &mut CallContext) -> Result<Globals> {
        let mut globals = self.globals(ctx)?;
        globals.written = Table::new(self.global_count, false);
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
        self.expand(ctx, &globals.layout)?;
        let mut written = Buffer::empty();
        globals
            .written
            .entries(ctx, &mut |ctx, index, _| written.push(ctx, index))?;
        for index in written.data {
            ctx.charge(1)?;
            let global = globals.get(ctx, index)?;
            let slot = self.global_base + index;
            self.store(ctx, facts, slot, global.value)?;
            let binding = self.locals.get(ctx, slot)?;
            self.locals.set(
                ctx,
                slot,
                Binding {
                    missing: global.missing,
                    ..binding
                },
            )?;
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

    pub(super) fn expand(&mut self, ctx: &mut CallContext, layout: &GlobalLayout) -> Result<bool> {
        let layout = self.global_layout.latest(ctx, layout)?;
        if self.global_layout.same(&layout) {
            return Ok(false);
        }
        if self.global_base.checked_add(layout.len()).is_none() {
            return ctx.fail(crate::ErrorKind::Memory, "checker state size overflow");
        }
        self.locals.grow_globals(ctx, layout.len())?;
        self.global_written.grow(ctx, layout.len())?;
        for index in self.global_count..layout.len() {
            ctx.charge(1)?;
            let initial = layout.initial(index);
            self.locals.set(
                ctx,
                self.global_base + index,
                Binding {
                    value: initial.value,
                    missing: initial.missing,
                    owner: blocks::Owner::Unknown,
                },
            )?;
        }
        self.global_count = layout.len();
        self.global_layout = layout;
        if self.captures.is_none() && self.global_count > 0 {
            self.captures = Some(blocks::Captures::new(ctx, 0, &[])?);
        }
        Ok(true)
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

#[cfg(test)]
mod tests;
