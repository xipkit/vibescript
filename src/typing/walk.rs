//! A walk over syntax for the checker's own questions about a body, such as
//! which names it mentions or whether it yields.
//!
//! The walk keeps what it has yet to visit on one stack, taking each list's
//! elements one at a time, so the stack is as deep as the syntax is nested
//! however wide its lists are. It charges the check's meter a step for each
//! statement, expression and assignment target it visits, checks the
//! budget every [`PACE`] of them, and stops with the check.

use super::meter::Meter;
use crate::{
    compilation::{Buffer, Bytes},
    syntax::{Argument, Expr, Node, Rescue, Statement, Stmt, Target, When},
};
use std::slice::Iter;

/// A walk charges the meter, and checks the budget, once per this many
/// visits.
const PACE: u64 = 64;

/// A statement, expression or assignment target a walk visits.
#[derive(Clone, Copy)]
pub(super) enum Item<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
    Target(&'a Target),
}

/// What a walk has yet to visit: one item, or what is left of a list.
pub(super) enum Next<'a> {
    Item(Item<'a>),
    Stmts(Iter<'a, Stmt>),
    Exprs(Iter<'a, Expr>),
    /// Arguments' values.
    Arguments(Iter<'a, Argument>),
    /// A hash literal's values.
    Pairs(Iter<'a, (Bytes, Expr)>),
    /// A conditional's conditions and results.
    Branches(Iter<'a, (Expr, Expr)>),
    /// An `if`'s conditions and bodies.
    Clauses(Iter<'a, (Expr, Buffer<Stmt>)>),
    /// A `case`'s `when`s: each one's values and result.
    Whens(Iter<'a, When>),
    /// A `case`'s results, without the values the `when`s name.
    Results(Iter<'a, When>),
    Values(Iter<'a, (Expr, bool)>),
    /// A `begin`'s rescues' bodies.
    Rescues(Iter<'a, Rescue>),
    /// A destructuring's parts.
    Parts(Iter<'a, (Option<Target>, bool)>),
}

impl Next<'_> {
    /// Whether nothing is left to take, once something has been: a single
    /// item is taken whole.
    fn exhausted(&self) -> bool {
        matches!(self, Next::Item(_)) || self.empty()
    }

    /// Whether a list is empty.
    fn empty(&self) -> bool {
        match self {
            Next::Item(_) => false,
            Next::Stmts(items) => items.len() == 0,
            Next::Exprs(items) => items.len() == 0,
            Next::Arguments(items) => items.len() == 0,
            Next::Pairs(items) => items.len() == 0,
            Next::Branches(items) => items.len() == 0,
            Next::Clauses(items) => items.len() == 0,
            Next::Whens(items) | Next::Results(items) => items.len() == 0,
            Next::Values(items) => items.len() == 0,
            Next::Rescues(items) => items.len() == 0,
            Next::Parts(items) => items.len() == 0,
        }
    }
}

/// A walk, whose entries each carry a tag of the walker's, such as whether
/// they stand inside a loop, which the items taken from them carry too.
pub(super) struct Walk<'a, 'm, T: Copy = ()> {
    stack: Vec<(Next<'a>, T)>,
    meter: &'m Meter,
    /// The items visited since the meter was last charged, which it is
    /// charged when the walk ends.
    visited: u64,
    /// Whether the check has stopped, so the walk visits no more.
    stopped: bool,
}

impl<'a, 'm, T: Copy> Walk<'a, 'm, T> {
    /// A walk charged to `meter`, which visits nothing if the check has
    /// stopped already.
    pub fn new(meter: &'m Meter) -> Self {
        Self {
            stack: Vec::new(),
            meter,
            visited: 0,
            stopped: meter.stopped(),
        }
    }

    /// Adds `next`, whose items carry `tag`, to what the walk visits.
    pub fn push(&mut self, next: Next<'a>, tag: T) {
        if !self.stopped && !next.empty() {
            self.stack.push((next, tag));
        }
    }

    /// Adds a body's statements.
    pub fn stmts(&mut self, body: &'a [Stmt], tag: T) {
        self.push(Next::Stmts(body.iter()), tag);
    }

    /// Adds an expression.
    pub fn expr(&mut self, expr: &'a Expr, tag: T) {
        self.push(Next::Item(Item::Expr(expr)), tag);
    }

    /// The bytes the stack takes.
    pub fn bytes(&self) -> usize {
        self.stack.capacity() * std::mem::size_of::<(Next<'_>, T)>()
    }

    /// The next item to visit, with its tag, or none once every one has
    /// been or the check has stopped. `held` is what the walker keeps
    /// beside the walk's own stack, which the budget bounds with it.
    pub fn next(&mut self, held: usize) -> Option<(Item<'a>, T)> {
        loop {
            if self.stopped {
                return None;
            }
            let (top, tag) = self.stack.last_mut()?;
            let tag = *tag;
            let (item, then) = match top {
                Next::Item(item) => (Some(*item), None),
                Next::Stmts(items) => (items.next().map(Item::Stmt), None),
                Next::Exprs(items) => (items.next().map(Item::Expr), None),
                Next::Arguments(items) => (items.next().map(|arg| Item::Expr(&arg.value)), None),
                Next::Pairs(items) => (items.next().map(|(_, value)| Item::Expr(value)), None),
                Next::Values(items) => (items.next().map(|(value, _)| Item::Expr(value)), None),
                Next::Branches(items) => match items.next() {
                    Some((condition, result)) => (
                        Some(Item::Expr(condition)),
                        Some(Next::Item(Item::Expr(result))),
                    ),
                    None => (None, None),
                },
                Next::Clauses(items) => match items.next() {
                    Some((condition, body)) => {
                        (Some(Item::Expr(condition)), Some(Next::Stmts(body.iter())))
                    }
                    None => (None, None),
                },
                Next::Whens(items) => match items.next() {
                    Some(when) => (
                        Some(Item::Expr(&when.result)),
                        Some(Next::Values(when.values.iter())),
                    ),
                    None => (None, None),
                },
                Next::Results(items) => (items.next().map(|when| Item::Expr(&when.result)), None),
                Next::Rescues(items) => (
                    None,
                    items.next().map(|rescue| Next::Stmts(rescue.body.iter())),
                ),
                Next::Parts(items) => (
                    items
                        .next()
                        .and_then(|(part, _)| part.as_ref())
                        .map(Item::Target),
                    None,
                ),
            };
            if top.exhausted() {
                self.stack.pop();
            }
            if let Some(then) = then {
                self.push(then, tag);
            }
            let Some(item) = item else {
                continue;
            };
            self.visited += 1;
            if self.visited == PACE {
                self.visited = 0;
                self.stopped = self.meter.pace(PACE, held + self.bytes());
                if self.stopped {
                    return None;
                }
            }
            return Some((item, tag));
        }
    }

    /// Adds every statement, expression and assignment target `item` holds,
    /// as a walk over all of a body's syntax visits them: those of nested
    /// blocks and `begin`s too, but not a block's parameters.
    pub fn children(&mut self, item: Item<'a>, tag: T) {
        match item {
            Item::Stmt(stmt) => match &stmt.node {
                Statement::Assign(target, _, value) => {
                    self.push(Next::Item(Item::Target(target)), tag);
                    self.expr(value, tag);
                }
                Statement::If(branches, alternate, _) => {
                    self.push(Next::Clauses(branches.iter()), tag);
                    self.stmts(alternate, tag);
                }
                Statement::While(condition, body, _) => {
                    self.expr(condition, tag);
                    self.stmts(body, tag);
                }
                Statement::For(target, iterable, body) => {
                    self.push(Next::Item(Item::Target(target)), tag);
                    self.expr(iterable, tag);
                    self.stmts(body, tag);
                }
                Statement::Expr(expr)
                | Statement::Return(Some(expr))
                | Statement::Break(Some(expr))
                | Statement::Next(Some(expr)) => self.expr(expr, tag),
                Statement::Raise(value, message) => {
                    for expr in value.iter().chain(message) {
                        self.expr(expr, tag);
                    }
                }
                _ => (),
            },
            Item::Expr(expr) => match &expr.node {
                Node::Try(attempt) => {
                    self.stmts(&attempt.body, tag);
                    self.push(Next::Rescues(attempt.rescues.iter()), tag);
                    self.stmts(&attempt.alternate, tag);
                    self.stmts(&attempt.ensure, tag);
                }
                Node::Compound(stmt) => self.push(Next::Item(Item::Stmt(stmt)), tag),
                Node::BlockCall(call, block) => {
                    self.expr(call, tag);
                    self.stmts(&block.body, tag);
                }
                Node::Shape(_, Some(fallback), _) => self.expr(fallback, tag),
                Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
                    self.push(Next::Exprs(values.iter()), tag);
                }
                Node::Hash(entries) => self.push(Next::Pairs(entries.iter()), tag),
                Node::Unary(_, value) => self.expr(value, tag),
                Node::Binary(_, left, right) => {
                    self.expr(left, tag);
                    self.expr(right, tag);
                }
                Node::Range(start, end, _) => {
                    for expr in start.iter().chain(end) {
                        self.expr(expr, tag);
                    }
                }
                Node::Conditional(branches, alternate) => {
                    self.push(Next::Branches(branches.iter()), tag);
                    self.expr(alternate, tag);
                }
                Node::Case(subject, whens, alternate) => {
                    for expr in subject.iter().chain(alternate) {
                        self.expr(expr, tag);
                    }
                    self.push(Next::Whens(whens.iter()), tag);
                }
                Node::Call(_, args, _) => self.push(Next::Arguments(args.iter()), tag),
                Node::ComputedCall(receiver, args)
                | Node::Method(receiver, _, args, _)
                | Node::SafeMethod(receiver, _, args, _) => {
                    self.expr(receiver, tag);
                    self.push(Next::Arguments(args.iter()), tag);
                }
                Node::Scope(receiver, _, args) => {
                    self.expr(receiver, tag);
                    if let Some(args) = args {
                        self.push(Next::Arguments(args.iter()), tag);
                    }
                }
                Node::Member(receiver, _) | Node::SafeMember(receiver, _) => {
                    self.expr(receiver, tag);
                }
                Node::Index(receiver, selectors) => {
                    self.expr(receiver, tag);
                    self.push(Next::Exprs(selectors.iter()), tag);
                }
                Node::Regex(..)
                | Node::Shape(_, None, _)
                | Node::Integer(_)
                | Node::BigInteger(..)
                | Node::Literal(_)
                | Node::Var(_) => (),
            },
            Item::Target(target) => match target {
                Target::Value(expr) => self.expr(expr, tag),
                Target::Typed(inner, _) => self.push(Next::Item(Item::Target(inner)), tag),
                Target::Tuple(parts) => self.push(Next::Parts(parts.iter()), tag),
            },
        }
    }
}

impl<T: Copy> Drop for Walk<'_, '_, T> {
    /// Charges the items visited since the meter was last charged.
    fn drop(&mut self) {
        self.meter.charge(self.visited);
    }
}

#[cfg(test)]
mod tests {
    use super::{PACE, Walk};
    use crate::{CancellationToken, compilation::Budget, typing::meter::Meter};

    /// Parses `source` and walks all of its top-level statements with
    /// `budget`: how many items it visited, the bytes its stack took and
    /// the steps it charged.
    fn walked(source: &str, budget: Budget) -> (u64, usize, u64) {
        let parsed = crate::syntax::parse(source, &()).unwrap();
        let meter = Meter::new(budget, None);
        let mut visited = 0;
        let bytes = {
            let mut walk = Walk::new(&meter);
            walk.stmts(&parsed.functions[0].body, ());
            while let Some((item, ())) = walk.next(0) {
                visited += 1;
                walk.children(item, ());
            }
            walk.bytes()
        };
        (visited, bytes, meter.steps())
    }

    fn wide(count: usize) -> String {
        let items = vec!["1"; count].join(", ");
        format!("x = [{items}]\ny = {{a: [{items}], b: f({items})}}\n")
    }

    #[test]
    fn a_wide_list_takes_one_entry_and_a_step_for_each_item() {
        let (visited, bytes, steps) = walked(&wide(10_000), Budget::default());
        // Each statement, its target and the variable it names; the first
        // array and its elements; the hash, its array and its elements, and
        // the call and its arguments.
        assert_eq!(
            visited,
            2 * 3 + (1 + 10_000) + 1 + (1 + 10_000) + (1 + 10_000)
        );
        assert_eq!(steps, visited);
        let entry = std::mem::size_of::<(super::Next<'_>, ())>();
        assert!(bytes <= 8 * entry, "{bytes} bytes for entries of {entry}");
    }

    #[test]
    fn a_walk_stops_within_its_pace_of_the_budget() {
        let source = wide(100_000);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        for budget in [
            Budget {
                steps: Some(1_000),
                ..Budget::default()
            },
            Budget {
                cancellation: Some(cancelled),
                ..Budget::default()
            },
            Budget {
                deadline: Some(std::time::Instant::now()),
                ..Budget::default()
            },
        ] {
            let steps = budget.steps.unwrap_or(0);
            let (visited, _, _) = walked(&source, budget);
            assert!(visited <= steps + PACE, "{visited} items visited");
        }
    }

    #[test]
    fn a_walk_visits_nothing_once_the_check_has_stopped() {
        let meter = Meter::new(Budget::default(), None);
        meter.stop();
        let parsed = crate::syntax::parse(&wide(10), &()).unwrap();
        let mut walk: Walk<'_, '_> = Walk::new(&meter);
        walk.stmts(&parsed.functions[0].body, ());
        assert!(walk.next(0).is_none());
        assert_eq!(walk.bytes(), 0);
    }
}
