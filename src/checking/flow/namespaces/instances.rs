use super::*;

impl Walker<'_> {
    pub(in super::super) fn instance_operator(
        &mut self,
        receiver: Fact,
        name: &str,
    ) -> Result<bool> {
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let receiver = self.facts.arm(receiver, i);
            if !matches!(self.facts.node(receiver), Node::Instance { .. }) {
                continue;
            }
            let Some(module) = self.namespace_index(receiver) else {
                return Ok(true);
            };
            for method in &self.program.namespaces[module].instance_methods {
                self.ctx.work_bytes(name.len().max(method.name.len()))?;
                if method.name == name || (name == "!=" && method.name == "==") {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(in super::super) fn self_value(&mut self, module: usize) -> Result<Fact> {
        match self.receiver {
            Some(value) => Ok(value),
            None => self.namespace_value(module),
        }
    }

    pub(in super::super) fn construct(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
    ) -> Result<Option<Fact>> {
        let Some(module) = self.namespace_index(receiver) else {
            self.incomplete(pc)?;
            return Ok(None);
        };
        let Node::TypeValue(class) = *self.facts.node(receiver) else {
            self.incomplete(pc)?;
            return Ok(None);
        };
        let root = self.namespace_slot(state, module) + 2;
        let heap = state.locals.get(self.ctx, root)?.value;
        // Widened allocation counts cannot identify a fresh singleton object.
        let Node::Tuple(entries) = self.facts.node(heap) else {
            self.incomplete(pc)?;
            return Ok(None);
        };
        let slot = entries.data.len();
        let mut entries = Buffer::with_capacity(self.ctx, slot + 1)?;
        if let Node::Tuple(previous) = self.facts.node(heap) {
            entries.extend(self.ctx, &previous.data)?;
        }
        let fields = self.facts.shape_fields(
            self.ctx,
            Buffer::empty(),
            false,
            Atom::String.fact(),
            HashKind::Plain,
        )?;
        entries.push(self.ctx, fields)?;
        let updated = self.facts.tuple(self.ctx, &entries.data)?;
        let address = Address::new(Some(root), heap);
        let args = [fields];
        let change = Change::Mutation {
            address: &address,
            method: Some(crate::bytecode::Method::Push),
            args: &args,
            fresh: true,
        };
        if self
            .publish(state, pc, &address, updated, change)?
            .is_some()
        {
            return Ok(None);
        }
        self.facts.instance(self.ctx, class, slot).map(Some)
    }

    fn instance_fields(&mut self, state: &State, receiver: Fact) -> Result<Option<Fact>> {
        let Node::Instance { slot, .. } = *self.facts.node(receiver) else {
            return Ok(None);
        };
        let Some(module) = self.namespace_index(receiver) else {
            return Ok(None);
        };
        let heap = state
            .locals
            .get(self.ctx, self.namespace_slot(state, module) + 2)?
            .value;
        let mut fields = Buffer::empty();
        for i in 0..self.facts.arm_count(heap) {
            self.ctx.charge(1)?;
            let Node::Tuple(entries) = self.facts.node(self.facts.arm(heap, i)) else {
                return Ok(None);
            };
            let Some(&value) = entries.data.get(slot) else {
                return Ok(None);
            };
            fields.push(self.ctx, value)?;
        }
        self.facts.union(self.ctx, &fields.data).map(Some)
    }

    pub(super) fn instance_field(
        &mut self,
        state: &State,
        receiver: Fact,
        name: &str,
    ) -> Result<Selected> {
        let Some(fields) = self.instance_fields(state, receiver)? else {
            return Ok(Selected {
                value: Atom::Never.fact(),
                missing: false,
                incomplete: true,
            });
        };
        namespaces::field(self.ctx, self.facts, fields, name)
    }

    fn instance_root(&mut self, state: &State, receiver: Fact) -> Result<Option<Address>> {
        let Node::Instance { slot, .. } = *self.facts.node(receiver) else {
            return Ok(None);
        };
        let Some(module) = self.namespace_index(receiver) else {
            return Ok(None);
        };
        if self.instance_fields(state, receiver)?.is_none() {
            return Ok(None);
        }
        let root = self.namespace_slot(state, module) + 2;
        let heap = state.locals.get(self.ctx, root)?.value;
        let mut address = Address::new(Some(root), heap);
        let key = self.facts.integer(self.ctx, slot as i64)?;
        address.index(self.ctx, self.facts, &[key])?;
        Ok(Some(address))
    }

    pub(in super::super) fn instance_read(
        &mut self,
        state: &mut State,
        pc: usize,
        name: &str,
    ) -> Result<bool> {
        let Some(receiver) = self.receiver else {
            self.namespace_name_error(state, pc)?;
            return Ok(false);
        };
        let field = self.instance_field(state, receiver, name)?;
        if field.incomplete {
            self.incomplete(pc)?;
            return Ok(false);
        }
        let value = if field.missing {
            self.facts
                .union(self.ctx, &[field.value, Atom::Nil.fact()])?
        } else {
            field.value
        };
        state.stack.push(self.ctx, Operand::new(value))?;
        Ok(true)
    }

    pub(in super::super) fn instance_store(
        &mut self,
        state: &mut State,
        pc: usize,
        name: &str,
        operand: Operand,
    ) -> Result<Option<Fact>> {
        let Some(receiver) = self.receiver else {
            self.namespace_name_error(state, pc)?;
            return Ok(None);
        };
        let Some(value) = self.normalize_property(state, pc, receiver, name, operand.value)? else {
            return Ok(None);
        };
        if !self.instance_write(state, pc, receiver, name, Operand { value, ..operand })? {
            return Ok(None);
        }
        Ok(Some(value))
    }

    pub(super) fn instance_write(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        name: &str,
        operand: Operand,
    ) -> Result<bool> {
        let Some(mut address) = self.instance_root(state, receiver)? else {
            self.incomplete(pc)?;
            return Ok(false);
        };
        let fields = address.value;
        let key = self.facts.string(self.ctx, name.as_bytes())?;
        address.target(self.ctx, self.facts, &[key], false)?;
        let result = self
            .facts
            .collection_write(self.ctx, fields, key, operand.value)?;
        if result.unsupported || result.rejected {
            self.incomplete(pc)?;
            return Ok(false);
        }
        let change = Change::Mutation {
            address: &address,
            method: None,
            args: &[],
            fresh: operand.fresh,
        };
        Ok(self
            .publish(state, pc, &address, result.receiver, change)?
            .is_none())
    }

    pub(in super::super) fn instance_address(
        &mut self,
        state: &mut State,
        pc: usize,
        name: &str,
    ) -> Result<Option<Edges>> {
        let Some(receiver) = self.receiver else {
            self.namespace_name_error(state, pc)?;
            return Ok(Some([None, None]));
        };
        let field = self.instance_field(state, receiver, name)?;
        if field.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if field.missing {
            let value = self
                .facts
                .union(self.ctx, &[field.value, Atom::Nil.fact()])?;
            if !self.instance_write(state, pc, receiver, name, Operand::new(value))? {
                return Ok(Some([None, None]));
            }
        }
        let Some(mut address) = self.instance_root(state, receiver)? else {
            return self.incomplete(pc).map(Some);
        };
        let key = self.facts.string(self.ctx, name.as_bytes())?;
        address.index(self.ctx, self.facts, &[key])?;
        address.instance = Some((receiver, key));
        state.addresses.push(self.ctx, address)?;
        Ok(None)
    }

    fn property_type(&mut self, receiver: Fact, name: &str) -> Result<Option<usize>> {
        let Some(module) = self.namespace_index(receiver) else {
            return Ok(None);
        };
        let mut getter = None;
        let mut setter = None;
        for method in &self.program.namespaces[module].instance_methods {
            self.ctx.work_bytes(name.len().max(method.name.len()))?;
            if method.name.strip_suffix('=') == Some(name) {
                setter = Some(method.function)
            }
            if method.name == name {
                getter = Some(method.function)
            }
        }
        Ok(if let Some(setter) = setter {
            let function = &self.program.functions[setter];
            if function
                .accessor
                .as_ref()
                .is_some_and(|(field, setter)| field == name && *setter)
            {
                function.params.first().and_then(|param| param.ty)
            } else {
                None
            }
        } else {
            getter.and_then(|getter| {
                let function = &self.program.functions[getter];
                function
                    .accessor
                    .as_ref()
                    .filter(|(field, setter)| field == name && !setter)
                    .and(function.return_type)
            })
        })
    }

    fn normalize_property(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        name: &str,
        actual: Fact,
    ) -> Result<Option<Fact>> {
        let Some(ty) = self.property_type(receiver, name)? else {
            return Ok(Some(actual));
        };
        let Some(expected) = self.property_contract(state, pc, ty)? else {
            return Ok(None);
        };
        let relation = self.facts.relation(self.ctx, actual, expected)?;
        if relation != Relation::Accepted {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?
        }
        if relation == Relation::Rejected {
            self.issue(pc, IssueKind::Property { actual, expected })?;
            if !self.facts.overlaps(self.ctx, actual, expected)? {
                return Ok(None);
            }
        }
        let value = self.facts.normalized(self.ctx, actual, expected)?;
        Ok((value != Atom::Never.fact()).then_some(value))
    }

    pub(in super::super) fn guard_instance(
        &mut self,
        state: &mut State,
        pc: usize,
        address: &Address,
        updated: Fact,
    ) -> Result<Option<Fact>> {
        let Some((receiver, key)) = address.instance else {
            return Ok(Some(updated));
        };
        let Node::Instance { slot, .. } = *self.facts.node(receiver) else {
            unreachable!()
        };
        let Node::String(name) = self.facts.node(key) else {
            unreachable!()
        };
        let name = name.clone();
        let name = std::str::from_utf8(name.as_bytes().unwrap()).unwrap();
        let index = self.facts.integer(self.ctx, slot as i64)?;
        let fields = self.facts.collection_index(self.ctx, updated, &[index])?;
        let value = self
            .facts
            .collection_index(self.ctx, fields.value, &[key])?;
        let Some(normalized) = self.normalize_property(state, pc, receiver, name, value.value)?
        else {
            return Ok(None);
        };
        let fields = self
            .facts
            .collection_write(self.ctx, fields.value, key, normalized)?;
        let heap = self
            .facts
            .collection_write(self.ctx, updated, index, fields.receiver)?;
        if fields.unsupported || heap.unsupported {
            self.incomplete(pc)?;
            return Ok(None);
        }
        Ok(Some(heap.receiver))
    }
}
