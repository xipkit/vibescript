use super::{
    arguments::Arguments,
    calls::{self, Analysis, LocatedIssue, Location, Target},
    environment::{Environment, Incomplete},
    facts::{Atom, Fact, Facts},
    flow::{Issue, IssueKind},
    inputs::Values,
    sources::SourceId,
};
use crate::{
    CallContext, CallOptions, Error, ErrorClass, ErrorKind, Result, Script, Value, budget::Buffer,
};

pub(super) struct Call<'a> {
    pub script: &'a Script,
    pub name: &'a str,
    pub arguments: &'a [Value],
    pub keywords: &'a [(String, Value)],
    pub options: &'a CallOptions,
}

/// Owns the facts referenced by one internal checking scope.
pub(super) struct Check {
    pub facts: Facts,
    pub analysis: Analysis,
    pub entry: bool,
    pub pending: Option<Pending>,
}

pub(super) enum Pending {
    Message(&'static str),
    Capability(Value),
}

impl From<&'static str> for Pending {
    fn from(message: &'static str) -> Self {
        Self::Message(message)
    }
}

impl std::fmt::Debug for Check {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Check")
            .field("analysis", &self.analysis)
            .finish_non_exhaustive()
    }
}

/// Checks one host invocation without running defaults, initializers or host code.
pub(super) fn check(ctx: &mut CallContext, call: Call<'_>) -> Result<Check> {
    ctx.checkpoint()?;
    ctx.work_bytes(call.name.len())?;
    let program = &call.script.inner.code.program;
    let function = *program
        .names
        .get(call.name)
        .ok_or_else(|| Error::new(ErrorKind::Name, format!("unknown function {}", call.name)))?;
    let mut facts = Facts::new(ctx)?;
    let environment = Environment::new(ctx, &mut facts, call.script, call.options)?;
    if let Some(reason) = environment.incomplete.data.first() {
        ctx.charge(1)?;
        let message = match reason {
            Incomplete::Capability(name) => Pending::Capability(name.clone()),
        };
        return unfinished(ctx, facts, function, message);
    }
    let world = environment.world();
    let mut values = Values::new();
    values.writers = Some([
        call.script.inner.output_writer.is_some(),
        call.script.inner.error_writer.is_some(),
    ]);
    let mut args = Arguments::new();
    for value in call.arguments {
        let value = values.argument(ctx, &mut facts, &world, value)?;
        if value.incomplete {
            return unfinished(
                ctx,
                facts,
                function,
                "Analysis of this argument is not implemented",
            );
        }
        if facts.escapes(value.value) {
            return detached(ctx, facts, function, value.value);
        }
        args.positional.push(ctx, value.value)?;
    }
    for (name, value) in call.keywords {
        let name = facts.symbol(ctx, name.as_bytes())?;
        let value = values.argument(ctx, &mut facts, &world, value)?;
        if value.incomplete {
            return unfinished(
                ctx,
                facts,
                function,
                "Analysis of this keyword argument is not implemented",
            );
        }
        if facts.escapes(value.value) {
            return detached(ctx, facts, function, value.value);
        }
        args.keyword(ctx, name, value.value)?;
    }
    let bound = args.bind_host(ctx, &mut facts, &program.functions[function].params)?;
    ctx.charge(program.namespaces.len() as u64)?;
    let initializers = (function != 0 || program.file)
        && program
            .namespaces
            .iter()
            .any(|namespace| namespace.body.is_some());
    if !initializers && !bound.failures.data.is_empty() {
        let mut analysis = empty(Atom::Never.fact(), 1 << ErrorClass::Argument as u8);
        for &failure in &bound.failures.data {
            ctx.charge(1)?;
            analysis.issues.push(
                ctx,
                LocatedIssue {
                    source: SourceId::ROOT,
                    function,
                    issue: Issue {
                        pc: 0,
                        kind: IssueKind::Call {
                            target: Target::Function(SourceId::ROOT.callable(function)),
                            failure,
                        },
                    },
                },
            )?;
        }
        return Ok(Check {
            facts,
            analysis,
            entry: true,
            pending: None,
        });
    }
    let analysis = calls::analyze_with_values(
        ctx,
        &mut facts,
        world,
        function,
        &bound.inputs.data,
        values,
        &bound.failures.data,
    )?;
    Ok(Check {
        facts,
        analysis,
        entry: false,
        pending: None,
    })
}

fn empty(_returns: Fact, _throws: u8) -> Analysis {
    Analysis {
        #[cfg(test)]
        returns: _returns,
        #[cfg(test)]
        throws: _throws,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        #[cfg(test)]
        contexts: 0,
    }
}

pub(super) fn unfinished(
    ctx: &mut CallContext,
    facts: Facts,
    function: usize,
    message: impl Into<Pending>,
) -> Result<Check> {
    let mut analysis = empty(Atom::Unknown.fact(), u8::MAX);
    analysis.incomplete.push(
        ctx,
        Location {
            source: SourceId::ROOT,
            function,
            pc: 0,
        },
    )?;
    Ok(Check {
        facts,
        analysis,
        entry: true,
        pending: Some(message.into()),
    })
}

fn detached(ctx: &mut CallContext, facts: Facts, function: usize, value: Fact) -> Result<Check> {
    let mut analysis = empty(Atom::Never.fact(), 1 << ErrorClass::Runtime as u8);
    analysis.issues.push(
        ctx,
        LocatedIssue {
            source: SourceId::ROOT,
            function,
            issue: Issue {
                pc: 0,
                kind: IssueKind::DetachedValue(Target::Value(value)),
            },
        },
    )?;
    Ok(Check {
        facts,
        analysis,
        entry: true,
        pending: None,
    })
}

/// Checks a declared function for its accepted parameter domains and optional defaults.
pub(super) fn check_function(
    ctx: &mut CallContext,
    script: &Script,
    name: &str,
    options: &CallOptions,
) -> Result<Check> {
    ctx.checkpoint()?;
    ctx.work_bytes(name.len())?;
    let program = &script.inner.code.program;
    let (function, constructor) = declaration(ctx, program, name)?;
    check_declaration(
        ctx,
        script,
        calls::General {
            function,
            constructor,
            scope: super::blocks::Scope::Invocation,
        },
        options,
    )
}

pub(super) fn check_declaration(
    ctx: &mut CallContext,
    script: &Script,
    selected: calls::General,
    options: &CallOptions,
) -> Result<Check> {
    let function = selected.function;
    let mut facts = Facts::new(ctx)?;
    let environment = Environment::new(ctx, &mut facts, script, options)?;
    if let Some(reason) = environment.incomplete.data.first() {
        ctx.charge(1)?;
        let message = match reason {
            Incomplete::Capability(name) => Pending::Capability(name.clone()),
        };
        return unfinished(ctx, facts, function, message);
    }
    let mut values = Values::new();
    values.writers = Some([
        script.inner.output_writer.is_some(),
        script.inner.error_writer.is_some(),
    ]);
    let analysis = calls::analyze_general(ctx, &mut facts, environment.world(), selected, values)?;
    Ok(Check {
        facts,
        analysis,
        entry: false,
        pending: None,
    })
}

fn declaration(
    ctx: &mut CallContext,
    program: &crate::bytecode::Program,
    name: &str,
) -> Result<(usize, bool)> {
    if let Some(&function) = program.names.get(name) {
        return Ok((function, false));
    }
    for namespace in &program.namespaces {
        ctx.work_bytes(name.len().max(namespace.name.len()))?;
        let Some(member) = name.strip_prefix(&namespace.name) else {
            continue;
        };
        if member == ".new" {
            if let Some((function, _)) = namespace.constructor {
                return Ok((function, true));
            }
        }
        let (methods, member) = if let Some(member) = member.strip_prefix('.') {
            (&namespace.methods, member)
        } else if let Some(member) = member.strip_prefix('#') {
            (&namespace.instance_methods, member)
        } else {
            continue;
        };
        for method in methods {
            ctx.work_bytes(member.len().max(method.name.len()))?;
            if method.name == member {
                return Ok((method.function, false));
            }
        }
    }
    Err(Error::new(
        ErrorKind::Name,
        format!("unknown function {name}"),
    ))
}
