use super::*;

impl Walker<'_> {
    pub(super) fn type_shadowed(&mut self, state: &State, guard: usize) -> Result<(bool, bool)> {
        let mut possible = false;
        for name in &self.program.type_guards[guard] {
            self.ctx.work_bytes(name.len())?;
            for (slot, candidate) in self.function.local_names.iter().enumerate() {
                self.ctx.charge(1)?;
                if crate::enums::compare_names(self.ctx, candidate.as_bytes(), name.as_bytes())?
                    == std::cmp::Ordering::Equal
                {
                    let binding = state.locals.get(self.ctx, slot)?;
                    if !binding.missing {
                        return Ok((true, true));
                    }
                    possible |= binding.value != Atom::Never.fact();
                }
            }
            if crate::builtin::Global::parse(name).is_some()
                || self.program.names.contains_key(name)
                || self.program.declaration_names.contains_key(name)
                || self.calls.global(self.ctx, name)?
            {
                return Ok((true, true));
            }
            for host in &self.program.hosts {
                if crate::enums::compare_names(self.ctx, host.as_bytes(), name.as_bytes())?
                    == std::cmp::Ordering::Equal
                {
                    return Ok((true, true));
                }
            }
        }
        Ok((false, possible))
    }

    pub(super) fn member(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        let name = &self.program.members[site.name];
        if let Some(method) = collection_blocks::Method::parse(name) {
            self.collection_block(state, pc, receiver, site, args, method)?;
            return Ok(Some([None, None]));
        }
        if args.block.is_some() {
            return self.incomplete(pc).map(Some);
        }
        let (value, rejected, incomplete, throws) = if let Some(result) =
            builtins::member(self.ctx, self.facts, receiver, site, name, &args)?
        {
            (
                result.value,
                !result.failures.data.is_empty(),
                result.incomplete,
                result.throws,
            )
        } else {
            if !args.keywords.data.is_empty() {
                return self.incomplete(pc).map(Some);
            }
            let result = self.facts.collection_member(
                self.ctx,
                receiver,
                site,
                name,
                &args.positional.data,
            )?;
            (result.value, result.rejected, result.unsupported, 0)
        };
        self.emit_error(
            state,
            pc,
            throws
                | if rejected {
                    handlers::bit(ErrorClass::Runtime)
                } else {
                    0
                },
        )?;
        if rejected {
            let arguments = self.facts.tuple(self.ctx, &args.positional.data)?;
            self.issue(
                pc,
                IssueKind::Member {
                    name: site.name,
                    receiver,
                    arguments,
                },
            )?;
        }
        if incomplete || self.facts.detached_builtin(value) {
            return self.incomplete(pc).map(Some);
        }
        if value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        state.stack.push(self.ctx, Operand::new(value))?;
        Ok(None)
    }
}
