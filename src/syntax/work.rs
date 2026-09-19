use super::{Argument, ArgumentKind, Block, Definition, Expr, Node, Statement, Stmt, Target};
use crate::{Result, compilation::Work};

// Alias declarations copy syntax before bytecode generation can charge its walks.
pub(super) fn definition(work: &dyn Work, definition: &Definition) -> Result<()> {
    work.bytes(definition.name.len())?;
    if let Some((name, _)) = &definition.accessor {
        work.bytes(name.len())?;
    }
    for parameter in &definition.params {
        work.bytes(parameter.name.len())?;
        if let Some(name) = &parameter.ivar {
            work.bytes(name.len())?;
        }
        if let Some(ty) = &parameter.ty {
            work.ty(ty)?;
        }
        if let Some(default) = &parameter.default {
            expression(work, default)?;
        }
    }
    if let Some(ty) = &definition.return_type {
        work.ty(ty)?;
    }
    body(work, &definition.body)
}

fn body(work: &dyn Work, body: &[Stmt]) -> Result<()> {
    for stmt in body {
        statement(work, stmt)?;
    }
    Ok(())
}

fn statement(work: &dyn Work, stmt: &Stmt) -> Result<()> {
    work.charge(1)?;
    match &stmt.node {
        Statement::Retry => (),
        Statement::Module(name) | Statement::UnboundClass(name) => work.bytes(name.len())?,
        Statement::Raise(value, message) => {
            for value in value.iter().chain(message) {
                expression(work, value)?;
            }
        }
        Statement::Expr(value) => expression(work, value)?,
        Statement::Assign(place, _, value) => {
            target(work, place)?;
            expression(work, value)?;
        }
        Statement::If(condition, yes, no) => {
            expression(work, condition)?;
            body(work, yes)?;
            body(work, no)?;
        }
        Statement::While(condition, statements) => {
            expression(work, condition)?;
            body(work, statements)?;
        }
        Statement::For(place, source, statements) => {
            target(work, place)?;
            expression(work, source)?;
            body(work, statements)?;
        }
        Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
            if let Some(value) = value {
                expression(work, value)?;
            }
        }
    }
    Ok(())
}

fn target(work: &dyn Work, place: &Target) -> Result<()> {
    work.charge(1)?;
    match place {
        Target::Value(value) => expression(work, value)?,
        Target::Typed(place, ty) => {
            work.ty(ty)?;
            target(work, place)?;
        }
        Target::Tuple(parts) => {
            for (part, _) in parts {
                work.charge(1)?;
                if let Some(part) = part {
                    target(work, part)?;
                }
            }
        }
    }
    Ok(())
}

fn arguments(work: &dyn Work, args: &[Argument]) -> Result<()> {
    for arg in args {
        if let ArgumentKind::Keyword(name) = &arg.kind {
            work.bytes(name.len())?;
        }
        expression(work, &arg.value)?;
    }
    Ok(())
}

fn block(work: &dyn Work, block: &Block) -> Result<()> {
    for param in &block.params {
        target(work, param)?;
    }
    body(work, &block.body)
}

fn expression(work: &dyn Work, value: &Expr) -> Result<()> {
    work.charge(1)?;
    match &value.node {
        Node::Integer(_) | Node::Literal(_) => (),
        Node::Regex(bytes, _) => work.bytes(bytes.len())?,
        Node::BigInteger(text, _) => work.bytes(text.len())?,
        Node::Var(text) => work.bytes(text.len())?,
        Node::Shape(ty, fallback, names) => {
            work.ty(ty)?;
            work.names(names)?;
            if let Some(fallback) = fallback {
                expression(work, fallback)?;
            }
        }
        Node::Try(attempt) => {
            body(work, &attempt.body)?;
            for rescue in &attempt.rescues {
                work.charge(rescue.classes.len())?;
                if let Some(name) = &rescue.binding {
                    work.bytes(name.len())?;
                }
                body(work, &rescue.body)?;
            }
            body(work, &attempt.alternate)?;
            body(work, &attempt.ensure)?;
        }
        Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
            for value in values {
                expression(work, value)?;
            }
        }
        Node::Hash(entries) => {
            for (key, value) in entries {
                work.bytes(key.len())?;
                expression(work, value)?;
            }
        }
        Node::Unary(_, value) => expression(work, value)?,
        Node::Binary(_, left, right) => {
            expression(work, left)?;
            expression(work, right)?;
        }
        Node::Range(first, last, _) => {
            for value in first.iter().chain(last) {
                expression(work, value)?;
            }
        }
        Node::Conditional(condition, yes, no) => {
            expression(work, condition)?;
            expression(work, yes)?;
            expression(work, no)?;
        }
        Node::Case(value, clauses, alternate) => {
            if let Some(value) = value {
                expression(work, value)?;
            }
            for clause in clauses {
                for (value, _) in &clause.values {
                    expression(work, value)?;
                }
                expression(work, &clause.result)?;
            }
            if let Some(value) = alternate {
                expression(work, value)?;
            }
        }
        Node::Loop(stmt) => statement(work, stmt)?,
        Node::Call(name, args, _) => {
            work.bytes(name.len())?;
            arguments(work, args)?;
        }
        Node::ComputedCall(receiver, args) => {
            expression(work, receiver)?;
            arguments(work, args)?;
        }
        Node::BlockCall(receiver, body) => {
            expression(work, receiver)?;
            block(work, body)?;
        }
        Node::Member(receiver, name) | Node::SafeMember(receiver, name) => {
            expression(work, receiver)?;
            work.bytes(name.len())?;
        }
        Node::Scope(receiver, name, args) => {
            expression(work, receiver)?;
            work.bytes(name.len())?;
            if let Some(args) = args {
                arguments(work, args)?;
            }
        }
        Node::Method(receiver, name, args, _) | Node::SafeMethod(receiver, name, args, _) => {
            expression(work, receiver)?;
            work.bytes(name.len())?;
            arguments(work, args)?;
        }
        Node::Index(receiver, indices) => {
            expression(work, receiver)?;
            for index in indices {
                expression(work, index)?;
            }
        }
    }
    Ok(())
}
