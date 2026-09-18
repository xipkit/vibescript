use super::*;
use crate::checking::type_bindings::{Binding as TypeBinding, Bindings, Resolution, Scope};

enum Contract {
    Value(Option<Fact>),
    Pending(usize),
}

impl Walker<'_> {
    pub(super) fn normalization_contract(
        &mut self,
        state: &mut State,
        pc: usize,
        ty: usize,
    ) -> Result<Option<Fact>> {
        loop {
            match self.contract(state, pc, ty)? {
                Contract::Value(value) => return Ok(value),
                Contract::Pending(index) => {
                    let slot = state.global_base + self.program.globals.len() + index;
                    if !self.import_root(state, pc, slot)? {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn contract(&mut self, state: &State, pc: usize, ty: usize) -> Result<Contract> {
        if !self.layouts.named_annotation(self.ctx, ty)? {
            return Ok(Contract::Value(Some(self.contracts[ty])));
        }
        let mut bindings = Bindings::new();
        let hosts = bindings.scope(self.ctx)?;
        self.calls.type_bindings(self.ctx, &mut bindings, hosts)?;
        for (index, root) in self.roots.iter().enumerate() {
            self.ctx.charge(1)?;
            let slot = state.global_base + self.program.globals.len() + index;
            let value = state.locals.get(self.ctx, slot)?;
            let binding = if value.missing {
                TypeBinding::Pending(index)
            } else {
                bindings.current(self.ctx, self.facts, value.value)?
            };
            bindings.insert(self.ctx, hosts, root.name.as_bytes().unwrap(), binding)?;
        }
        let sources = self.layouts.type_sources(self.ctx, self.function_index)?;
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
                return Ok(Contract::Value(None));
            };
            if owner == self.function_index {
                continue;
            }
            let Some(depth) = self.layouts.depth(self.ctx, self.function_index, owner)? else {
                self.incomplete(pc)?;
                return Ok(Contract::Value(None));
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
        for (_, scope) in levels.data {
            self.ctx.charge(1)?;
            scopes.push(self.ctx, scope)?;
        }
        let source = bindings.source(
            self.ctx,
            self.facts,
            self.program,
            self.layouts.source_owner,
        )?;
        for (index, (global, _)) in self.program.globals.iter().enumerate() {
            self.ctx.charge(1)?;
            let value = state.locals.get(self.ctx, state.global_base + index)?.value;
            let binding = bindings.current(self.ctx, self.facts, value)?;
            bindings.insert(self.ctx, source, global.name().as_bytes(), binding)?;
        }
        bindings.overlay(self.ctx, source, &[hosts])?;
        scopes.extend(self.ctx, &[hosts, source])?;
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
}
