use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    /// Narrows the local a splat operand was read from to the alternatives the splat
    /// expands, or to those it rejects on the error path.
    pub(super) fn narrow_splat(
        &mut self,
        state: &mut State,
        operand: Operand,
        keyword: bool,
        expands: bool,
    ) -> Result<()> {
        let Some(slot) = operand.origin else {
            return Ok(());
        };
        let binding = state.locals.get(self.ctx, slot)?;
        // Optional lookup can resolve a different binding when this one is absent, and a
        // contract choice keeps its conversion order only as a whole.
        let count = self.facts.arm_count(binding.value);
        self.ctx.charge(count as u64)?;
        if binding.missing
            || (0..count).any(|index| {
                matches!(
                    self.facts.node(self.facts.arm(binding.value, index)),
                    Node::Choice(_)
                )
            })
        {
            return Ok(());
        }
        let parts =
            crate::checking::arguments::split_splat(self.ctx, self.facts, binding.value, keyword)?;
        let value = if expands {
            parts.admitted
        } else {
            parts.rejected
        };
        if value != Atom::Never.fact() && value != binding.value {
            state.refine(self.ctx, self.facts, slot, value)?;
        }
        Ok(())
    }
}
