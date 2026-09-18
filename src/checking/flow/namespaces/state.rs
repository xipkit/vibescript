use super::*;

impl Walker<'_> {
    pub(in super::super) fn resolve_name(
        &mut self,
        state: &mut State,
        pc: usize,
        op: Op,
    ) -> Result<Option<Edges>> {
        let (slot, name, named, parenthesized) = match op {
            Op::ResolveCall(slot, name, parenthesized) => (slot, name, true, parenthesized),
            Op::CallName(slot, name) => (slot, name, false, true),
            _ => unreachable!(),
        };
        let unbound =
            slot == usize::MAX || state.locals.get(self.ctx, slot)?.value == Atom::Never.fact();
        if unbound {
            if let Some(field) =
                self.namespace_constant(state, &self.program.members[name], named)?
            {
                if field.incomplete {
                    return self.incomplete(pc).map(Some);
                }
                if field.missing {
                    let module = self.function.namespace.unwrap();
                    for present in [true, false] {
                        let mut next = state.snapshot(self.ctx)?;
                        self.refine_namespace(
                            &mut next,
                            module,
                            &self.program.members[name],
                            present,
                        )?;
                        let edges = self.resolve_name(&mut next, pc, op)?;
                        self.member_edges(pc, next, edges)?;
                    }
                    return Ok(Some([None, None]));
                }
            }
        }
        if let Some(root) = self.root_read_slot(state, op)? {
            if !self.import_root(state, pc, root)? {
                return Ok(Some([None, None]));
            }
        }
        let Some(target) = self.target(state, pc, slot, name, named)? else {
            return Ok(Some([None, None]));
        };
        if target == Target::Undefined {
            self.namespace_name_error(state, pc)?;
            return Ok(Some([None, None]));
        }
        if named {
            state.arguments.push(
                self.ctx,
                Pending {
                    target,
                    receiver: None,
                    arguments: Arguments::new(),
                },
            )?;
        } else {
            state.arguments.data.last_mut().unwrap().target = target;
        }
        let method = match target {
            Target::Method { constructor, .. } => !constructor,
            Target::Function(function) => self.program.functions[function].namespace.is_some(),
            _ => false,
        };
        state
            .arguments
            .data
            .last_mut()
            .unwrap()
            .arguments
            .options_hash = !parenthesized || !method;
        self.resolve_value_target(state, pc)
    }

    pub(in super::super) fn namespace_member_target(
        &mut self,
        state: &mut State,
        pc: usize,
        site: CallSite,
        read: bool,
    ) -> Result<Option<Edges>> {
        let receiver = state.addresses.data.last().unwrap().value;
        let name = &self.program.members[site.name];
        if let Some(variants) = self.member_variants(receiver, name)? {
            for receiver in variants.data {
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = receiver;
                let edges = self.namespace_member_target(&mut next, pc, site, read)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if self.namespace_index(receiver).is_none() {
            return self.incomplete(pc).map(Some);
        }
        let mut address = Address::new(None, receiver);
        address.member = Some(site.name);
        let key = self.facts.string(self.ctx, name.as_bytes())?;
        address.selectors.push(self.ctx, key)?;
        *state.addresses.data.last_mut().unwrap() = address;
        if !read {
            return Ok(None);
        }
        match self.namespace_selection(state, receiver, name, site.scope, false)? {
            Selection::Call(target) => self.invoke(state, pc, target, Arguments::new()),
            Selection::Field(value, missing) => {
                if missing {
                    self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
                }
                state.stack.push(self.ctx, Operand::new(value))?;
                Ok(None)
            }
            Selection::Incomplete => self.incomplete(pc).map(Some),
            Selection::Rejected => {
                self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
                Ok(Some([None, None]))
            }
        }
    }

    pub(in super::super) fn namespace_member_store(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        name: usize,
        operand: Operand,
    ) -> Result<Option<Edges>> {
        if let Some(variants) = self.member_variants(receiver, &self.program.members[name])? {
            for receiver in variants.data {
                let mut next = state.snapshot(self.ctx)?;
                let edges = self.namespace_member_store(&mut next, pc, receiver, name, operand)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        let Some(module) = self.namespace_index(receiver) else {
            return self.incomplete(pc).map(Some);
        };
        let field = &self.program.members[name];
        let instance = matches!(self.facts.node(receiver), Node::Instance { .. });
        let definition = &self.program.namespaces[module];
        let methods = if instance {
            &definition.instance_methods
        } else {
            &definition.methods
        };
        for method in methods {
            self.ctx.work_bytes(field.len().max(method.name.len()))?;
            if method.name.strip_suffix('=') != Some(field) {
                continue;
            }
            let mut args = Arguments::new();
            args.positional.push(self.ctx, operand.value)?;
            if method.visibility == Visibility::Private
                || (method.visibility == Visibility::Protected
                    && (self.function.namespace != Some(module)
                        || self.function.instance != instance))
            {
                self.namespace_error(state, pc, receiver, name, &args)?;
                return Ok(Some([None, None]));
            }
            let target = if instance {
                Target::Method {
                    function: method.function,
                    receiver,
                    constructor: false,
                }
            } else {
                Target::Function(method.function)
            };
            let edges = self.invoke(state, pc, target, args)?;
            if edges.is_none() {
                state.stack.data.pop().unwrap();
                state.stack.push(self.ctx, Operand::new(operand.value))?;
            }
            return Ok(edges);
        }
        if instance {
            for method in methods {
                self.ctx.work_bytes(field.len().max(method.name.len()))?;
                if method.name == *field {
                    self.namespace_error(state, pc, receiver, name, &Arguments::new())?;
                    return Ok(Some([None, None]));
                }
            }
            if !self.instance_write(state, pc, receiver, field, operand)? {
                return Ok(Some([None, None]));
            }
            state.stack.push(self.ctx, Operand::new(operand.value))?;
            return Ok(None);
        }
        if let Some(edges) = self.namespace_write(state, pc, module, field, operand)? {
            return Ok(Some(edges));
        }
        state.stack.push(self.ctx, Operand::new(operand.value))?;
        Ok(None)
    }

    pub(in super::super) fn namespace_constant_edges(
        &mut self,
        mut state: State,
        pc: usize,
        name: usize,
        next: usize,
    ) -> Result<Edges> {
        let name = &self.program.members[name];
        let Some(field) = self.namespace_constant(&state, name, false)? else {
            return Ok([Some((pc + 1, state)), None]);
        };
        if field.incomplete {
            return self.incomplete(pc);
        }
        let module = self.function.namespace.unwrap();
        let missing = if field.missing {
            let mut missing = state.snapshot(self.ctx)?;
            self.refine_namespace(&mut missing, module, name, false)?;
            Some((pc + 1, missing))
        } else {
            None
        };
        self.refine_namespace(&mut state, module, name, true)?;
        state.stack.push(self.ctx, Operand::new(field.value))?;
        Ok([Some((next, state)), missing])
    }

    fn variable_module(&mut self, state: &State, pc: usize, name: usize) -> Result<Option<usize>> {
        let name = &self.program.members[name];
        if (name.starts_with('@') && !name.starts_with("@@")) || self.function.namespace.is_none() {
            self.namespace_name_error(state, pc)?;
            return Ok(None);
        }
        Ok(self.function.namespace)
    }

    pub(super) fn namespace_name_error(&mut self, state: &State, pc: usize) -> Result<()> {
        self.issue(
            pc,
            IssueKind::Call {
                target: Target::Undefined,
                failure: Failure::Undefined,
            },
        )?;
        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))
    }

    pub(in super::super) fn namespace_variable(
        &mut self,
        state: &mut State,
        pc: usize,
        name: usize,
        optional: bool,
    ) -> Result<bool> {
        if let Some(name) = self.program.members[name]
            .strip_prefix('@')
            .filter(|name| !name.starts_with('@'))
        {
            return self.instance_read(state, pc, name);
        }
        let Some(module) = self.variable_module(state, pc, name)? else {
            return Ok(false);
        };
        let name = &self.program.members[name];
        let name = name.strip_prefix("@@").unwrap_or(name);
        let field = self.namespace_field(state, module, name)?;
        if field.incomplete {
            self.incomplete(pc)?;
            return Ok(false);
        }
        let mut value = field.value;
        if field.missing {
            if optional {
                value = self.facts.union(self.ctx, &[value, Atom::Nil.fact()])?;
            } else {
                self.namespace_name_error(state, pc)?;
            }
        }
        if value == Atom::Never.fact() {
            return Ok(false);
        }
        state.stack.push(self.ctx, Operand::new(value))?;
        Ok(true)
    }

    fn namespace_write(
        &mut self,
        state: &mut State,
        pc: usize,
        module: usize,
        name: &str,
        operand: Operand,
    ) -> Result<Option<Edges>> {
        let slot = self.namespace_slot(state, module);
        let fields = state.locals.get(self.ctx, slot)?.value;
        let key = self.facts.string(self.ctx, name.as_bytes())?;
        let mut address = Address::new(Some(slot), fields);
        address.target(self.ctx, self.facts, &[key], false)?;
        let result = self
            .facts
            .collection_write(self.ctx, fields, key, operand.value)?;
        if result.unsupported || result.rejected {
            return self.incomplete(pc).map(Some);
        }
        let change = Change::Mutation {
            address: &address,
            method: None,
            args: &[],
            fresh: operand.fresh,
        };
        self.publish(state, pc, &address, result.receiver, change)
    }

    pub(in super::super) fn namespace_store(
        &mut self,
        state: &mut State,
        pc: usize,
        name: usize,
    ) -> Result<Option<Edges>> {
        if let Some(name) = self.program.members[name]
            .strip_prefix('@')
            .filter(|name| !name.starts_with('@'))
        {
            let operand = *state.stack.data.last().unwrap();
            let Some(value) = self.instance_store(state, pc, name, operand)? else {
                return Ok(Some([None, None]));
            };
            state.stack.data.last_mut().unwrap().value = value;
            return Ok(None);
        }
        let Some(module) = self.variable_module(state, pc, name)? else {
            return Ok(Some([None, None]));
        };
        let name = &self.program.members[name];
        let name = name.strip_prefix("@@").unwrap_or(name);
        let operand = *state.stack.data.last().unwrap();
        self.namespace_write(state, pc, module, name, operand)
    }

    fn push_field_address(&mut self, state: &mut State, module: usize, name: &str) -> Result<()> {
        let slot = self.namespace_slot(state, module);
        let fields = state.locals.get(self.ctx, slot)?.value;
        let key = self.facts.string(self.ctx, name.as_bytes())?;
        let mut address = Address::new(Some(slot), fields);
        address.index(self.ctx, self.facts, &[key])?;
        state.addresses.push(self.ctx, address)
    }

    fn namespace_address_fallback(
        &mut self,
        state: &mut State,
        pc: usize,
        name: &str,
    ) -> Result<bool> {
        self.ctx.work_bytes(name.len())?;
        let address = if let Some((slot, binding)) = self.ambient_binding(state, name)? {
            if binding.missing {
                self.incomplete(pc)?;
                return Ok(false);
            }
            Address::new(Some(slot), binding.value)
        } else if let Some(&index) = self.program.declaration_names.get(name) {
            Address::new(None, self.load_declaration(state, pc, index)?)
        } else {
            let mut global = None;
            for (index, (key, _)) in self.program.globals.iter().enumerate() {
                self.ctx.work_bytes(name.len().max(key.name().len()))?;
                if key.name() == name {
                    global = Some(index);
                    break;
                }
            }
            let Some(index) = global else {
                self.namespace_name_error(state, pc)?;
                return Ok(false);
            };
            let Some(index) = self.global_index(state, index)? else {
                self.incomplete(pc)?;
                return Ok(false);
            };
            let slot = state.global_base + index;
            if (self.program.globals.len()..self.program.globals.len() + self.roots.len())
                .contains(&index)
                && !self.import_root(state, pc, slot)?
            {
                return Ok(false);
            }
            let Some(address) = self.global_address(state, pc, index)? else {
                return Ok(false);
            };
            address
        };
        state.addresses.push(self.ctx, address)?;
        Ok(true)
    }

    pub(in super::super) fn namespace_address(
        &mut self,
        state: &mut State,
        pc: usize,
        name: usize,
        optional: bool,
    ) -> Result<Option<Edges>> {
        if let Some(name) = self.program.members[name]
            .strip_prefix('@')
            .filter(|name| !name.starts_with('@'))
        {
            return self.instance_address(state, pc, name);
        }
        let Some(module) = self.variable_module(state, pc, name)? else {
            return Ok(Some([None, None]));
        };
        let name = &self.program.members[name];
        let name = name.strip_prefix("@@").unwrap_or(name);
        let field = self.namespace_field(state, module, name)?;
        if field.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if field.missing {
            if optional {
                let value = self
                    .facts
                    .union(self.ctx, &[field.value, Atom::Nil.fact()])?;
                if let Some(edges) =
                    self.namespace_write(state, pc, module, name, Operand::new(value))?
                {
                    return Ok(Some(edges));
                }
            } else {
                let mut missing = state.snapshot(self.ctx)?;
                self.refine_namespace(&mut missing, module, name, false)?;
                if self.namespace_address_fallback(&mut missing, pc, name)? {
                    self.native_continue(pc, missing)?;
                }
                if field.value == Atom::Never.fact() {
                    return Ok(Some([None, None]));
                }
                self.refine_namespace(state, module, name, true)?;
            }
        }
        self.push_field_address(state, module, name)?;
        Ok(None)
    }

    pub(in super::super) fn namespace_scope_address(
        &mut self,
        state: &mut State,
        pc: usize,
        site: CallSite,
    ) -> Result<Option<Edges>> {
        let receiver = state.addresses.data.pop().unwrap().value;
        let name = &self.program.members[site.name];
        if matches!(self.facts.node(receiver), Node::Instance { .. }) {
            self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
            return Ok(Some([None, None]));
        }
        let Some(module) = self.namespace_index(receiver) else {
            return self.incomplete(pc).map(Some);
        };
        let field = self.namespace_field(state, module, name)?;
        if field.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if field.missing {
            self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
        }
        if field.value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        self.refine_namespace(state, module, name, true)?;
        self.push_field_address(state, module, name)?;
        Ok(None)
    }

    pub(in super::super) fn initialize_namespace(
        &mut self,
        state: &mut State,
        pc: usize,
        module: usize,
    ) -> Result<Option<Edges>> {
        let slot = self.namespace_slot(state, module) + 1;
        let binding = state.locals.get(self.ctx, slot)?;
        let yes = self
            .facts
            .filter(self.ctx, binding.value, Test::Truth, true)?;
        let no = self
            .facts
            .filter(self.ctx, binding.value, Test::Truth, false)?;
        if no == Atom::Never.fact() {
            return Ok(None);
        }
        if yes != Atom::Never.fact() {
            let mut skipped = state.snapshot(self.ctx)?;
            skipped.locals.set(
                self.ctx,
                slot,
                Binding {
                    value: yes,
                    ..binding
                },
            )?;
            self.native_continue(pc, skipped)?;
        }
        state.locals.set(
            self.ctx,
            slot,
            Binding {
                value: no,
                ..binding
            },
        )?;
        let body = self.program.namespaces[module].body.unwrap();
        self.initialize_with_ambient(state, pc, body)?;
        Ok(Some([None, None]))
    }

    pub(in super::super) fn complete_namespace(&mut self, state: &mut State) -> Result<()> {
        if self.function.initializer {
            let slot = self.namespace_slot(state, self.function.namespace.unwrap()) + 1;
            let value = self.facts.boolean(self.ctx, true)?;
            state.store(self.ctx, self.facts, slot, value)?;
        }
        Ok(())
    }
}
