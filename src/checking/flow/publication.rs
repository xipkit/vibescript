use super::*;

#[derive(Clone, Copy)]
pub(super) enum MutationOutput {
    Value(Operand),
    Address(Fact),
}

impl Walker<'_> {
    pub(super) fn publish_result(
        &mut self,
        state: &mut State,
        pc: usize,
        address: &Address,
        receiver: Fact,
        change: Change<'_>,
        output: MutationOutput,
    ) -> Result<Option<Edges>> {
        if !address.supported {
            return self.incomplete(pc).map(Some);
        }
        if address.attached != Attached::No {
            if address.root.is_none() {
                return self.incomplete(pc).map(Some);
            }
            let rebuilt = address.rebuild(self.ctx, self.facts, receiver)?;
            if rebuilt.unsupported {
                return self.incomplete(pc).map(Some);
            }
            let guarded = self.guard_instance(state, pc, address, rebuilt.value)?;
            for mut alternative in guarded.alternatives.data {
                let edges =
                    self.publish_result(&mut alternative, pc, address, receiver, change, output)?;
                self.member_edges(pc, alternative, edges)?;
            }
            let Some(updated) = guarded.value else {
                return Ok(Some([None, None]));
            };
            if let Some(edges) = self.publish_rebuilt(state, pc, address, updated, change)? {
                return Ok(Some(edges));
            }
        }
        match output {
            MutationOutput::Value(value) => state.stack.push(self.ctx, value)?,
            MutationOutput::Address(value) => {
                state.addresses.push(self.ctx, Address::new(None, value))?;
            }
        }
        Ok(None)
    }

    pub(super) fn publish(
        &mut self,
        state: &mut State,
        pc: usize,
        address: &Address,
        receiver: Fact,
        change: Change<'_>,
    ) -> Result<Option<Edges>> {
        // Direct field stores normalize before publication; nested writes use publish_result.
        debug_assert!(address.instance.is_none());
        if !address.supported {
            return self.incomplete(pc).map(Some);
        }
        if address.attached == Attached::No {
            return Ok(None);
        }
        if address.root.is_none() {
            return self.incomplete(pc).map(Some);
        }
        let result = address.rebuild(self.ctx, self.facts, receiver)?;
        if result.unsupported {
            return self.incomplete(pc).map(Some);
        }
        self.publish_rebuilt(state, pc, address, result.value, change)
    }

    fn publish_rebuilt(
        &mut self,
        state: &mut State,
        pc: usize,
        address: &Address,
        mut updated: Fact,
        change: Change<'_>,
    ) -> Result<Option<Edges>> {
        let slot = address.root.unwrap();
        if !self.alias_captures(state, pc, address, updated)? {
            return Ok(Some([None, None]));
        }
        let Some((aliased, alias_address)) = self.alias_instances(state, pc, address, updated)?
        else {
            return Ok(Some([None, None]));
        };
        updated = aliased;
        let change = match (&alias_address, change) {
            (
                Some(address),
                Change::Mutation {
                    method,
                    args,
                    fresh,
                    ..
                },
            ) => Change::Mutation {
                address,
                method,
                args,
                fresh,
            },
            (_, change) => change,
        };
        if address.attached == Attached::Maybe {
            let current = state.locals.get(self.ctx, slot)?.value;
            updated = self.facts.union(self.ctx, &[current, updated])?;
        }
        if updated == Atom::Never.fact() {
            return self.incomplete(pc).map(Some);
        }
        state.store(self.ctx, self.facts, slot, updated)?;
        if state.capture_locals && slot < state.global_base {
            state
                .captures
                .as_mut()
                .unwrap()
                .refresh(self.ctx, self.facts, slot, updated, &change)?;
        }
        state.refresh_globals(self.ctx, self.facts, slot, updated, &change)?;
        for pending in &mut state.addresses.data {
            self.ctx.charge(1)?;
            if pending.root == Some(slot) {
                pending.refresh(self.ctx, self.facts, updated, &change)?;
            }
        }
        Ok(None)
    }
}
