use super::super::{
    arguments::Failure,
    calls::{LocatedIssue, Target},
    facts::Facts,
    flow::IssueKind,
};
use super::types::Writer;
use crate::{CallContext, Result, budget::Charge, bytecode::Program};

pub(super) fn issue(
    ctx: &mut CallContext,
    program: &Program,
    facts: &Facts,
    issue: &LocatedIssue,
) -> Result<(String, Option<Charge>)> {
    let mut out = Writer::new(ctx);
    match issue.issue.kind {
        IssueKind::DetachedValue(_) => out.text("Attached methods cannot be used as values")?,
        IssueKind::TypeBinding { ty, ambiguous } => {
            out.text(if ambiguous {
                "Ambiguous type in "
            } else {
                "Unknown type in "
            })?;
            crate::shapes::format(&program.types[ty], &mut out)?;
        }
        IssueKind::MissingBlock => out.text("No block was supplied for yield")?,
        IssueKind::BlockGivenArguments => out.text("block_given? does not accept arguments")?,
        IssueKind::Ordering { name, left, right } => {
            member(&mut out, program, name)?;
            out.text(" cannot compare ")?;
            out.fact(facts, left)?;
            out.text(" with ")?;
            out.fact(facts, right)?;
        }
        IssueKind::CallbackResult {
            name,
            actual,
            expected,
        } => {
            member(&mut out, program, name)?;
            out.text(" block result: ")?;
            out.mismatch(facts, actual, expected)?;
        }
        IssueKind::Index {
            receiver,
            arguments,
        } => {
            out.text("Cannot index ")?;
            out.fact(facts, receiver)?;
            out.text(" with ")?;
            out.fact(facts, arguments)?;
        }
        IssueKind::Write {
            receiver,
            selectors,
            value,
        } => {
            out.text("Cannot write ")?;
            out.fact(facts, value)?;
            out.text(" into ")?;
            out.fact(facts, receiver)?;
            out.text(" at ")?;
            out.fact(facts, selectors)?;
        }
        IssueKind::Member {
            name,
            receiver,
            arguments,
        } => {
            member(&mut out, program, name)?;
            out.text(" does not accept receiver ")?;
            out.fact(facts, receiver)?;
            out.text(" with arguments ")?;
            out.fact(facts, arguments)?;
        }
        IssueKind::Range { value } => {
            out.text("Range endpoint must be int; got ")?;
            out.fact(facts, value)?;
        }
        IssueKind::Iterate { value } => {
            out.text("Cannot iterate over ")?;
            out.fact(facts, value)?;
        }
        IssueKind::CaseSplat { value } => {
            out.text("Case splat must be an array; got ")?;
            out.fact(facts, value)?;
        }
        IssueKind::Regex { .. } => out.text("Invalid regular expression")?,
        IssueKind::Raise { value } => {
            out.text("Cannot raise ")?;
            out.fact(facts, value)?;
        }
        IssueKind::Call { target, failure } => call(&mut out, program, facts, target, failure)?,
        IssueKind::Splat { actual, keyword } => {
            out.text(if keyword {
                "Keyword splat must be a hash; got "
            } else {
                "Positional splat must be an array; got "
            })?;
            out.fact(facts, actual)?;
        }
        IssueKind::Return { actual, expected } => {
            out.text("Return value: ")?;
            out.mismatch(facts, actual, expected)?;
        }
        IssueKind::Property { actual, expected } => {
            out.text("Property value ")?;
            out.fact(facts, actual)?;
            out.text(" does not match ")?;
            out.fact(facts, expected)?;
        }
        IssueKind::Default { actual, expected } => {
            out.text("Default argument: ")?;
            out.mismatch(facts, actual, expected)?;
        }
        IssueKind::Reassignment {
            slot,
            before,
            after,
        } => {
            out.text("Reassignment of ")?;
            out.quoted(
                program.functions[issue.function]
                    .local_names
                    .get(slot)
                    .map_or(b"binding", String::as_bytes),
            )?;
            out.text(" changes ")?;
            out.fact(facts, before)?;
            out.text(" to ")?;
            out.fact(facts, after)?;
        }
        IssueKind::Unary { op, value } => {
            out.text("Operator ")?;
            out.quoted(op.as_bytes())?;
            out.text(" does not accept ")?;
            out.fact(facts, value)?;
        }
        IssueKind::Operator { name, receiver } => {
            out.text("Operator ")?;
            out.quoted(name.as_bytes())?;
            out.text(" is unavailable on ")?;
            out.fact(facts, receiver)?;
        }
        IssueKind::Binary { op, left, right } => {
            out.text("Operator ")?;
            out.quoted(op.as_bytes())?;
            out.text(" does not accept ")?;
            out.fact(facts, left)?;
            out.text(" and ")?;
            out.fact(facts, right)?;
        }
    }
    Ok(out.finish())
}

fn member(out: &mut Writer<'_>, program: &Program, name: usize) -> Result<()> {
    out.quoted(
        program
            .members
            .get(name)
            .map_or(b"method", String::as_bytes),
    )
}

fn target(out: &mut Writer<'_>, program: &Program, target: Target) -> Result<()> {
    match target {
        Target::Function(index)
        | Target::Block(index)
        | Target::Method {
            function: index, ..
        } => out.quoted(program.functions[index].trace_name.as_bytes()),
        Target::Host(index) => match program.hosts.get(index) {
            Some(name) => out.quoted(name.as_bytes()),
            None => out.text("host method"),
        },
        Target::Builtin(builtin) => out.quoted(builtin.name().as_bytes()),
        Target::Helper { name, .. } => out.quoted(name.as_bytes()),
        _ => out.text("call"),
    }
}

fn parameter(out: &mut Writer<'_>, program: &Program, target: Target, index: usize) -> Result<()> {
    out.text("argument ")?;
    if let Target::Function(function) | Target::Block(function) | Target::Method { function, .. } =
        target
    {
        if let Some(parameter) = program.functions[function].params.get(index) {
            return out.quoted(parameter.name.as_bytes());
        }
    }
    out.number(index.saturating_add(1))
}

fn call(
    out: &mut Writer<'_>,
    program: &Program,
    facts: &Facts,
    selected: Target,
    failure: Failure,
) -> Result<()> {
    target(out, program, selected)?;
    out.text(": ")?;
    match failure {
        Failure::NonCallable => out.text("value is not callable"),
        Failure::Undefined => out.text("undefined callable"),
        Failure::HostArity | Failure::BuiltinArity => out.text("wrong number of arguments"),
        Failure::HostKeywords | Failure::BuiltinKeywords => {
            out.text("keyword arguments are not accepted")
        }
        Failure::HostBlock | Failure::BuiltinBlock => out.text("a block is not accepted"),
        Failure::HostBlockDriver => out.text("callback cannot invoke a block"),
        Failure::HostGrant => out.text("capability grant belongs to an earlier invocation"),
        Failure::HostResult { actual, expected } => {
            out.text("result ")?;
            out.mismatch(facts, actual, expected)
        }
        Failure::HostTypeBinding {
            parameter: index,
            expected,
            ambiguous,
        } => {
            if let Some(index) = index {
                parameter(out, program, selected, index)?;
            } else {
                out.text("result")?;
            }
            out.text(if ambiguous {
                " has an ambiguous type: "
            } else {
                " has an unknown type: "
            })?;
            out.fact(facts, expected)
        }
        Failure::BuiltinKeyword(name) | Failure::ExtraKeyword(name) => {
            out.text("unexpected keyword ")?;
            match facts.node(name) {
                super::super::facts::Node::String(value)
                | super::super::facts::Node::Symbol(value) => out.quoted(value.as_bytes().unwrap()),
                _ => out.fact(facts, name),
            }
        }
        Failure::BuiltinKeywordType {
            name,
            actual,
            expected,
        } => {
            out.text("keyword ")?;
            match facts.node(name) {
                super::super::facts::Node::String(value)
                | super::super::facts::Node::Symbol(value) => {
                    out.quoted(value.as_bytes().unwrap())?
                }
                _ => out.fact(facts, name)?,
            }
            out.text(": ")?;
            out.mismatch(facts, actual, expected)
        }
        Failure::BuiltinValue => out.text("invalid argument value"),
        Failure::DetachedValue(_) => out.text("attached methods cannot be used as values"),
        Failure::TypeLiteral(actual) => {
            out.text("expected a type expression, got ")?;
            out.fact(facts, actual)
        }
        Failure::BuiltinDomain(actual) => {
            out.text("argument is outside the supported domain: ")?;
            out.fact(facts, actual)
        }
        Failure::JsonValue(actual) => {
            out.text("cannot encode ")?;
            out.fact(facts, actual)?;
            out.text(" as JSON")
        }
        Failure::Missing(index) => {
            out.text("missing ")?;
            parameter(out, program, selected, index)
        }
        Failure::ExtraPositionals => out.text("too many positional arguments"),
        Failure::Type {
            parameter: index,
            actual,
            expected,
        } => {
            parameter(out, program, selected, index)?;
            out.text(": ")?;
            out.mismatch(facts, actual, expected)
        }
    }
}
