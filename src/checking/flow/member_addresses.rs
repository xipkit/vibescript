use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    pub(super) fn address_member(
        &mut self,
        state: &mut State,
        pc: usize,
        site: CallSite,
        namespace: bool,
    ) -> Result<Option<Edges>> {
        let name = &self.program.members[site.name];
        let receiver = state.addresses.data.last().unwrap().value;
        if let Some(variants) =
            super::super::objects::variants(self.ctx, self.facts, receiver, name)?
        {
            for receiver in variants.data {
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = receiver;
                let edges = self.address_member(&mut next, pc, site, namespace)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if matches!(self.facts.node(receiver), Node::Callable { .. }) {
            self.export_value(state, pc, receiver, false)?;
            return Ok(Some([None, None]));
        }
        if !namespace && matches!(self.facts.node(receiver), Node::Shape(..)) {
            if let Some((field, false)) =
                self.facts
                    .selected_field(self.ctx, receiver, name.as_bytes())?
            {
                if matches!(
                    self.facts.node(field),
                    Node::Callable {
                        target: super::super::facts::Callable::Function(_),
                        ..
                    }
                ) {
                    if site.auto && site.scope {
                        self.export_value(state, pc, field, false)?;
                        return Ok(Some([None, None]));
                    }
                    return self.member_address(state, pc, receiver, site);
                }
            }
        }
        // Addressed chains select stored fields; an explicit helper call selects a copy.
        if matches!(
            self.facts.node(receiver),
            Node::Shape(_, _, _, HashKind::Plain | HashKind::Object)
        ) {
            if let Some((field, false)) =
                self.facts
                    .selected_field(self.ctx, receiver, name.as_bytes())?
            {
                if state.addresses.data.last().unwrap().root.is_none()
                    && !self.facts.known_non_callable(self.ctx, field)?
                {
                    state.addresses.data.pop().unwrap();
                    if self.dynamic(field)? {
                        return self.incomplete(pc).map(Some);
                    }
                    if !self.read_value(state, pc, field, None)? {
                        return Ok(Some([None, None]));
                    }
                    let value = state.stack.data.pop().unwrap().value;
                    state.addresses.push(self.ctx, Address::new(None, value))?;
                    return Ok(None);
                }
                let key = self.facts.string(self.ctx, name.as_bytes())?;
                let result =
                    state
                        .addresses
                        .data
                        .last_mut()
                        .unwrap()
                        .index(self.ctx, self.facts, &[key])?;
                return self.index_outcome(state, pc, receiver, &[key], &result);
            }
        }
        match super::super::objects::select(self.ctx, self.facts, receiver, site, name)? {
            Some(super::super::objects::Selection::Native)
                if crate::bytecode::mutating_member(name) && !namespace =>
            {
                return self.mutate(state, pc, site, &[], true, false);
            }
            Some(super::super::objects::Selection::Field(field))
                if site.scope || self.facts.known_non_callable(self.ctx, field)? =>
            {
                let key = self.facts.string(self.ctx, name.as_bytes())?;
                let result =
                    state
                        .addresses
                        .data
                        .last_mut()
                        .unwrap()
                        .index(self.ctx, self.facts, &[key])?;
                return self.index_outcome(state, pc, receiver, &[key], &result);
            }
            Some(_) => return self.member_address(state, pc, receiver, site),
            None => (),
        }
        let mut protected_field = true;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            let Node::Protected(shape, _) = self.facts.node(arm) else {
                protected_field = false;
                break;
            };
            if !self
                .facts
                .selected_field(self.ctx, *shape, name.as_bytes())?
                .is_some_and(|(_, optional)| !optional)
            {
                protected_field = false;
                break;
            }
        }
        if protected_field {
            let key = self.facts.string(self.ctx, name.as_bytes())?;
            let result =
                state
                    .addresses
                    .data
                    .last_mut()
                    .unwrap()
                    .index(self.ctx, self.facts, &[key])?;
            if let Some(edges) = self.index_outcome(state, pc, receiver, &[key], &result)? {
                return Ok(Some(edges));
            }
            return Ok(None);
        }
        if builtins::value_member(self.ctx, self.facts, receiver, name)? {
            state.addresses.data.pop().unwrap();
            if let Some(edges) = self.member(state, pc, receiver, site, Arguments::new())? {
                return Ok(Some(edges));
            }
            let value = state.stack.data.pop().unwrap().value;
            state.addresses.push(self.ctx, Address::new(None, value))?;
            return Ok(None);
        }
        let (mut fields, mut absent) = (false, false);
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            match self.facts.node(arm) {
                Node::Protected(shape, _) => {
                    let selected = self
                        .facts
                        .selected_field(self.ctx, *shape, name.as_bytes())?;
                    fields |= selected.is_some();
                    absent |= selected.is_none();
                }
                Node::Shape(_, open, _, _) if !namespace => {
                    let selected = self.facts.selected_field(self.ctx, arm, name.as_bytes())?;
                    if *open
                        || selected.is_some_and(|(_, optional)| {
                            optional && crate::members::hash_builtin(name)
                        })
                    {
                        return self.incomplete(pc).map(Some);
                    }
                    fields |= selected.is_some();
                    absent |= selected.is_none();
                }
                Node::Hash(..) if !namespace => {
                    return self.incomplete(pc).map(Some);
                }
                _ => absent = true,
            }
        }
        if fields && absent {
            return self.incomplete(pc).map(Some);
        }
        if fields {
            let key = self.facts.string(self.ctx, name.as_bytes())?;
            let result =
                state
                    .addresses
                    .data
                    .last_mut()
                    .unwrap()
                    .index(self.ctx, self.facts, &[key])?;
            if let Some(edges) = self.index_outcome(state, pc, receiver, &[key], &result)? {
                return Ok(Some(edges));
            }
        } else if crate::bytecode::mutating_member(name) && !namespace {
            if let Some(edges) = self.mutate(state, pc, site, &[], true, false)? {
                return Ok(Some(edges));
            }
        } else {
            let result = self
                .facts
                .collection_member(self.ctx, receiver, site, name, &[])?;
            if result.rejected {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                let arguments = self.facts.tuple(self.ctx, &[])?;
                self.issue(
                    pc,
                    IssueKind::Member {
                        name: site.name,
                        receiver,
                        arguments,
                    },
                )?;
            }
            if result.unsupported {
                return self.incomplete(pc).map(Some);
            }
            if result.value == Atom::Never.fact() {
                return Ok(Some([None, None]));
            }
            *state.addresses.data.last_mut().unwrap() = Address::new(None, result.value);
        }
        Ok(None)
    }

    fn member_address(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
    ) -> Result<Option<Edges>> {
        state.addresses.data.pop().unwrap();
        let outer = self.native_results.replace(Buffer::empty());
        let result = self.member(state, pc, receiver, site, Arguments::new());
        let results = std::mem::replace(&mut self.native_results, outer).unwrap();
        let edges = result?;
        for mut next in results.data {
            self.ctx.charge(1)?;
            let value = next.stack.data.pop().unwrap().value;
            next.addresses.push(self.ctx, Address::new(None, value))?;
            self.native_continue(pc, next)?;
        }
        if edges.is_none() {
            let value = state.stack.data.pop().unwrap().value;
            state.addresses.push(self.ctx, Address::new(None, value))?;
        }
        Ok(edges)
    }
}
