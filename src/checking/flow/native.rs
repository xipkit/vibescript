use super::*;

mod forwarding;
mod reduction;
pub(super) use reduction::NativeFrame;
mod site;
pub(super) use site::MemberSite;

impl Walker<'_> {
    pub(super) fn native_continue(&mut self, pc: usize, state: State) -> Result<()> {
        if let Some(results) = &mut self.native_results {
            results.push(self.ctx, state)
        } else {
            self.extra.push(self.ctx, (pc + 1, state))
        }
    }

    pub(super) fn member_edges(
        &mut self,
        pc: usize,
        state: State,
        edges: Option<Edges>,
    ) -> Result<()> {
        if let Some(edges) = edges {
            for edge in edges.into_iter().flatten() {
                self.extra.push(self.ctx, edge)?;
            }
        } else {
            self.native_continue(pc, state)?;
        }
        Ok(())
    }

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
        site: impl Into<MemberSite>,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        let site = site.into();
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        if let Some(variants) = self.member_variants(receiver, name)? {
            for receiver in variants.data {
                self.ctx.charge(1)?;
                let mut next = state.snapshot(self.ctx)?;
                let arguments = args.snapshot(self.ctx)?;
                let edges = self.member(&mut next, pc, receiver, site, arguments)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if self.namespace_receiver(receiver)? {
            return self.namespace_member(state, pc, receiver, site, args, false);
        }
        match crate::checking::objects::select(self.ctx, self.facts, receiver, site.call, name)? {
            Some(crate::checking::objects::Selection::Field(field)) => {
                if site.auto {
                    if site.scope {
                        state.stack.push(self.ctx, Operand::new(field))?;
                        return Ok(None);
                    }
                    if self.dynamic(field)? {
                        return self.incomplete(pc).map(Some);
                    }
                    return Ok(if self.read_value(state, pc, field, None)? {
                        None
                    } else {
                        Some([None, None])
                    });
                }
                let target = self.value_target(field)?;
                return self.invoke(state, pc, target, args);
            }
            Some(crate::checking::objects::Selection::Missing) => {
                self.collection_error(state, pc, receiver, site, &args, ErrorClass::Runtime)?;
                return Ok(Some([None, None]));
            }
            Some(crate::checking::objects::Selection::Incomplete) => {
                return self.incomplete(pc).map(Some);
            }
            _ => (),
        }
        if let Some(method) = collection_blocks::text::TextMethod::parse(name) {
            if method.materializes() {
                let mut strings = true;
                for i in 0..self.facts.arm_count(receiver) {
                    self.ctx.charge(1)?;
                    strings &= self.facts.atom(self.facts.arm(receiver, i)) == Some(Atom::String);
                }
                if strings {
                    let mut args = args;
                    args.block = None;
                    return self.member_without_collection(state, pc, receiver, site, &args);
                }
            }
            self.text_block(state, pc, receiver, site, args, method)?;
            return Ok(Some([None, None]));
        }
        if let Some(method) = collection_blocks::Method::parse(name) {
            self.collection_block(state, pc, receiver, site, args, method)?;
            return Ok(Some([None, None]));
        }
        self.member_without_collection(state, pc, receiver, site, &args)
    }

    pub(super) fn member_without_collection(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
    ) -> Result<Option<Edges>> {
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        let (value, rejected, incomplete, throws) = if let Some(result) =
            builtins::member(self.ctx, self.facts, receiver, site.call, name, args)?
        {
            let mut result = result;
            if name == "is_type?" {
                self.type_predicate_outcome(state, pc, receiver, args, &mut result)?;
            }
            (
                result.value,
                !result.failures.data.is_empty(),
                result.incomplete,
                result.throws,
            )
        } else {
            if !args.keywords.data.is_empty() || args.block.is_some() {
                return self.incomplete(pc).map(Some);
            }
            let result = self.facts.collection_member(
                self.ctx,
                receiver,
                site.call,
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
        if incomplete {
            return self.incomplete(pc).map(Some);
        }
        if value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        state.stack.push(self.ctx, Operand::new(value))?;
        Ok(None)
    }
}
