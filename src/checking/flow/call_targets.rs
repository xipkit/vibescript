use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    pub(super) fn set_call_target(
        &mut self,
        state: &mut State,
        pc: usize,
        value: Fact,
    ) -> Result<Option<Edges>> {
        let mut builtins = false;
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            builtins |= matches!(
                self.facts.node(self.facts.arm(value, i)),
                Node::Builtin(_) | Node::Callable { .. }
            );
        }
        if self.facts.arm_count(value) > 1 && (builtins || self.dynamic(value)?) {
            for i in 0..self.facts.arm_count(value) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(value, i);
                let mut next = state.snapshot(self.ctx)?;
                next.arguments.data.last_mut().unwrap().target = self.value_target(arm)?;
                self.native_continue(pc, next)?;
            }
            return Ok(Some([None, None]));
        }
        state.arguments.data.last_mut().unwrap().target = self.value_target(value)?;
        Ok(None)
    }

    pub(super) fn call_member(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
    ) -> Result<Option<Edges>> {
        if let Some(variants) = self.member_variants(receiver, &self.program.members[site.name])? {
            for receiver in variants.data {
                let mut next = state.snapshot(self.ctx)?;
                let edges = self.call_member(&mut next, pc, receiver, site)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        let name = &self.program.members[site.name];
        if matches!(self.facts.atom(receiver), Some(Atom::Unknown | Atom::Any)) {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            state.arguments.data.last_mut().unwrap().target = Target::Dynamic;
            return Ok(None);
        }
        if self.namespace_receiver(receiver)? {
            return self.namespace_call_target(state, pc, receiver, site);
        }
        if matches!(self.facts.node(receiver), Node::Offset(_)) {
            let target = Target::Offset(receiver);
            self.issue(
                pc,
                IssueKind::Call {
                    target,
                    failure: Failure::BuiltinValue,
                },
            )?;
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            return Ok(Some([None, None]));
        }
        if let Node::Builtin(builtin) = self.facts.node(receiver) {
            self.issue(
                pc,
                IssueKind::Call {
                    target: Target::Builtin(*builtin),
                    failure: Failure::BuiltinValue,
                },
            )?;
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            return Ok(Some([None, None]));
        }
        let fields = if let Node::Protected(shape, _) = self.facts.node(receiver) {
            *shape
        } else {
            receiver
        };
        let protected = fields != receiver;
        let field = if protected
            || matches!(
                self.facts.node(fields),
                Node::Shape(_, false, _, HashKind::Any | HashKind::Object)
            ) {
            self.facts
                .selected_field(self.ctx, fields, name.as_bytes())?
                .and_then(|(value, optional)| (!optional).then_some(value))
        } else {
            return self.incomplete(pc).map(Some);
        };
        let Some(field) = field else {
            if !site.scope
                && (crate::members::hash_builtin(name) || crate::members::names::universal(name))
            {
                return self.incomplete(pc).map(Some);
            }
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            self.issue(
                pc,
                IssueKind::Call {
                    target: Target::Undefined,
                    failure: Failure::Undefined,
                },
            )?;
            return Ok(Some([None, None]));
        };
        self.set_call_target(state, pc, field)
    }
}
