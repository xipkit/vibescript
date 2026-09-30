//! Drops syntax trees without native recursion.
//!
//! Trees nest as deeply as the syntax limit. The compiler's generated drop
//! glue recurses through several frames per level, which can exhaust a small
//! native stack such as a WebAssembly host's. Dropping an expression or a
//! statement instead detaches its children onto a heap stack and drops them
//! one at a time, so each nested drop finds an empty node. A node's list of
//! children, such as an array literal's elements or a body's statements,
//! goes on the stack as one iterator that yields them in turn, so the stack
//! grows with the tree's depth and never holds a copy of a wide list.

use super::{Block, Expr, Node, Statement, Stmt, Target, Try};

enum Part {
    Expr(Expr),
    Stmt(Stmt),
    Target(Target),
    Exprs(Box<dyn Iterator<Item = Expr>>),
    Stmts(Box<dyn Iterator<Item = Stmt>>),
    Targets(Box<dyn Iterator<Item = Target>>),
}

/// Puts `exprs` on the stack, to be dropped in turn.
fn exprs(pending: &mut Vec<Part>, exprs: impl Iterator<Item = Expr> + 'static) {
    pending.push(Part::Exprs(Box::new(exprs)));
}

/// Puts `stmts` on the stack, to be dropped in turn.
fn stmts(pending: &mut Vec<Part>, stmts: impl Iterator<Item = Stmt> + 'static) {
    pending.push(Part::Stmts(Box::new(stmts)));
}

impl Drop for Expr {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        node(
            std::mem::replace(&mut self.node, Node::Integer(0)),
            &mut pending,
        );
        drain(pending);
    }
}

impl Drop for Stmt {
    fn drop(&mut self) {
        let mut pending = Vec::new();
        statement(
            std::mem::replace(&mut self.node, Statement::Retry),
            &mut pending,
        );
        drain(pending);
    }
}

fn drain(mut pending: Vec<Part>) {
    while let Some(part) = pending.pop() {
        match part {
            Part::Expr(mut expr) => node(
                std::mem::replace(&mut expr.node, Node::Integer(0)),
                &mut pending,
            ),
            Part::Stmt(mut stmt) => statement(
                std::mem::replace(&mut stmt.node, Statement::Retry),
                &mut pending,
            ),
            Part::Target(target) => match target {
                Target::Value(expr) => pending.push(Part::Expr(expr)),
                Target::Tuple(parts) => pending.push(Part::Targets(Box::new(
                    parts.into_iter().filter_map(|(target, _)| target),
                ))),
                Target::Typed(target, _) => pending.push(Part::Target(target.into_inner())),
            },
            Part::Exprs(mut rest) => {
                if let Some(expr) = rest.next() {
                    pending.push(Part::Exprs(rest));
                    pending.push(Part::Expr(expr));
                }
            }
            Part::Stmts(mut rest) => {
                if let Some(stmt) = rest.next() {
                    pending.push(Part::Stmts(rest));
                    pending.push(Part::Stmt(stmt));
                }
            }
            Part::Targets(mut rest) => {
                if let Some(target) = rest.next() {
                    pending.push(Part::Targets(rest));
                    pending.push(Part::Target(target));
                }
            }
        }
    }
}

fn node(node: Node, pending: &mut Vec<Part>) {
    let mut expr = |expr: Expr| pending.push(Part::Expr(expr));
    match node {
        Node::Regex(..)
        | Node::Integer(_)
        | Node::BigInteger(..)
        | Node::Literal(_)
        | Node::Var(_) => {}
        Node::Try(attempt) => attempted(attempt.into_inner(), pending),
        Node::Shape(_, value, _) => value.into_iter().for_each(|e| expr(e.into_inner())),
        Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
            exprs(pending, values.into_iter());
        }
        Node::Hash(pairs) => exprs(pending, pairs.into_iter().map(|(_, value)| value)),
        Node::Unary(_, value)
        | Node::Member(value, _)
        | Node::SafeMember(value, _)
        | Node::Scope(value, _, None) => expr(value.into_inner()),
        Node::Binary(_, left, right) => {
            expr(left.into_inner());
            expr(right.into_inner());
        }
        Node::Range(start, end, _) => {
            start
                .into_iter()
                .chain(end)
                .for_each(|e| expr(e.into_inner()));
        }
        Node::Conditional(branches, alternate) => {
            expr(alternate.into_inner());
            exprs(
                pending,
                branches
                    .into_iter()
                    .flat_map(|(condition, result)| [condition, result]),
            );
        }
        Node::Case(subject, whens, alternate) => {
            subject
                .into_iter()
                .chain(alternate)
                .for_each(|e| expr(e.into_inner()));
            exprs(
                pending,
                whens.into_iter().flat_map(|when| {
                    when.values
                        .into_iter()
                        .map(|(value, _)| value)
                        .chain(std::iter::once(when.result))
                }),
            );
        }
        Node::Compound(stmt) => pending.push(Part::Stmt(stmt.into_inner())),
        Node::Call(_, arguments, _) => {
            exprs(
                pending,
                arguments.into_iter().map(|argument| argument.value),
            );
        }
        Node::ComputedCall(receiver, arguments)
        | Node::Scope(receiver, _, Some(arguments))
        | Node::Method(receiver, _, arguments, _)
        | Node::SafeMethod(receiver, _, arguments, _) => {
            expr(receiver.into_inner());
            exprs(
                pending,
                arguments.into_iter().map(|argument| argument.value),
            );
        }
        Node::BlockCall(receiver, block) => {
            expr(receiver.into_inner());
            attached(block, pending);
        }
        Node::Index(receiver, indices) => {
            expr(receiver.into_inner());
            exprs(pending, indices.into_iter());
        }
    }
}

fn statement(statement: Statement, pending: &mut Vec<Part>) {
    match statement {
        Statement::Retry
        | Statement::Module(_)
        | Statement::UnboundClass(_)
        | Statement::Unsupported => {}
        Statement::Raise(value, message) => pending.extend(
            value
                .into_iter()
                .chain(message)
                .map(|e| Part::Expr(e.into_inner())),
        ),
        Statement::Expr(expr) => pending.push(Part::Expr(expr)),
        Statement::Assign(target, _, value) => {
            pending.push(Part::Target(target));
            pending.push(Part::Expr(value));
        }
        Statement::If(branches, alternate, _) => {
            stmts(pending, alternate.into_iter());
            for (condition, body) in branches {
                pending.push(Part::Expr(condition));
                stmts(pending, body.into_iter());
            }
        }
        Statement::While(condition, body, _) => {
            pending.push(Part::Expr(condition));
            stmts(pending, body.into_iter());
        }
        Statement::For(target, values, body) => {
            pending.push(Part::Target(target));
            pending.push(Part::Expr(values));
            stmts(pending, body.into_iter());
        }
        Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
            pending.extend(value.map(Part::Expr));
        }
    }
}

fn attempted(attempt: Try, pending: &mut Vec<Part>) {
    let rescues = attempt.rescues.into_iter().flat_map(|rescue| rescue.body);
    stmts(
        pending,
        attempt
            .body
            .into_iter()
            .chain(attempt.alternate)
            .chain(attempt.ensure)
            .chain(rescues),
    );
}

fn attached(block: Block, pending: &mut Vec<Part>) {
    pending.push(Part::Targets(Box::new(block.params.into_iter())));
    stmts(pending, block.body.into_iter());
}
