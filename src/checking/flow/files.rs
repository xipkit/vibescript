use super::*;
use crate::checking::file_bindings;

impl<'a> Walker<'a> {
    pub(super) fn file_slot(&mut self, state: &State, name: &str) -> Result<Option<usize>> {
        Ok(self
            .layouts
            .files
            .index(self.ctx, name)?
            .map(|index| state.global_base + self.program.globals.len() + self.roots.len() + index))
    }

    pub(super) fn file_binding(
        &mut self,
        state: &State,
        name: &str,
    ) -> Result<Option<(usize, Binding)>> {
        let Some(slot) = self.file_slot(state, name)? else {
            return Ok(None);
        };
        let binding = state.locals.get(self.ctx, slot)?;
        Ok((binding.value != Atom::Never.fact()).then_some((slot, binding)))
    }

    pub(super) fn file_type_value(
        &mut self,
        state: &State,
        name: &str,
        original: Fact,
    ) -> Result<Fact> {
        if let Some((_, binding)) = self.file_binding(state, name)? {
            return if binding.missing {
                self.facts.union(self.ctx, &[binding.value, original])
            } else {
                Ok(binding.value)
            };
        }
        Ok(original)
    }

    fn file_name(&self, op: Op) -> Option<&'a str> {
        if let Some(slot) = file_bindings::local(op) {
            return self.function.local_names.get(slot).map(String::as_str);
        }
        match op {
            Op::FileValue(name, _)
            | Op::FileAddress(name, _)
            | Op::RootAddress(name, _)
            | Op::Unbound(name)
            | Op::ResolveCall(_, name, _)
            | Op::CallName(_, name) => Some(&self.program.members[name]),
            Op::Global(index)
            | Op::GlobalReceiver(index, _)
            | Op::AddressGlobal(index)
            | Op::ResolveGlobalCall(index) => Some(self.program.globals[index].0.name()),
            Op::Declaration(index) => Some(file_bindings::declaration_name(self.program, index)),
            Op::AutoCall(index) => Some(&self.program.functions[index].name),
            Op::HostValue(index) => Some(&self.program.hosts[index]),
            _ => None,
        }
    }

    pub(super) fn file_variants(&mut self, state: &State, op: Op) -> Result<Option<[State; 2]>> {
        if !self.program.file || !file_bindings::branches(op) {
            return Ok(None);
        }
        let local = file_bindings::local(op).or(match op {
            Op::ResolveCall(slot, _, _) | Op::CallName(slot, _) if slot != usize::MAX => Some(slot),
            _ => None,
        });
        if let Some(slot) = local {
            if let Some(states) = self.split_file_presence(state, slot)? {
                return Ok(Some(states));
            }
        }
        let Some(name) = self.file_name(op) else {
            return Ok(None);
        };
        let name = name.as_bytes();
        let mut file = None;
        for (index, candidate) in self.layouts.files.names.data.iter().enumerate() {
            let candidate = candidate.as_bytes().unwrap();
            self.ctx.work_bytes(candidate.len().max(name.len()))?;
            if candidate == name {
                file = Some(index);
                break;
            }
        }
        match file {
            Some(index) => self.split_file_presence(
                state,
                state.global_base + self.program.globals.len() + self.roots.len() + index,
            ),
            None => Ok(None),
        }
    }

    fn split_file_presence(&mut self, state: &State, slot: usize) -> Result<Option<[State; 2]>> {
        let binding = state.locals.get(self.ctx, slot)?;
        if !binding.missing || binding.value == Atom::Never.fact() {
            return Ok(None);
        }
        let mut present = state.snapshot(self.ctx)?;
        let mut absent = state.snapshot(self.ctx)?;
        present.locals.set(
            self.ctx,
            slot,
            Binding {
                missing: false,
                ..binding
            },
        )?;
        absent.locals.set(
            self.ctx,
            slot,
            Binding {
                value: Atom::Never.fact(),
                ..binding
            },
        )?;
        Ok(Some([present, absent]))
    }

    fn file_root_bound(&mut self, name: &str) -> Result<bool> {
        self.ctx.work_bytes(name.len())?;
        Ok(self.file_declared_target(name)?.is_some()
            || crate::builtin::Global::parse(name).is_some()
            || self.calls.global(self.ctx, name)?)
    }

    fn file_local(&mut self, state: &State, slot: usize) -> Result<bool> {
        if slot >= self.function.local_names.len() {
            return Ok(false);
        }
        let name = &self.function.local_names[slot];
        self.ctx.charge(self.function.params.len() as u64 + 1)?;
        if name.starts_with('\0')
            || self.function.params.iter().any(|param| param.slot == slot)
            || !state.locals.get(self.ctx, slot)?.missing
        {
            return Ok(false);
        }
        if self.function_index == 0 {
            return Ok(true);
        }
        if self
            .layouts
            .initializer_block(self.ctx, self.program, self.function_index)?
        {
            return Ok(false);
        }
        Ok(self.file_binding(state, name)?.is_some() || self.file_root_bound(name)?)
    }

    /// Redirects only bindings owned by the file; a skipped declaration does not shadow a root.
    pub(super) fn file_op(&mut self, state: &State, op: Op) -> Result<Option<Op>> {
        if !self.program.file {
            return Ok(Some(op));
        }
        let Some(slot) = file_bindings::local(op) else {
            return Ok(Some(op));
        };
        if !self.file_local(state, slot)? {
            return Ok(Some(op));
        }
        let name = &self.function.local_names[slot];
        if matches!(op, Op::Declare(_))
            && self.file_binding(state, name)?.is_none()
            && self.file_root_bound(name)?
        {
            return Ok(None);
        }
        let file = self.file_slot(state, name)?.unwrap();
        Ok(Some(match op {
            Op::Load(_) => Op::Load(file),
            Op::LoadOptional(_, name) => Op::LoadOptional(file, name),
            Op::ReceiverBound(_, next) => Op::ReceiverBound(file, next),
            Op::Declare(_) => Op::Declare(file),
            Op::Store(_) => Op::Store(file),
            Op::AddStore(_) => Op::AddStore(file),
            Op::AddressLocal(_) => Op::AddressLocal(file),
            Op::AddressBound(_, next) => Op::AddressBound(file, next),
            _ => unreachable!(),
        }))
    }

    pub(super) fn file_target(&mut self, state: &State, name: &str) -> Result<Option<Target>> {
        if !self.program.file {
            return Ok(None);
        }
        if let Some((_, binding)) = self.file_binding(state, name)? {
            return Ok(Some(if binding.missing {
                Target::Unsupported
            } else {
                self.value_target(binding.value)?
            }));
        }
        self.file_declared_target(name)
    }

    pub(super) fn file_declared_target(&mut self, name: &str) -> Result<Option<Target>> {
        if !self.program.file {
            return Ok(None);
        }
        self.ctx.work_bytes(name.len())?;
        if self.program.declaration_names.contains_key(name) {
            return Ok(Some(Target::NonCallable));
        }
        if let Some(&function) = self.program.names.get(name) {
            return Ok(Some(Target::Function(function)));
        }
        for (index, host) in self.program.hosts.iter().enumerate() {
            self.ctx.work_bytes(host.len().max(name.len()))?;
            if host == name {
                return Ok(Some(Target::Host(index)));
            }
        }
        Ok(None)
    }

    pub(super) fn file_edges(
        &mut self,
        mut state: State,
        pc: usize,
        name: usize,
        next: usize,
        address: bool,
    ) -> Result<Edges> {
        let name_index = name;
        let name = &self.program.members[name];
        if address {
            let mut local = None;
            for (slot, candidate) in self.function.local_names.iter().enumerate() {
                self.ctx.work_bytes(candidate.len().max(name.len()))?;
                if candidate == name {
                    local = Some(slot);
                    break;
                }
            }
            if let Some(slot) = local {
                if !self.file_local(&state, slot)? {
                    return Ok([Some((pc + 1, state)), None]);
                }
            } else {
                if let Some(field) = self.namespace_constant(&state, name, false)? {
                    if field.incomplete {
                        return self.incomplete(pc);
                    }
                    if field.missing {
                        let module = self.function.namespace.unwrap();
                        for present in [true, false] {
                            let mut variant = state.snapshot(self.ctx)?;
                            self.refine_namespace(&mut variant, module, name, present)?;
                            let edges = self.file_edges(variant, pc, name_index, next, true)?;
                            for edge in edges.into_iter().flatten() {
                                self.extra.push(self.ctx, edge)?;
                            }
                        }
                        return Ok([None, None]);
                    }
                    return Ok([Some((pc + 1, state)), None]);
                }
                if self.ambient_binding(&state, name)?.is_some() {
                    return Ok([Some((pc + 1, state)), None]);
                }
            }
        }
        let binding = self.file_binding(&state, name)?;
        let slot = if let Some((slot, binding)) = binding {
            debug_assert!(!binding.missing);
            Some(slot)
        } else if address || self.file_declared_target(name)?.is_none() {
            if let Some(index) = self.root_index(name)? {
                let slot = state.global_base + index;
                if !self.import_root(&mut state, pc, slot)? {
                    return Ok([None, None]);
                }
                Some(slot)
            } else {
                None
            }
        } else {
            None
        };
        let Some(slot) = slot else {
            return Ok([Some((pc + 1, state)), None]);
        };
        let value = state.locals.get(self.ctx, slot)?.value;
        if address {
            state
                .addresses
                .push(self.ctx, Address::new(Some(slot), value))?;
            return Ok([Some((next, state)), None]);
        }
        let start = self.extra.data.len();
        let readable = self.read_value(&mut state, pc, value, Some(slot))?;
        for (target, _) in &mut self.extra.data[start..] {
            self.ctx.charge(1)?;
            if *target == pc + 1 {
                *target = next;
            }
        }
        Ok([readable.then_some((next, state)), None])
    }
}
