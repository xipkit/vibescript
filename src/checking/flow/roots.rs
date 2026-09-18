use super::*;

pub(super) fn find(ctx: &mut CallContext, roots: &[Root], name: &str) -> Result<Option<usize>> {
    for (index, root) in roots.iter().enumerate() {
        let key = root.name.as_bytes().unwrap();
        ctx.work_bytes(key.len().max(name.len()))?;
        if key == name.as_bytes() {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

impl Walker<'_> {
    pub(super) fn root_index(&mut self, state: &State, name: &str) -> Result<Option<usize>> {
        Ok(find(self.ctx, self.roots, name)?.map(|index| state.source_slots.roots.data[index]))
    }

    pub(super) fn global_index(&mut self, state: &State, index: usize) -> Result<Option<usize>> {
        let name = self.program.globals[index].0.name();
        if let Some((slot, binding)) = self.file_binding(state, name)? {
            debug_assert!(!binding.missing);
            return Ok(Some(slot - state.global_base));
        }
        if let Some(index) = self.root_index(state, name)? {
            return Ok(Some(index));
        }
        Ok((!self.calls.global(self.ctx, name)?).then_some(state.source_slots.globals.data[index]))
    }

    pub(super) fn root_op(&mut self, state: &State, op: Op) -> Result<Option<Op>> {
        if self.program.file {
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
            | Op::AddressBound(slot, _) => slot,
            _ => return Ok(Some(op)),
        };
        if slot >= self.function.local_names.len() {
            return Ok(Some(op));
        }
        self.ctx.charge(self.function.params.len() as u64 + 1)?;
        if self.function.params.iter().any(|param| param.slot == slot) {
            return Ok(Some(op));
        }
        let binding = state.locals.get(self.ctx, slot)?;
        if !binding.missing {
            return Ok(Some(op));
        }
        let Some(index) = self.root_index(state, &self.function.local_names[slot])? else {
            return Ok(Some(op));
        };
        if binding.value != Atom::Never.fact() {
            return Ok(None);
        }
        let root = state.global_base + index;
        Ok(Some(match op {
            Op::Load(_) => Op::Load(root),
            Op::LoadOptional(_, name) => Op::LoadOptional(root, name),
            Op::ReceiverBound(_, target) => Op::ReceiverBound(root, target),
            Op::Declare(_) => Op::Declare(root),
            Op::Store(_) => Op::Store(root),
            Op::AddStore(_) => Op::AddStore(root),
            Op::AddressLocal(_) => Op::AddressLocal(root),
            Op::AddressBound(_, target) => Op::AddressBound(root, target),
            _ => unreachable!(),
        }))
    }

    pub(super) fn resolve_value_target(
        &mut self,
        state: &mut State,
        pc: usize,
    ) -> Result<Option<Edges>> {
        let Target::Value(value) = state.arguments.data.last().unwrap().target else {
            return Ok(None);
        };
        self.set_call_target(state, pc, value)
    }
}
