use super::*;
use crate::checking::type_bindings::{Bindings, Resolution, Scope};

struct Environment {
    bindings: Bindings,
    scopes: [Scope; 2],
}

impl Solver<'_> {
    fn host_environment(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        globals: &Globals,
    ) -> Result<Environment> {
        let mut bindings = Bindings::new();
        let hosts = bindings.scope(ctx)?;
        self.type_bindings(ctx, &mut bindings, hosts)?;
        let roots = self.roots(ctx, facts)?;
        for (index, root) in roots.data.iter().enumerate() {
            ctx.charge(1)?;
            let value = globals.values.data[self.world.program.globals.len() + index];
            let binding = bindings.current(ctx, facts, value)?;
            bindings.insert(ctx, hosts, root.name.as_bytes().unwrap(), binding)?;
        }
        let source =
            bindings.current_source(ctx, facts, self.world.program, self.world.source_owner)?;
        for (index, (name, _)) in self.world.program.globals.iter().enumerate() {
            ctx.charge(1)?;
            let binding = bindings.current(ctx, facts, globals.values.data[index])?;
            bindings.insert(ctx, source, name.name().as_bytes(), binding)?;
        }
        bindings.overlay(ctx, source, &[hosts])?;
        Ok(Environment {
            bindings,
            scopes: [hosts, source],
        })
    }

    pub(super) fn host_call(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        args: &Arguments,
        globals: &Globals,
        outcome: &mut Outcome,
    ) -> Result<()> {
        self.host_arguments(ctx, facts, index, args, globals, outcome)?;
        if outcome.incomplete || outcome.value == Atom::Never.fact() {
            return Ok(());
        }
        outcome.value = Atom::Never.fact();
        if args.block.is_some() {
            outcome.incomplete = true;
            return Ok(());
        }
        self.host_result(ctx, facts, index, None, globals, outcome)
    }

    pub(super) fn host_arguments(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        args: &Arguments,
        globals: &Globals,
        outcome: &mut Outcome,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let Some(host) = self.world.hosts.get(index) else {
            outcome.incomplete = true;
            return Ok(());
        };
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
        let environment = if host.unresolved {
            Some(self.host_environment(ctx, facts, globals)?)
        } else {
            None
        };
        let host = &self.world.hosts[index];
        for (parameter, (&actual, &expected)) in args
            .positional
            .data
            .iter()
            .zip(&host.params.data)
            .enumerate()
        {
            ctx.charge(1)?;
            let Some(expected) = expected else { continue };
            let Some(expected) = host.contract(
                ctx,
                facts,
                environment.as_ref(),
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
        index: usize,
        actual: Option<Fact>,
        globals: &Globals,
        outcome: &mut Outcome,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let Some(host) = self.world.hosts.get(index) else {
            outcome.incomplete = true;
            return Ok(());
        };
        if host.unresolved && host.source.is_none() {
            outcome.incomplete = true;
            return Ok(());
        }
        let environment = if facts.unresolved(host.result) {
            Some(self.host_environment(ctx, facts, globals)?)
        } else {
            None
        };
        let host = &self.world.hosts[index];
        // A sticky break supplies its own result; the callback cannot replace it
        // or raise a different error after the transfer.
        if actual.is_none() {
            outcome.throws = u8::MAX;
        }
        if let Some(result) =
            host.contract(ctx, facts, environment.as_ref(), None, host.result, outcome)?
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
}

impl Host<'_> {
    fn contract(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        environment: Option<&Environment>,
        parameter: Option<usize>,
        expected: Fact,
        outcome: &mut Outcome,
    ) -> Result<Option<Fact>> {
        ctx.charge(1)?;
        if !facts.unresolved(expected) {
            return Ok(Some(expected));
        }
        let signature = self.source.unwrap();
        let ty = parameter.map_or(signature.result.as_ref(), |index| {
            signature.params[index].as_ref()
        });
        let environment = environment.unwrap();
        let mut failure = None;
        let resolved = facts.annotation(ctx, ty.unwrap(), |ctx, name| {
            if failure.is_some() {
                return Ok(None);
            }
            match environment
                .bindings
                .resolve(ctx, &environment.scopes, name, false)?
            {
                Resolution::Known(fact) => Ok(Some(fact)),
                resolution => {
                    failure = Some(resolution);
                    Ok(None)
                }
            }
        })?;
        match failure {
            None => Ok(Some(resolved)),
            Some(Resolution::Dynamic) => {
                outcome.incomplete = true;
                Ok(None)
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
                Ok(None)
            }
        }
    }
}
