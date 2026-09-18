use super::{
    arguments::Arguments,
    calls::{self, Analysis, LocatedIssue, Target},
    environment::{Environment, Incomplete},
    facts::{Atom, Fact, Facts},
    flow::{Issue, IssueKind},
    inputs::Values,
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

/// Owns the facts referenced by an internal exact-call analysis.
pub(super) struct Check {
    pub facts: Facts,
    pub analysis: Analysis,
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
    for reason in &environment.incomplete.data {
        ctx.charge(1)?;
        if matches!(reason, Incomplete::Capability(_) | Incomplete::File) {
            return unfinished(ctx, facts, function);
        }
    }
    let world = environment.world();
    let mut values = Values::new();
    let mut args = Arguments::new();
    for value in call.arguments {
        let value = values.argument(ctx, &mut facts, &world, value)?;
        if value.incomplete {
            return unfinished(ctx, facts, function);
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
            return unfinished(ctx, facts, function);
        }
        if facts.escapes(value.value) {
            return detached(ctx, facts, function, value.value);
        }
        args.keyword(ctx, name, value.value)?;
    }
    if !environment.incomplete.data.is_empty() {
        return unfinished(ctx, facts, function);
    }
    let bound = args.bind_host(ctx, &mut facts, &program.functions[function].params)?;
    if !bound.failures.data.is_empty() {
        let mut analysis = empty(Atom::Never.fact(), 1 << ErrorClass::Argument as u8);
        for &failure in &bound.failures.data {
            ctx.charge(1)?;
            analysis.issues.push(
                ctx,
                LocatedIssue {
                    function,
                    issue: Issue {
                        pc: 0,
                        kind: IssueKind::Call {
                            target: Target::Function(function),
                            failure,
                        },
                    },
                },
            )?;
        }
        return Ok(Check { facts, analysis });
    }
    let analysis =
        calls::analyze_with_values(ctx, &mut facts, world, function, &bound.inputs.data, values)?;
    Ok(Check { facts, analysis })
}

fn empty(returns: Fact, throws: u8) -> Analysis {
    Analysis {
        returns,
        throws,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        contexts: 0,
    }
}

fn unfinished(ctx: &mut CallContext, facts: Facts, function: usize) -> Result<Check> {
    let mut analysis = empty(Atom::Unknown.fact(), u8::MAX);
    analysis.incomplete.push(ctx, (function, 0))?;
    Ok(Check { facts, analysis })
}

fn detached(ctx: &mut CallContext, facts: Facts, function: usize, value: Fact) -> Result<Check> {
    let mut analysis = empty(Atom::Never.fact(), 1 << ErrorClass::Runtime as u8);
    analysis.issues.push(
        ctx,
        LocatedIssue {
            function,
            issue: Issue {
                pc: 0,
                kind: IssueKind::DetachedValue(Target::Value(value)),
            },
        },
    )?;
    Ok(Check { facts, analysis })
}
