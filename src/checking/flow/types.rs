use super::*;
use crate::checking::type_bindings::{Binding as TypeBinding, Bindings, Resolution, Scope};

impl Walker<'_> {
    pub(super) fn normalization_contract(
        &mut self,
        state: &State,
        pc: usize,
        ty: usize,
    ) -> Result<Option<Fact>> {
        if self.block_inputs.is_none() {
            return Ok(Some(self.contracts[ty]));
        }
        let mut bindings = Bindings::new();
        let hosts = bindings.scope(self.ctx)?;
        let replaced = self.calls.type_bindings(self.ctx, &mut bindings, hosts)?;
        let sources = self.layouts.type_sources(self.ctx, self.function_index)?;
        if sources.is_empty() && !replaced && !self.facts.unresolved(self.contracts[ty]) {
            return Ok(Some(self.contracts[ty]));
        }
        let mut levels: Buffer<(usize, Scope)> = Buffer::empty();
        for source in sources {
            self.ctx.charge(1)?;
            let name = &self.program.functions[source.function].local_names[source.name];
            let value = state.locals.get(self.ctx, source.capture)?;
            if value.value == Atom::Never.fact() && value.missing {
                continue;
            }
            let binding = Bindings::value(self.ctx, self.facts, value.value)?;
            if binding == TypeBinding::Other {
                continue;
            }
            let blocks::Owner::Function(owner) = value.owner else {
                self.incomplete(pc)?;
                return Ok(None);
            };
            if owner == self.function_index {
                continue;
            }
            let Some(depth) = self.layouts.depth(self.ctx, self.function_index, owner)? else {
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
        for (_, scope) in levels.data {
            self.ctx.charge(1)?;
            scopes.push(self.ctx, scope)?;
        }
        let source = bindings.source(self.ctx, self.facts, self.program, 0)?;
        for declaration in &self.program.declarations {
            self.ctx.charge(1)?;
            if let Kind::Namespace(namespace) = &declaration.0 {
                bindings.insert(
                    self.ctx,
                    source,
                    namespace.definition.name.as_bytes(),
                    crate::checking::type_bindings::Binding::Unknown,
                )?;
            }
        }
        bindings.overlay(self.ctx, source, &[hosts])?;
        scopes.extend(self.ctx, &[hosts, source])?;
        let mut failure = None;
        let mut dynamic = false;
        let fact = self
            .facts
            .annotation(self.ctx, &self.program.types[ty], |ctx, name| {
                let resolution = bindings.resolve(ctx, &scopes.data, name, false)?;
                match resolution {
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
        if dynamic {
            self.incomplete(pc)?;
            return Ok(None);
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
            return Ok(None);
        }
        Ok(Some(fact))
    }
}
