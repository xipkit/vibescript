use super::*;
use crate::checking::type_bindings::{Binding as TypeBinding, Bindings, Resolution, Scope};

enum Contract {
    Value(Option<Fact>),
    Pending(usize),
}

pub(super) struct Normalization {
    pub value: Option<Fact>,
    pub alternatives: Buffer<State>,
}

pub(super) struct PredicateType {
    pub value: Option<Resolution>,
    pub alternatives: Buffer<State>,
}

impl Normalization {
    pub fn new(value: Option<Fact>) -> Self {
        Self {
            value,
            alternatives: Buffer::empty(),
        }
    }
}

impl Walker<'_> {
    pub(super) fn normalization_contract(
        &mut self,
        state: &mut State,
        pc: usize,
        ty: usize,
    ) -> Result<Normalization> {
        self.prepared_contract(state, pc, ty, true)
    }

    pub(super) fn bind_supplied(
        &mut self,
        mut state: State,
        pc: usize,
        index: usize,
        target: usize,
        actual: Fact,
    ) -> Result<Option<State>> {
        let parameter = &self.function.params[index];
        let mut value = actual;
        if let Some(ty) = parameter.ty {
            let normalized = self.normalization_contract(&mut state, pc, ty)?;
            for alternative in normalized.alternatives.data {
                if let Some(next) = self.bind_supplied(alternative, pc, index, target, actual)? {
                    self.extra.push(self.ctx, (target, next))?;
                }
            }
            let Some(expected) = normalized.value else {
                return Ok(None);
            };
            let actual = if self.general {
                super::super::arguments::general_input(
                    self.ctx,
                    self.facts,
                    parameter.kind,
                    Some(expected),
                )?
            } else {
                actual
            };
            let relation = self.facts.relation(self.ctx, actual, expected)?;
            if relation != Relation::Accepted {
                self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
            }
            if relation == Relation::Rejected {
                self.issue(
                    pc,
                    IssueKind::Call {
                        target: Target::Function(self.source.callable(self.function_index)),
                        failure: Failure::Type {
                            parameter: index,
                            actual,
                            expected,
                        },
                    },
                )?;
                if !self.facts.overlaps(self.ctx, actual, expected)? {
                    return Ok(None);
                }
            }
            value = self.facts.normalized(self.ctx, actual, expected)?;
        }
        if self.general {
            let Some(actual) = self.general_value(&mut state, pc, value)? else {
                return Ok(None);
            };
            value = actual;
        }
        state.store(self.ctx, self.facts, parameter.slot, value)?;
        Ok(Some(state))
    }

    pub(super) fn property_contract(
        &mut self,
        state: &mut State,
        pc: usize,
        source: SourceId,
        ty: usize,
    ) -> Result<Normalization> {
        if source == self.source {
            return self.prepared_contract(state, pc, ty, false);
        }
        let mut result = Normalization::new(None);
        loop {
            let globals = state.global_call(self.ctx)?;
            let annotation = self
                .calls
                .annotation(self.ctx, self.facts, source, ty, &globals)?;
            self.emit_error(state, pc, annotation.throws)?;
            match annotation.resolution {
                Resolution::Known(value) => {
                    result.value = (value != Atom::Never.fact()).then_some(value);
                    return Ok(result);
                }
                failure @ (Resolution::Missing | Resolution::Ambiguous) => {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    self.issue(
                        pc,
                        IssueKind::TypeFactBinding {
                            expected: annotation.expected,
                            ambiguous: failure == Resolution::Ambiguous,
                        },
                    )?;
                    return Ok(result);
                }
                Resolution::Dynamic => {
                    self.incomplete(pc)?;
                    return Ok(result);
                }
                Resolution::Pending(root) => {
                    let slot = state.global_base + state.source_slots.roots.data[root];
                    let Some(branches) = self.import_root_branches(state, pc, slot)? else {
                        return Ok(result);
                    };
                    for branch in branches.data {
                        result.alternatives.push(self.ctx, branch)?;
                    }
                }
            }
        }
    }

    fn prepared_contract(
        &mut self,
        state: &mut State,
        pc: usize,
        ty: usize,
        lexical: bool,
    ) -> Result<Normalization> {
        let mut result = Normalization::new(None);
        loop {
            match self.contract(state, pc, ty, lexical)? {
                Contract::Value(value) => {
                    result.value = value;
                    return Ok(result);
                }
                Contract::Pending(index) => {
                    let slot = state.global_base + state.source_slots.roots.data[index];
                    let Some(branches) = self.import_root_branches(state, pc, slot)? else {
                        return Ok(result);
                    };
                    for branch in branches.data {
                        result.alternatives.push(self.ctx, branch)?;
                    }
                }
            }
        }
    }

    fn contract(&mut self, state: &State, pc: usize, ty: usize, lexical: bool) -> Result<Contract> {
        if !self.layouts.named_annotation(self.ctx, ty)? {
            return Ok(Contract::Value(Some(self.contracts[ty])));
        }
        let Some((bindings, scopes)) = self.type_environment(state, pc, lexical, false)? else {
            return Ok(Contract::Value(None));
        };
        let mut failure = None;
        let mut dynamic = false;
        let mut pending = None;
        let fact = self
            .facts
            .annotation(self.ctx, &self.program.types[ty], |ctx, name| {
                if failure.is_some() || pending.is_some() || dynamic {
                    return Ok(None);
                }
                let resolution = bindings.resolve(ctx, &scopes.data, name, false)?;
                match resolution {
                    Resolution::Pending(index) => {
                        pending.get_or_insert(index);
                        Ok(None)
                    }
                    Resolution::Known(fact) => Ok(Some(fact)),
                    Resolution::Missing | Resolution::Ambiguous => {
                        failure.get_or_insert(resolution);
                        Ok(None)
                    }
                    Resolution::Dynamic => {
                        dynamic = true;
                        Ok(None)
                    }
                }
            })?;
        if let Some(index) = pending {
            return Ok(Contract::Pending(index));
        }
        if dynamic {
            self.incomplete(pc)?;
            return Ok(Contract::Value(None));
        }
        if let Some(failure) = failure {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            self.issue(
                pc,
                IssueKind::TypeBinding {
                    ty,
                    ambiguous: failure == Resolution::Ambiguous,
                },
            )?;
            return Ok(Contract::Value(None));
        }
        Ok(Contract::Value(Some(fact)))
    }
    fn type_environment(
        &mut self,
        state: &State,
        pc: usize,
        lexical: bool,
        current: bool,
    ) -> Result<Option<(Bindings, Buffer<Scope>)>> {
        let mut bindings = Bindings::new();
        let hosts = bindings.scope(self.ctx)?;
        self.calls.type_bindings(self.ctx, &mut bindings, hosts)?;
        for (index, root) in self.roots.iter().enumerate() {
            self.ctx.charge(1)?;
            let slot = state.global_base + state.source_slots.roots.data[index];
            let value = state.locals.get(self.ctx, slot)?;
            let binding = if value.missing {
                TypeBinding::Pending(index)
            } else {
                bindings.current(self.ctx, self.facts, value.value)?
            };
            bindings.insert(self.ctx, hosts, root.name.as_bytes().unwrap(), binding)?;
        }
        for (receiving, name, slot) in state.global_layout.roots() {
            self.ctx.charge(1)?;
            if *receiving != state.source_slots.receiving
                || state.source_slots.root(self.ctx, *slot)?.is_some()
            {
                continue;
            }
            let value = state.locals.get(self.ctx, state.global_base + slot)?;
            if value.value == Atom::Never.fact() {
                continue;
            }
            let binding = bindings.current(self.ctx, self.facts, value.value)?;
            if value.missing {
                bindings.optional(self.ctx, hosts, name.as_bytes().unwrap(), binding)?;
            } else {
                bindings.insert(self.ctx, hosts, name.as_bytes().unwrap(), binding)?;
            }
        }
        let sources = if lexical {
            self.layouts.type_sources(self.ctx, self.function_index)?
        } else {
            &[]
        };
        let mut levels: Buffer<(usize, Scope)> = Buffer::empty();
        for source in sources {
            self.ctx.charge(1)?;
            let name = &self.program.functions[source.function].local_names[source.name];
            let value = state.locals.get(self.ctx, source.capture)?;
            if value.value == Atom::Never.fact() && value.missing {
                continue;
            }
            let binding = bindings.current(self.ctx, self.facts, value.value)?;
            if binding == TypeBinding::Other {
                continue;
            }
            let blocks::Owner::Function(owner) = value.owner else {
                self.incomplete(pc)?;
                return Ok(None);
            };
            if owner.source != self.source {
                self.incomplete(pc)?;
                return Ok(None);
            }
            if owner.index == self.function_index {
                continue;
            }
            let Some(depth) = self
                .layouts
                .depth(self.ctx, self.function_index, owner.index)?
            else {
                self.incomplete(pc)?;
                return Ok(None);
            };
            self.ctx.charge(levels.data.len() as u64)?;
            let scope =
                if let Some((_, scope)) = levels.data.iter().find(|(index, _)| *index == depth) {
                    *scope
                } else {
                    let scope = bindings.scope(self.ctx)?;
                    levels.push(self.ctx, (depth, scope))?;
                    scope
                };
            if value.missing {
                bindings.optional(self.ctx, scope, name.as_bytes(), binding)?;
            } else {
                bindings.insert(self.ctx, scope, name.as_bytes(), binding)?;
            }
        }
        self.ctx.charge(
            levels
                .data
                .len()
                .saturating_mul(levels.data.len().max(1).ilog2() as usize + 1) as u64,
        )?;
        levels.data.sort_unstable_by_key(|(depth, _)| *depth);
        let mut scopes = Buffer::empty();
        if current {
            let scope = bindings.scope(self.ctx)?;
            for (slot, name) in self.function.local_names.iter().enumerate() {
                self.ctx.charge(1)?;
                let value = state.locals.get(self.ctx, slot)?;
                if value.value == Atom::Never.fact() && value.missing {
                    continue;
                }
                let binding = bindings.current(self.ctx, self.facts, value.value)?;
                if value.missing {
                    bindings.optional(self.ctx, scope, name.as_bytes(), binding)?;
                } else {
                    bindings.insert(self.ctx, scope, name.as_bytes(), binding)?;
                }
            }
            scopes.push(self.ctx, scope)?;
        }
        for (_, scope) in levels.data {
            self.ctx.charge(1)?;
            scopes.push(self.ctx, scope)?;
        }
        if let Some(parent) = self.ambient.filter(|_| lexical) {
            let scope = bindings.scope(self.ctx)?;
            let base = self
                .layouts
                .locals(self.ctx, self.program, self.function_index)?;
            for (index, name) in self.program.functions[parent]
                .local_names
                .iter()
                .enumerate()
            {
                self.ctx.charge(1)?;
                let value = state.locals.get(self.ctx, base + index)?;
                if value.value == Atom::Never.fact() && value.missing {
                    continue;
                }
                let binding = bindings.current(self.ctx, self.facts, value.value)?;
                if value.missing {
                    bindings.optional(self.ctx, scope, name.as_bytes(), binding)?;
                } else {
                    bindings.insert(self.ctx, scope, name.as_bytes(), binding)?;
                }
            }
            scopes.push(self.ctx, scope)?;
        }
        if self.program.file {
            let scope = bindings.scope(self.ctx)?;
            let base = state.global_base + state.source_slots.files.start;
            for (index, name) in self.layouts.files.names.data.iter().enumerate() {
                self.ctx.charge(1)?;
                let value = state.locals.get(self.ctx, base + index)?;
                if value.value == Atom::Never.fact() && value.missing {
                    continue;
                }
                let binding = bindings.current(self.ctx, self.facts, value.value)?;
                if value.missing {
                    bindings.optional(self.ctx, scope, name.as_bytes().unwrap(), binding)?;
                } else {
                    bindings.insert(self.ctx, scope, name.as_bytes().unwrap(), binding)?;
                }
            }
            scopes.push(self.ctx, scope)?;
        }
        let source = bindings.scope(self.ctx)?;
        for (index, declaration) in self.program.declarations.iter().enumerate() {
            self.ctx.charge(1)?;
            let name = match &declaration.0 {
                Kind::Namespace(value) => &value.definition.name,
                Kind::Enum(value) => &value.definition.name,
                _ => unreachable!(),
            };
            let value = self.declaration_value(state, index)?;
            let binding = bindings.current(self.ctx, self.facts, value)?;
            bindings.insert(self.ctx, source, name.as_bytes(), binding)?;
        }
        for (index, (global, _)) in self.program.globals.iter().enumerate() {
            self.ctx.charge(1)?;
            let value = state
                .locals
                .get(
                    self.ctx,
                    state.global_base + state.source_slots.globals.data[index],
                )?
                .value;
            let value = self.file_type_value(state, global.name(), value)?;
            let binding = bindings.current(self.ctx, self.facts, value)?;
            bindings.insert(self.ctx, source, global.name().as_bytes(), binding)?;
        }
        bindings.overlay(self.ctx, source, &[hosts])?;
        scopes.extend(self.ctx, &[hosts, source])?;
        if self.program.file {
            let globals = state.global_call(self.ctx)?;
            if let Some(receiving) =
                self.calls
                    .receiving_types(self.ctx, self.facts, &globals, &mut bindings)?
            {
                scopes.push(self.ctx, receiving)?;
            }
        }
        Ok(Some((bindings, scopes)))
    }

    pub(super) fn predicate_type(
        &mut self,
        state: &mut State,
        pc: usize,
        name: &str,
    ) -> Result<PredicateType> {
        let mut result = PredicateType {
            value: None,
            alternatives: Buffer::empty(),
        };
        loop {
            let Some((bindings, scopes)) = self.type_environment(state, pc, true, true)? else {
                return Ok(result);
            };
            let resolution = bindings.resolve(self.ctx, &scopes.data, name, true)?;
            if let Resolution::Pending(index) = resolution {
                let slot = state.global_base + state.source_slots.roots.data[index];
                let Some(branches) = self.import_root_branches(state, pc, slot)? else {
                    return Ok(result);
                };
                for branch in branches.data {
                    result.alternatives.push(self.ctx, branch)?;
                }
            } else {
                result.value = Some(resolution);
                return Ok(result);
            }
        }
    }
}
