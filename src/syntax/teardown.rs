//! Drops syntax trees without native recursion.
//!
//! Trees nest as deeply as the syntax limit. The compiler's generated drop
//! glue recurses through several frames per level, which can exhaust a small
//! native stack such as a WebAssembly host's. Dropping an expression or a
//! statement instead detaches its children onto a heap stack and drops them
//! one at a time, so each nested drop finds an empty node. A node's list of
//! children, such as an array literal's elements or a body's statements,
//! goes on the stack as the list's own iterator, which yields them in turn,
//! so the stack grows with the tree's depth and never holds a copy of a wide
//! list. One stack serves every drop on a thread, so dropping a tree, even
//! after a compilation's budget has run out, allocates only while that stack
//! first grows, or grows past what a thread keeps.

use super::{Argument, Block, Expr, Node, Rescue, Statement, Stmt, Target, Try, When};
use crate::compilation::{Buffer, Bytes, IntoIter};
use std::cell::RefCell;

enum Part {
    Expr(Expr),
    Stmt(Stmt),
    Target(Target),
    Exprs(IntoIter<Expr>),
    Arguments(IntoIter<Argument>),
    Pairs(IntoIter<(Bytes, Expr)>),
    Branches(IntoIter<(Expr, Expr)>),
    Whens(IntoIter<When>),
    Values(IntoIter<(Expr, bool)>),
    Stmts(IntoIter<Stmt>),
    Clauses(IntoIter<(Expr, Buffer<Stmt>)>),
    Rescues(IntoIter<Rescue>),
    Targets(IntoIter<Target>),
    Parts(IntoIter<(Option<Target>, bool)>),
}

impl Part {
    /// Whether this is a list with nothing left in it.
    fn exhausted(&self) -> bool {
        match self {
            Part::Expr(_) | Part::Stmt(_) | Part::Target(_) => false,
            Part::Exprs(rest) => rest.len() == 0,
            Part::Arguments(rest) => rest.len() == 0,
            Part::Pairs(rest) => rest.len() == 0,
            Part::Branches(rest) => rest.len() == 0,
            Part::Whens(rest) => rest.len() == 0,
            Part::Values(rest) => rest.len() == 0,
            Part::Stmts(rest) => rest.len() == 0,
            Part::Clauses(rest) => rest.len() == 0,
            Part::Rescues(rest) => rest.len() == 0,
            Part::Targets(rest) => rest.len() == 0,
            Part::Parts(rest) => rest.len() == 0,
        }
    }
}

/// Puts `part` on the stack unless it is a list with nothing left, so a
/// node's last child takes its place and a chain of single children, however
/// deep, keeps the stack short.
fn push(pending: &mut Vec<Part>, part: Part) {
    if !part.exhausted() {
        pending.push(part);
    }
}

thread_local! {
    /// The stack drops on this thread share, kept with its capacity between
    /// them.
    static PENDING: RefCell<Vec<Part>> = const { RefCell::new(Vec::new()) };
}

/// Drops what `start` puts on the stack, and what those put there in turn.
fn teardown(start: impl FnOnce(&mut Vec<Part>)) {
    // A drop inside this one finds the shared stack taken and an empty one
    // in its place, which only an emptied node reaches, so it pushes nothing.
    let mut pending = PENDING
        .try_with(|shared| std::mem::take(&mut *shared.borrow_mut()))
        .unwrap_or_default();
    start(&mut pending);
    drain(&mut pending);
    // A stack a very deep tree grew is kept only as large as a thread keeps
    // one.
    if pending.capacity() > KEPT {
        pending.shrink_to(KEPT);
    }
    let _ = PENDING.try_with(|shared| {
        let mut shared = shared.borrow_mut();
        if shared.capacity() < pending.capacity() {
            *shared = pending;
        }
    });
}

/// The most entries of the shared stack a thread keeps between drops.
const KEPT: usize = 256;

impl Drop for Expr {
    fn drop(&mut self) {
        if matches!(
            self.node,
            Node::Integer(_) | Node::Literal(_) | Node::Var(_) | Node::Regex(..)
        ) {
            return;
        }
        let taken = std::mem::replace(&mut self.node, Node::Integer(0));
        teardown(|pending| node(taken, pending));
    }
}

impl Drop for Stmt {
    fn drop(&mut self) {
        if matches!(
            self.node,
            Statement::Retry | Statement::Unsupported | Statement::Module(_)
        ) {
            return;
        }
        let taken = std::mem::replace(&mut self.node, Statement::Retry);
        teardown(|pending| statement(taken, pending));
    }
}

/// Drops the next item of `rest`, putting `rest` back first when it has one.
macro_rules! next {
    ($pending:ident, $variant:ident, $rest:ident, |$item:pat_param| $push:expr) => {{
        let mut rest = $rest;
        if let Some($item) = rest.next() {
            push($pending, Part::$variant(rest));
            $push;
        }
    }};
}

fn drain(pending: &mut Vec<Part>) {
    while let Some(part) = pending.pop() {
        match part {
            Part::Expr(mut expr) => {
                node(std::mem::replace(&mut expr.node, Node::Integer(0)), pending)
            }
            Part::Stmt(mut stmt) => {
                statement(std::mem::replace(&mut stmt.node, Statement::Retry), pending)
            }
            Part::Target(target) => match target {
                Target::Value(expr) => pending.push(Part::Expr(expr)),
                Target::Tuple(parts) => push(pending, Part::Parts(parts.into_iter())),
                Target::Typed(target, _) => pending.push(Part::Target(target.into_inner())),
            },
            Part::Exprs(rest) => next!(pending, Exprs, rest, |expr| pending.push(Part::Expr(expr))),
            Part::Arguments(rest) => next!(pending, Arguments, rest, |argument| {
                pending.push(Part::Expr(argument.value))
            }),
            Part::Pairs(rest) => next!(pending, Pairs, rest, |(_, value)| {
                pending.push(Part::Expr(value))
            }),
            Part::Branches(rest) => next!(pending, Branches, rest, |(condition, result)| {
                pending.push(Part::Expr(condition));
                pending.push(Part::Expr(result));
            }),
            Part::Whens(rest) => next!(pending, Whens, rest, |when| {
                push(pending, Part::Values(when.values.into_iter()));
                pending.push(Part::Expr(when.result));
            }),
            Part::Values(rest) => next!(pending, Values, rest, |(value, _)| {
                pending.push(Part::Expr(value))
            }),
            Part::Stmts(rest) => next!(pending, Stmts, rest, |stmt| pending.push(Part::Stmt(stmt))),
            // A body goes below its condition, so the condition, which rarely
            // nests, is dropped first and the body takes the level's place.
            Part::Clauses(rest) => next!(pending, Clauses, rest, |(condition, body)| {
                push(pending, Part::Stmts(body.into_iter()));
                pending.push(Part::Expr(condition));
            }),
            Part::Rescues(rest) => next!(pending, Rescues, rest, |rescue| {
                push(pending, Part::Stmts(rescue.body.into_iter()))
            }),
            Part::Targets(rest) => next!(pending, Targets, rest, |target| {
                pending.push(Part::Target(target))
            }),
            Part::Parts(rest) => next!(pending, Parts, rest, |(target, _)| {
                pending.extend(target.map(Part::Target))
            }),
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
            push(pending, Part::Exprs(values.into_iter()));
        }
        Node::Hash(pairs) => push(pending, Part::Pairs(pairs.into_iter())),
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
            push(pending, Part::Branches(branches.into_iter()));
        }
        Node::Case(subject, whens, alternate) => {
            subject
                .into_iter()
                .chain(alternate)
                .for_each(|e| expr(e.into_inner()));
            push(pending, Part::Whens(whens.into_iter()));
        }
        Node::Compound(stmt) => pending.push(Part::Stmt(stmt.into_inner())),
        Node::Call(_, arguments, _) => push(pending, Part::Arguments(arguments.into_iter())),
        Node::ComputedCall(receiver, arguments)
        | Node::Scope(receiver, _, Some(arguments))
        | Node::Method(receiver, _, arguments, _)
        | Node::SafeMethod(receiver, _, arguments, _) => {
            expr(receiver.into_inner());
            push(pending, Part::Arguments(arguments.into_iter()));
        }
        Node::BlockCall(receiver, block) => {
            expr(receiver.into_inner());
            attached(block, pending);
        }
        Node::Index(receiver, indices) => {
            expr(receiver.into_inner());
            push(pending, Part::Exprs(indices.into_iter()));
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
            push(pending, Part::Stmts(alternate.into_iter()));
            push(pending, Part::Clauses(branches.into_iter()));
        }
        Statement::While(condition, body, _) => {
            push(pending, Part::Stmts(body.into_iter()));
            pending.push(Part::Expr(condition));
        }
        Statement::For(target, values, body) => {
            push(pending, Part::Stmts(body.into_iter()));
            pending.push(Part::Target(target));
            pending.push(Part::Expr(values));
        }
        Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
            pending.extend(value.map(Part::Expr));
        }
    }
}

fn attempted(attempt: Try, pending: &mut Vec<Part>) {
    push(pending, Part::Stmts(attempt.body.into_iter()));
    push(pending, Part::Stmts(attempt.alternate.into_iter()));
    push(pending, Part::Stmts(attempt.ensure.into_iter()));
    push(pending, Part::Rescues(attempt.rescues.into_iter()));
}

fn attached(block: Block, pending: &mut Vec<Part>) {
    push(pending, Part::Targets(block.params.into_iter()));
    push(pending, Part::Stmts(block.body.into_iter()));
}
