use super::*;

impl Walker<'_> {
    pub(super) fn native_operator_instruction(
        &mut self,
        state: &mut State,
        pc: usize,
        op: Op,
    ) -> Result<Option<Edges>> {
        match op {
            Op::Binary("===") => {
                let target = state.stack.data.pop().unwrap();
                let matcher = state.stack.data.pop().unwrap();
                if let Some(edges) = self.case_compare(state, pc, Some(target), matcher, false)? {
                    return Ok(Some(edges));
                }
            }
            Op::Index(count) => {
                let base = state.stack.data.len() - count - 1;
                let receiver = state.stack.data[base].value;
                let mut args = Buffer::empty();
                for operand in &state.stack.data[base + 1..] {
                    args.push(self.ctx, operand.value)?;
                }
                let result = self
                    .facts
                    .collection_index(self.ctx, receiver, &args.data)?;
                if result.rejected {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    let arguments = self.facts.tuple(self.ctx, &args.data)?;
                    self.issue(
                        pc,
                        IssueKind::Index {
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
                state.stack.data.truncate(base);
                state.stack.push(self.ctx, Operand::new(result.value))?;
            }
            Op::Shovel(site) => {
                let value = state.stack.data.pop().unwrap().value;
                let receiver = state.addresses.data.last().unwrap().value;
                let mut allowed = Buffer::empty();
                let mut rejected = false;
                for i in 0..self.facts.arm_count(receiver) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(receiver, i);
                    match self.facts.node(arm) {
                        Node::Array(_) | Node::Tuple(_) | Node::Atom(Atom::Unknown | Atom::Any) => {
                            allowed.push(self.ctx, arm)?
                        }
                        Node::Named(_) | Node::Nominal { .. } => {
                            return self.incomplete(pc).map(Some);
                        }
                        Node::Atom(Atom::Never) => (),
                        _ => rejected = true,
                    }
                }
                if rejected {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    self.issue(
                        pc,
                        IssueKind::Binary {
                            op: "<<",
                            left: receiver,
                            right: value,
                        },
                    )?;
                }
                let receiver = self.facts.union(self.ctx, &allowed.data)?;
                if receiver == Atom::Never.fact() {
                    return Ok(Some([None, None]));
                }
                state.addresses.data.last_mut().unwrap().value = receiver;
                if let Some(edges) = self.mutate(state, pc, site, &[value], false, false)? {
                    return Ok(Some(edges));
                }
            }
            Op::AddressIndex(count) | Op::AddressTarget(count, _) => {
                let base = state.stack.data.len() - count;
                let mut args = Buffer::empty();
                for operand in &state.stack.data[base..] {
                    args.push(self.ctx, operand.value)?;
                }
                state.stack.data.truncate(base);
                let address = state.addresses.data.last_mut().unwrap();
                let receiver = address.value;
                let result = match op {
                    Op::AddressIndex(_) => Some(address.index(self.ctx, self.facts, &args.data)?),
                    Op::AddressTarget(_, read) => {
                        address.target(self.ctx, self.facts, &args.data, read)?
                    }
                    _ => unreachable!(),
                };
                if let Some(result) = result {
                    if let Some(edges) =
                        self.index_outcome(state, pc, receiver, &args.data, &result)?
                    {
                        return Ok(Some(edges));
                    }
                    if matches!(op, Op::AddressTarget(..)) {
                        state.stack.push(self.ctx, Operand::new(result.value))?;
                    }
                }
            }
            Op::AddressStore => {
                let value = state.stack.data.pop().unwrap();
                let address = state.addresses.data.pop().unwrap();
                if !address.supported {
                    return self.incomplete(pc).map(Some);
                }
                if let Some(name) = address.member {
                    if self.namespace_receiver(address.value)? {
                        if let Some(edges) =
                            self.namespace_member_store(state, pc, address.value, name, value)?
                        {
                            return Ok(Some(edges));
                        }
                        return Ok(None);
                    }
                }
                let protection = address.protection(self.ctx, self.facts)?;
                if protection != Attached::No {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    let selectors = self.facts.tuple(self.ctx, &address.selectors.data)?;
                    self.issue(
                        pc,
                        IssueKind::Write {
                            receiver: address.value,
                            selectors,
                            value: value.value,
                        },
                    )?;
                    if protection == Attached::Yes {
                        return Ok(Some([None, None]));
                    }
                }
                if !address.supported {
                    return self.incomplete(pc).map(Some);
                }
                let selectors = self.facts.tuple(self.ctx, &address.selectors.data)?;
                let [key] = address.selectors.data.as_slice() else {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    self.issue(
                        pc,
                        IssueKind::Write {
                            receiver: address.value,
                            selectors,
                            value: value.value,
                        },
                    )?;
                    return Ok(Some([None, None]));
                };
                let result =
                    self.facts
                        .collection_write(self.ctx, address.value, *key, value.value)?;
                if result.rejected {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    self.issue(
                        pc,
                        IssueKind::Write {
                            receiver: address.value,
                            selectors,
                            value: value.value,
                        },
                    )?;
                }
                if result.unsupported {
                    return self.incomplete(pc).map(Some);
                }
                if result.value == Atom::Never.fact() {
                    return Ok(Some([None, None]));
                }
                let change = Change::Mutation {
                    address: &address,
                    method: None,
                    args: &[],
                    fresh: value.fresh,
                };
                if let Some(edges) = self.publish(state, pc, &address, result.receiver, change)? {
                    return Ok(Some(edges));
                }
                let mut value = value;
                if address.attached != Attached::No {
                    if value.origin == address.root {
                        value.origin = None;
                    }
                    if value
                        .predicate
                        .is_some_and(|predicate| Some(predicate.slot) == address.root)
                    {
                        value.predicate = None;
                    }
                }
                state.stack.push(self.ctx, value)?;
            }
            Op::Binary(_) | Op::AddStore(_) => {
                let instruction = op;
                let op = match op {
                    Op::Binary(op) => op,
                    _ => "+",
                };
                let right = state.stack.data.pop().unwrap();
                let left = state.stack.data.pop().unwrap();
                let result = self
                    .facts
                    .scalar_binary(self.ctx, op, left.value, right.value)?;
                if result.unsupported {
                    return self.incomplete(pc).map(Some);
                }
                let (errors, stops) =
                    self.binary_errors(op, left.value, right.value, result.rejected)?;
                self.emit_error(state, pc, errors)?;
                if result.rejected {
                    self.issue(
                        pc,
                        IssueKind::Binary {
                            op,
                            left: left.value,
                            right: right.value,
                        },
                    )?;
                }
                if stops {
                    return Ok(Some([None, None]));
                }
                let predicate = if matches!(op, "==" | "!=")
                    && !result.rejected
                    && self.facts.known_primitive(self.ctx, left.value)?
                    && self.facts.known_primitive(self.ctx, right.value)?
                {
                    let origin = if left.value == Atom::Nil.fact() {
                        right.origin
                    } else if right.value == Atom::Nil.fact() {
                        left.origin
                    } else {
                        None
                    };
                    origin.map(|slot| Predicate {
                        slot,
                        test: Test::Nil,
                        yes: op == "==",
                    })
                } else {
                    None
                };
                if let Op::AddStore(slot) = instruction {
                    self.store(state, pc, slot, Operand::new(result.value))?;
                }
                state.stack.push(
                    self.ctx,
                    Operand {
                        predicate,
                        ..Operand::new(result.value)
                    },
                )?;
            }
            _ => unreachable!(),
        }
        Ok(None)
    }
}
