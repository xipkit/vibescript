use super::*;
use crate::checking::type_bindings::{Bindings, Resolution, Scope};

#[derive(Clone, Copy)]
pub(super) struct HostTarget<'a> {
    pub world: World<'a>,
    pub index: usize,
}

enum Contract {
    Known(Fact),
    Rejected,
    Pending(usize),
}

struct Environment {
    bindings: Bindings,
    scopes: [Scope; 4],
    count: usize,
}

impl Solver<'_, '_> {
    /// Resolves annotations without borrowing the caller's lexical type environment.
    pub(super) fn source_annotation(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        ty: usize,
        globals: &Globals,
    ) -> Result<Annotation> {
        let expected = self.world.contracts[ty];
        let mut result = Annotation {
            expected,
            resolution: Resolution::Known(expected),
            throws: 0,
        };
        if !self.layouts.named_annotation(ctx, ty)? {
            return Ok(result);
        }
        let environment = self.host_environment(ctx, facts, globals)?;
        let mut failure = None;
        let value = facts.annotation(ctx, &self.world.program.types[ty], |ctx, name| {
            if failure.is_some() {
                return Ok(None);
            }
            match environment.bindings.resolve(
                ctx,
                &environment.scopes[..environment.count],
                name,
                false,
            )? {
                Resolution::Known(value) => Ok(Some(value)),
                resolution => {
                    failure = Some(resolution);
                    Ok(None)
                }
            }
        })?;
        result.resolution = failure.unwrap_or(Resolution::Known(value));
        Ok(result)
    }

    fn host_environment(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
    ) -> Result<Environment> {
        let mut bindings = Bindings::new();
        let slots = globals.layout.source(ctx, self.source)?;
        let hosts = bindings.scope(ctx)?;
        self.type_bindings(ctx, &mut bindings, hosts)?;
        let roots = self.roots(ctx, facts)?;
        for (index, root) in roots.data.iter().enumerate() {
            ctx.charge(1)?;
            let slot = slots.roots.data[index];
            let binding = if globals.missing.data[slot] {
                crate::checking::type_bindings::Binding::Pending(index)
            } else {
                bindings.current(ctx, facts, globals.values.data[slot])?
            };
            bindings.insert(ctx, hosts, root.name.as_bytes().unwrap(), binding)?;
        }
        for (receiving, name, slot) in globals.layout.roots() {
            ctx.charge(1)?;
            if *receiving != slots.receiving || slots.root(ctx, *slot)?.is_some() {
                continue;
            }
            let value = globals.values.data[*slot];
            if value == Atom::Never.fact() {
                continue;
            }
            let binding = bindings.current(ctx, facts, value)?;
            if globals.missing.data[*slot] {
                bindings.optional(ctx, hosts, name.as_bytes().unwrap(), binding)?;
            } else {
                bindings.insert(ctx, hosts, name.as_bytes().unwrap(), binding)?;
            }
        }
        let source = bindings.source(ctx, facts, self.world.program, self.world.source_owner)?;
        if self.world.program.file {
            for index in 0..self.world.program.declarations.len() {
                let name =
                    crate::checking::file_bindings::declaration_name(self.world.program, index);
                let original = globals.values.data[slots.declarations.start + index];
                let value = self.file_type_value(ctx, facts, globals, name, original)?;
                let binding = bindings.current(ctx, facts, value)?;
                bindings.insert(ctx, source, name.as_bytes(), binding)?;
            }
        }
        for (index, (name, _)) in self.world.program.globals.iter().enumerate() {
            ctx.charge(1)?;
            let value = self.file_type_value(
                ctx,
                facts,
                globals,
                name.name(),
                globals.values.data[slots.globals.data[index]],
            )?;
            let binding = bindings.current(ctx, facts, value)?;
            bindings.insert(ctx, source, name.name().as_bytes(), binding)?;
        }
        bindings.overlay(ctx, source, &[hosts])?;
        let (scopes, count) = if self.world.program.file {
            let file = bindings.scope(ctx)?;
            let base = slots.files.start;
            for (index, name) in self.layouts.files.names.data.iter().enumerate() {
                ctx.charge(1)?;
                let value = globals.values.data[base + index];
                let missing = globals.missing.data[base + index];
                if value == Atom::Never.fact() && missing {
                    continue;
                }
                let binding = bindings.current(ctx, facts, value)?;
                if missing {
                    bindings.optional(ctx, file, name.as_bytes().unwrap(), binding)?;
                } else {
                    bindings.insert(ctx, file, name.as_bytes().unwrap(), binding)?;
                }
            }
            ([file, hosts, source, source], 3)
        } else {
            ([hosts, source, source, source], 2)
        };
        let (mut scopes, mut count) = (scopes, count);
        if let Some(receiving) = self.receiving_type_scope(ctx, facts, globals, &mut bindings)? {
            scopes[count] = receiving;
            count += 1;
        }
        Ok(Environment {
            bindings,
            scopes,
            count,
        })
    }

    fn file_type_value(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
        name: &str,
        original: Fact,
    ) -> Result<Fact> {
        let Some(index) = self.layouts.files.index(ctx, name)? else {
            return Ok(original);
        };
        let base = globals.layout.source(ctx, self.source)?.files.start;
        let value = globals.values.data[base + index];
        if globals.missing.data[base + index] {
            facts.union(ctx, &[value, original])
        } else {
            Ok(value)
        }
    }

    pub(super) fn host_call(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: HostTarget<'_>,
        args: &Arguments,
        globals: &Globals,
        outcome: &mut Outcome,
    ) -> Result<()> {
        self.host_arguments(ctx, facts, target, args, globals, outcome)?;
        if outcome.incomplete || outcome.value == Atom::Never.fact() {
            return Ok(());
        }
        outcome.value = Atom::Never.fact();
        if args.block.is_some() {
            outcome.incomplete = true;
            return Ok(());
        }
        self.host_result(ctx, facts, target, None, globals, outcome)
    }

    pub(super) fn host_arguments(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: HostTarget<'_>,
        args: &Arguments,
        globals: &Globals,
        outcome: &mut Outcome,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let Some(host) = self.state.values.host(ctx, &target.world, target.index)? else {
            outcome.incomplete = true;
            return Ok(());
        };
        if !host.granted {
            outcome.failures.push(ctx, Failure::HostGrant)?;
            return Ok(());
        }
        if args.block.is_some() && host.blocks == HostBlocks::Rejected {
            outcome.failures.push(ctx, Failure::HostBlockDriver)?;
            return Ok(());
        }
        // Opaque argument validators run before the declarative signature checks.
        outcome.throws = u8::MAX;
        if host.constrained {
            let failure = if args.positional.data.len() < host.required
                || args.positional.data.len() > host.params.data.len()
            {
                Some(Failure::HostArity)
            } else if !args.keywords.data.is_empty() {
                Some(Failure::HostKeywords)
            } else if args.block.is_some() && !host.accepts_block {
                Some(Failure::HostBlock)
            } else {
                None
            };
            if let Some(failure) = failure {
                outcome.failures.push(ctx, failure)?;
                return Ok(());
            }
        }
        if host.unresolved && host.source.is_none() {
            outcome.incomplete = true;
            return Ok(());
        }
        let params = host.params.data.len();
        for (parameter, &actual) in args.positional.data.iter().take(params).enumerate() {
            let expected = self
                .state
                .values
                .host(ctx, &target.world, target.index)?
                .unwrap()
                .params
                .data[parameter];
            ctx.charge(1)?;
            let Some(expected) = expected else { continue };
            let Some(expected) = self.host_contract(
                ctx,
                facts,
                target,
                globals,
                Some(parameter),
                expected,
                outcome,
            )?
            else {
                return Ok(());
            };
            if facts.relation(ctx, actual, expected)? == Relation::Rejected {
                outcome.failures.push(
                    ctx,
                    Failure::Type {
                        parameter,
                        actual,
                        expected,
                    },
                )?;
                if !facts.overlaps(ctx, actual, expected)? {
                    return Ok(());
                }
            }
        }
        outcome.value = Atom::Nil.fact();
        Ok(())
    }

    pub(super) fn host_result(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: HostTarget<'_>,
        actual: Option<Fact>,
        globals: &Globals,
        outcome: &mut Outcome,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let Some(host) = self.state.values.host(ctx, &target.world, target.index)? else {
            outcome.incomplete = true;
            return Ok(());
        };
        if host.unresolved && host.source.is_none() {
            outcome.incomplete = true;
            return Ok(());
        }
        let expected = host.result;
        // A sticky break supplies its own result; the callback cannot replace it
        // or raise a different error after the transfer.
        if actual.is_none() {
            outcome.throws = u8::MAX;
        }
        if let Some(result) =
            self.host_contract(ctx, facts, target, globals, None, expected, outcome)?
        {
            outcome.value = if let Some(actual) = actual {
                let relation = facts.relation(ctx, actual, result)?;
                if relation != Relation::Accepted {
                    outcome.throws |= 1 << crate::ErrorClass::Runtime as u8;
                }
                if relation == Relation::Rejected {
                    outcome.failures.push(
                        ctx,
                        Failure::HostResult {
                            actual,
                            expected: result,
                        },
                    )?;
                }
                if result == Atom::Unknown.fact() {
                    actual
                } else {
                    facts.normalized(ctx, actual, result)?
                }
            } else {
                facts.value_domain(ctx, result)?
            };
            if outcome.value != Atom::Never.fact() {
                // Custom result validators are opaque and may reject a valid value.
                outcome.throws = u8::MAX;
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn host_contract(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        target: HostTarget<'_>,
        globals: &Globals,
        parameter: Option<usize>,
        expected: Fact,
        outcome: &mut Outcome,
    ) -> Result<Option<Fact>> {
        let environment = if facts.unresolved(expected) {
            Some(self.host_environment(ctx, facts, globals)?)
        } else {
            None
        };
        let host = self
            .state
            .values
            .host(ctx, &target.world, target.index)?
            .unwrap();
        match host.contract(
            ctx,
            facts,
            environment.as_ref(),
            parameter,
            expected,
            outcome,
        )? {
            Contract::Known(fact) => Ok(Some(fact)),
            Contract::Rejected => Ok(None),
            Contract::Pending(root) => {
                outcome.pending = Some(root);
                Ok(None)
            }
        }
    }
}

impl Host {
    fn contract(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        environment: Option<&Environment>,
        parameter: Option<usize>,
        expected: Fact,
        outcome: &mut Outcome,
    ) -> Result<Contract> {
        ctx.charge(1)?;
        if !facts.unresolved(expected) {
            return Ok(Contract::Known(expected));
        }
        let signature = self.source.as_ref().unwrap();
        let ty = parameter.map_or(signature.result.as_ref(), |index| {
            signature.params[index].as_ref()
        });
        let environment = environment.unwrap();
        let mut failure = None;
        let resolved = facts.annotation(ctx, ty.unwrap(), |ctx, name| {
            if failure.is_some() {
                return Ok(None);
            }
            match environment.bindings.resolve(
                ctx,
                &environment.scopes[..environment.count],
                name,
                false,
            )? {
                Resolution::Known(fact) => Ok(Some(fact)),
                resolution => {
                    failure = Some(resolution);
                    Ok(None)
                }
            }
        })?;
        match failure {
            None => Ok(Contract::Known(resolved)),
            Some(Resolution::Pending(index)) => Ok(Contract::Pending(index)),
            Some(Resolution::Dynamic) => {
                outcome.incomplete = true;
                Ok(Contract::Rejected)
            }
            Some(resolution) => {
                outcome.failures.push(
                    ctx,
                    Failure::HostTypeBinding {
                        parameter,
                        expected,
                        ambiguous: resolution == Resolution::Ambiguous,
                    },
                )?;
                Ok(Contract::Rejected)
            }
        }
    }
}
