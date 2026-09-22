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
        if let Some(variants) = self.member_variants(receiver, name, site.scope)? {
            for receiver in variants.data {
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = receiver;
                let edges = self.address_member(&mut next, pc, site, namespace)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if matches!(self.facts.atom(receiver), Some(Atom::Unknown | Atom::Any)) {
            return self.member_address(state, pc, receiver, site);
        }
        if self.namespace_receiver(receiver)? {
            if namespace {
                return self.namespace_scope_address(state, pc, site);
            }
            return self.namespace_address_call(state, pc, site.into(), Arguments::new(), true);
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
            Node::Shape(_, _, _, kind) if kind.single()
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
        use super::super::objects::{Selection, absent_is_native};
        let selection = super::super::objects::select(self.ctx, self.facts, receiver, site, name)?;
        if matches!(selection, Some(Selection::Native | Selection::Uncertain(_)))
            && crate::bytecode::mutating_member(name)
            && !namespace
        {
            return self.mutate(state, pc, site, &[], true, false);
        }
        if let Some(Selection::Uncertain(field)) = selection {
            // The stored field may override the member; analyze that path separately.
            let mut present = state.snapshot(self.ctx)?;
            let edges = if site.scope || self.facts.known_non_callable(self.ctx, field)? {
                self.address_field(&mut present, pc, receiver, name)?
            } else {
                self.address_result(&mut present, pc, |walker, state| {
                    walker.member_field(state, pc, field, site.into(), Arguments::new(), false)
                })?
            };
            self.member_edges(pc, present, edges)?;
            if !absent_is_native(site, name) {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                return Ok(Some([None, None]));
            }
        }
        match selection {
            Some(Selection::Field(field))
                if site.scope || self.facts.known_non_callable(self.ctx, field)? =>
            {
                return self.address_field(state, pc, receiver, name);
            }
            Some(Selection::Uncertain(_)) => {
                return self.address_result(state, pc, |walker, state| {
                    walker.member_native(state, pc, receiver, site.into(), Arguments::new())
                });
            }
            Some(_) => return self.member_address(state, pc, receiver, site),
            None => (),
        }
        let mut protected_field = true;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            let Node::Protected(shape, ..) = self.facts.node(arm) else {
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
                Node::Protected(shape, ..) => {
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
            if result.rejected || result.throws {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            }
            if result.value != Atom::Never.fact() && self.native_limit(receiver, name, &[])? {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            if result.rejected {
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
        self.address_result(state, pc, |walker, state| {
            walker.member(state, pc, receiver, site, Arguments::new())
        })
    }

    /// Selects the stored field named by an address chain member.
    fn address_field(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        name: &str,
    ) -> Result<Option<Edges>> {
        let key = self.facts.string(self.ctx, name.as_bytes())?;
        let result =
            state
                .addresses
                .data
                .last_mut()
                .unwrap()
                .index(self.ctx, self.facts, &[key])?;
        self.index_outcome(state, pc, receiver, &[key], &result)
    }

    /// Replaces the selected address with the value produced by a member evaluation.
    fn address_result(
        &mut self,
        state: &mut State,
        pc: usize,
        evaluate: impl FnOnce(&mut Self, &mut State) -> Result<Option<Edges>>,
    ) -> Result<Option<Edges>> {
        state.addresses.data.pop().unwrap();
        let outer = self.native_results.replace(Buffer::empty());
        let result = evaluate(self, state);
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
