use super::*;
use crate::checking::addresses::Attached;

/// How a same-name call resolves while an assignment evaluates the value of its binding.
///
/// `helper = helper()` calls the `helper` function rather than the local being assigned.
/// Such a call skips each binding that an enclosing assignment is filling and resolves
/// through the next enclosing scope, or by name when none remains.
pub(super) enum Bypass {
    /// No enclosing assignment skips the binding.
    Kept,
    /// The call resolves by name, optionally skipping a namespace body's outer binding.
    Name {
        ambient: bool,
    },
    /// The call reaches this binding of an enclosing scope.
    Outer(Binding),
    Unsupported,
}

impl Walker<'_> {
    pub(super) fn bypass(&mut self, state: &State, pc: usize, op: Op) -> Result<Bypass> {
        let (Op::ResolveCall(own, ..), Op::ResolveCall(slot, ..)) = (self.function.code[pc], op)
        else {
            return Ok(Bypass::Kept);
        };
        if own == usize::MAX || !self.chain_bypassed(pc, own)? {
            return Ok(Bypass::Kept);
        }
        // A namespace body's absent local resolved to its declaring frame's binding.
        if slot != own {
            return Ok(Bypass::Name { ambient: false });
        }
        let captured = self.program.functions[self.function_index]
            .captures
            .get(own)
            .copied()
            .flatten()
            .is_some();
        let captures = state.captures.as_ref().filter(|_| state.capture_locals);
        let (Some(captures), true) = (captures, captured) else {
            return Ok(Bypass::Name { ambient: true });
        };
        match captures.attachment(self.ctx, own)? {
            // The block's own binding holds the value, so the enclosing binding comes next.
            Attached::No => {
                if !self
                    .layouts
                    .bypassed(self.ctx, self.function_index, pc, own)?
                {
                    return Ok(Bypass::Kept);
                }
                let outer = Binding {
                    value: captures.value(self.ctx, own)?,
                    missing: captures.missing(self.ctx, own)?,
                    owner: captures.owner(self.ctx, own)?,
                };
                if outer.value == Atom::Never.fact() {
                    return Ok(Bypass::Name { ambient: true });
                }
                let Some(level) = self.enclosing(self.function_index, own)? else {
                    return Ok(Bypass::Unsupported);
                };
                self.skip_holder(level, outer.owner, Some(outer))
            }
            Attached::Yes => {
                let owner = state.locals.get(self.ctx, own)?.owner;
                self.skip_holder((self.function_index, pc, own), owner, None)
            }
            Attached::Maybe => Ok(Bypass::Unsupported),
        }
    }

    /// Reports whether any assignment along the lexical chain of `slot` skips its binding.
    fn chain_bypassed(&mut self, pc: usize, slot: usize) -> Result<bool> {
        let mut level = Some((self.function_index, pc, slot));
        while let Some((function, pc, slot)) = level {
            if self.layouts.bypassed(self.ctx, function, pc, slot)? {
                return Ok(true);
            }
            level = self.enclosing(function, slot)?;
        }
        Ok(false)
    }

    /// Follows a captured slot to its enclosing function and the attaching instruction.
    fn enclosing(&mut self, function: usize, slot: usize) -> Result<Option<(usize, usize, usize)>> {
        let Some(capture) = self.program.functions[function]
            .captures
            .get(slot)
            .copied()
            .flatten()
        else {
            return Ok(None);
        };
        let mut level = (function, 0);
        for _ in 0..=capture.depth {
            let Some(parent) = self.layouts.attachment(self.ctx, level.0)? else {
                return Ok(None);
            };
            level = parent;
        }
        Ok(Some((level.0, level.1, capture.slot)))
    }

    /// Resolves past the binding that `owner` holds, when an assignment from its scope or
    /// a nested one skips it. `binding` describes that holder when it is not skipped.
    fn skip_holder(
        &mut self,
        level: (usize, usize, usize),
        owner: blocks::Owner,
        binding: Option<Binding>,
    ) -> Result<Bypass> {
        let blocks::Owner::Function(owner) = owner else {
            return Ok(Bypass::Unsupported);
        };
        let mut skipped = false;
        let mut level = Some(level);
        while let Some((function, pc, slot)) = level {
            skipped |= self.layouts.bypassed(self.ctx, function, pc, slot)?;
            if self.source.callable(function) == owner {
                if !skipped {
                    return Ok(binding.map_or(Bypass::Kept, Bypass::Outer));
                }
                // Only a further enclosing binding could still receive the call.
                return Ok(if self.enclosing(function, slot)?.is_some() {
                    Bypass::Unsupported
                } else {
                    Bypass::Name { ambient: true }
                });
            }
            level = self.enclosing(function, slot)?;
        }
        Ok(Bypass::Unsupported)
    }
}
