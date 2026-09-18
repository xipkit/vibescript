use super::*;
use crate::{checking::facts::Node, syntax::modules::Visibility};

mod native;

enum Continuation {
    Stack,
    Negate,
    Store(usize),
    Address,
    Assigned(Fact),
}

impl Walker<'_> {
    pub(super) fn operator_instruction(
        &mut self,
        state: &mut State,
        pc: usize,
        op: Op,
    ) -> Result<Option<Edges>> {
        let stack_receiver = match op {
            Op::Binary(_) | Op::AddStore(_) => Some(state.stack.data.len() - 2),
            Op::Index(count) => Some(state.stack.data.len() - count - 1),
            _ => None,
        };
        let receiver = stack_receiver.map_or_else(
            || state.addresses.data.last().unwrap().value,
            |slot| state.stack.data[slot].value,
        );
        let mut instances = false;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            instances |= matches!(
                self.facts.node(self.facts.arm(receiver, i)),
                Node::Instance { .. }
            );
        }
        if !instances {
            return self.native_operator_instruction(state, pc, op);
        }
        if self.facts.arm_count(receiver) > 1 {
            for i in 0..self.facts.arm_count(receiver) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(receiver, i);
                let mut next = state.snapshot(self.ctx)?;
                if let Some(slot) = stack_receiver {
                    next.stack.data[slot].value = arm;
                } else {
                    next.addresses.data.last_mut().unwrap().value = arm;
                }
                let edges = self.operator_instruction(&mut next, pc, op)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if matches!(op, Op::AddressStore) && state.addresses.data.last().unwrap().member.is_some() {
            return self.native_operator_instruction(state, pc, op);
        }
        if let Op::AddressTarget(count, false) = op {
            let base = state.stack.data.len() - count;
            let address = state.addresses.data.last_mut().unwrap();
            for operand in state.stack.data.drain(base..) {
                address.selectors.push(self.ctx, operand.value)?;
            }
            return Ok(None);
        }
        let Some(module) = self.namespace_index(receiver) else {
            return self.incomplete(pc).map(Some);
        };
        let name = match op {
            Op::Binary(name) => name,
            Op::AddStore(_) => "+",
            Op::Shovel(_) => "<<",
            Op::AddressStore => "[]=",
            _ => "[]",
        };
        let mut found = self.operator_method(module, name)?;
        let mut negate = false;
        if found.is_none() && name == "!=" {
            found = self.operator_method(module, "==")?;
            negate = found.is_some();
        }
        let Some((function, visibility)) = found else {
            if matches!(op, Op::Binary(_) | Op::AddStore(_)) {
                return self.native_operator_instruction(state, pc, op);
            }
            self.operator_error(state, pc, receiver, name)?;
            return Ok(Some([None, None]));
        };
        let allowed = match visibility {
            Visibility::Public => true,
            Visibility::Private => false,
            Visibility::Protected => {
                self.function.instance && self.function.namespace == Some(module)
            }
        };
        if !allowed {
            self.operator_error(state, pc, receiver, name)?;
            return Ok(Some([None, None]));
        }
        let mut args = Arguments::new();
        let continuation = match op {
            Op::Binary(_) | Op::AddStore(_) => {
                let value = state.stack.data.pop().unwrap().value;
                state.stack.data.pop();
                args.positional.push(self.ctx, value)?;
                match op {
                    Op::AddStore(slot) => Continuation::Store(slot),
                    _ if negate => Continuation::Negate,
                    _ => Continuation::Stack,
                }
            }
            Op::Index(count) => {
                let base = state.stack.data.len() - count;
                for operand in state.stack.data.drain(base..) {
                    args.positional.push(self.ctx, operand.value)?;
                }
                state.stack.data.pop();
                Continuation::Stack
            }
            Op::Shovel(_) => {
                args.positional
                    .push(self.ctx, state.stack.data.pop().unwrap().value)?;
                state.addresses.data.pop();
                Continuation::Stack
            }
            Op::AddressIndex(count) | Op::AddressTarget(count, true) => {
                let base = state.stack.data.len() - count;
                for operand in state.stack.data.drain(base..) {
                    args.positional.push(self.ctx, operand.value)?;
                }
                if matches!(op, Op::AddressIndex(_)) {
                    state.addresses.data.pop();
                    Continuation::Address
                } else {
                    state
                        .addresses
                        .data
                        .last_mut()
                        .unwrap()
                        .selectors
                        .extend(self.ctx, &args.positional.data)?;
                    Continuation::Stack
                }
            }
            Op::AddressStore => {
                let address = state.addresses.data.pop().unwrap();
                let value = state.stack.data.pop().unwrap().value;
                args.positional = address.selectors;
                args.positional.push(self.ctx, value)?;
                Continuation::Assigned(value)
            }
            _ => unreachable!(),
        };
        let target = Target::Method {
            function,
            receiver,
            constructor: false,
        };
        // Operator calls have no attached block; normal exits return in this state.
        if let Some(edges) = self.invoke(state, pc, target, args)? {
            return Ok(Some(edges));
        }
        match continuation {
            Continuation::Stack => (),
            Continuation::Negate => {
                let value = state.stack.data.pop().unwrap().value;
                let truth = self.facts.test_result(self.ctx, value, Test::Truth)?;
                let value = match self.facts.node(truth) {
                    Node::Boolean(yes) => self.facts.boolean(self.ctx, !yes)?,
                    _ => Atom::Bool.fact(),
                };
                state.stack.push(self.ctx, Operand::new(value))?;
            }
            Continuation::Store(slot) => {
                let value = state.stack.data.last().unwrap().value;
                self.store(state, pc, slot, Operand::new(value))?;
            }
            Continuation::Address => {
                let value = state.stack.data.pop().unwrap().value;
                state.addresses.push(self.ctx, Address::new(None, value))?;
            }
            Continuation::Assigned(value) => {
                *state.stack.data.last_mut().unwrap() = Operand::new(value);
            }
        }
        Ok(None)
    }

    fn operator_method(
        &mut self,
        module: usize,
        name: &str,
    ) -> Result<Option<(usize, Visibility)>> {
        for method in &self.program.namespaces[module].instance_methods {
            self.ctx.charge(1)?;
            self.ctx.work_bytes(name.len().max(method.name.len()))?;
            if method.name == name {
                return Ok(Some((method.function, method.visibility)));
            }
        }
        Ok(None)
    }

    fn operator_error(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        name: &'static str,
    ) -> Result<()> {
        self.issue(pc, IssueKind::Operator { name, receiver })?;
        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))
    }
}
